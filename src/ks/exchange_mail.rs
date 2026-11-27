use super::{
    wire::{self, Endpoint, Wire, MAX_FILE},
    Fault,
};
use mailparse::MailHeaderMap;
const MARKER: &str = "SENDDOCSIDENTIFICATIONLABEL";
#[derive(Clone)]
pub(super) struct Settings {
    pub smtp: String,
    pub pop: String,
    pub from: String,
    pub user: String,
    pub password: String,
}
pub(super) fn address(s: &str) -> Result<&str, Fault> {
    wire::clean(s)?;
    if s.is_empty() || !s.contains('@') || s.contains(['<', '>', ' ', '\t']) || !s.is_ascii() {
        return Err("Ожидается почтовый адрес вида user@example.org".into());
    }
    Ok(s)
}
pub(super) fn send(
    settings: &Settings,
    to: &str,
    subject: &str,
    name: &str,
    bytes: &[u8],
) -> Result<(), Fault> {
    address(&settings.from)?;
    address(to)?;
    wire::clean(subject)?;
    if !super::packet_name(name) {
        return Err("Недопустимое имя почтового вложения".into());
    }
    if bytes.len().saturating_mul(4) / 3 + bytes.len() / 57 * 2 + 8192 > MAX_FILE {
        return Err(
            "Почтовое вложение слишком велико; включите разбиение 7z на части в настройках обмена"
                .into(),
        );
    }
    let endpoint = Endpoint::parse(&settings.smtp, "smtp", 25, 465)?;
    let mut stream = Wire::connect(&endpoint)?;
    stream.expect(&[220])?;
    stream.command("EHLO codex-ais")?;
    let (code, mut capabilities) = stream.reply()?;
    if code != 250 {
        stream.command("HELO codex-ais")?;
        stream.expect(&[250])?;
        capabilities.clear();
    }
    let upgrade = endpoint.starttls
        || (!endpoint.tls && capabilities.to_ascii_uppercase().contains("STARTTLS"));
    if upgrade {
        stream.command("STARTTLS")?;
        stream.expect(&[220])?;
        stream.tls(&endpoint.host)?;
        stream.command("EHLO codex-ais")?;
        capabilities = stream.expect(&[250])?;
    }
    let caps = capabilities.to_ascii_uppercase();
    // Legacy Mail uses the account for POP3; use it for SMTP only when AUTH
    // is advertised. Plain relays remain compatible with the original.
    if !settings.user.is_empty()
        && caps
            .lines()
            .any(|s| s.get(4..).is_some_and(|s| s.starts_with("AUTH")))
    {
        if caps.contains("PLAIN") {
            let auth = super::super::base64::encode_bytes(
                format!("\0{}\0{}", settings.user, settings.password).as_bytes(),
            )?;
            stream.command(&format!("AUTH PLAIN {auth}"))?;
            let (code, _) = stream.reply()?;
            if code == 334 {
                stream.command(&auth)?;
                stream.expect(&[235])?;
            } else if code != 235 {
                return Err(format!("SMTP: отказ авторизации ({code})").into());
            }
        } else if caps.contains("LOGIN") {
            stream.command("AUTH LOGIN")?;
            stream.expect(&[334])?;
            stream.command(&super::super::base64::encode_bytes(
                settings.user.as_bytes(),
            )?)?;
            stream.expect(&[334])?;
            stream.command(&super::super::base64::encode_bytes(
                settings.password.as_bytes(),
            )?)?;
            stream.expect(&[235])?;
        } else {
            return Err("SMTP: сервер не поддерживает AUTH PLAIN/LOGIN".into());
        }
    }
    stream.command(&format!("MAIL FROM:<{}>", settings.from))?;
    stream.expect(&[250])?;
    stream.command(&format!("RCPT TO:<{to}>"))?;
    stream.expect(&[250, 251])?;
    stream.command("DATA")?;
    stream.expect(&[354])?;
    let boundary = format!(
        "codex-ais-{}-{}",
        std::process::id(),
        super::SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let date = crate::ks::date::Date::now()?.gmt_string()?;
    stream.send(format!("Date: {date}\r\nMessage-ID: <{boundary}@codex-ais>\r\n").as_bytes())?;
    stream.send(format!("From: {}\r\nTo: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\nContent-Type: text/plain; charset=us-ascii\r\n\r\nAIS court document exchange\r\n *** ID = {MARKER} ***\r\n--{boundary}\r\nContent-Type: application/octet-stream; name=\"{name}\"\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: base64\r\n\r\n",settings.from).as_bytes())?;
    // Encode in complete 57-byte lines: no second full-sized copy of the packet.
    for chunk in bytes.chunks(57 * 256) {
        let encoded = super::super::base64::encode_bytes(chunk)?;
        for line in encoded.as_bytes().chunks(76) {
            stream.send(line)?;
            stream.send(b"\r\n")?;
        }
    }
    stream.send(format!("--{boundary}--\r\n.\r\n").as_bytes())?;
    stream.expect(&[250])?;
    // A successful DATA response means delivery was accepted. A lost QUIT
    // reply must not turn that into an automatic duplicate transmission.
    let _ = stream.command("QUIT");
    let _ = stream.expect(&[221]);
    Ok(())
}
pub(super) struct Pop {
    wire: Wire,
    pub messages: usize,
    acknowledged: bool,
}
impl Pop {
    pub fn connect(settings: &Settings) -> Result<Self, Fault> {
        let endpoint = Endpoint::parse(&settings.pop, "pop3", 110, 995)?;
        let mut result = Self {
            wire: Wire::connect(&endpoint)?,
            messages: 0,
            acknowledged: false,
        };
        result.ok()?;
        if endpoint.starttls {
            result.wire.command("STLS")?;
            result.ok()?;
            result.wire.tls(&endpoint.host)?;
        }
        result
            .wire
            .command(&format!("USER {}", wire::clean(&settings.user)?))?;
        result.ok()?;
        result
            .wire
            .command(&format!("PASS {}", wire::clean(&settings.password)?))?;
        result.ok()?;
        result.wire.command("STAT")?;
        let stat = result.ok()?;
        result.messages = stat
            .split_whitespace()
            .nth(1)
            .ok_or("POP3: неверный STAT")?
            .parse()
            .map_err(|_| "POP3: неверное число писем")?;
        if result.messages > 100000 {
            return Err("POP3: слишком много писем в ящике обмена".into());
        }
        Ok(result)
    }
    fn ok(&mut self) -> Result<String, Fault> {
        let line = String::from_utf8_lossy(&self.wire.line()?).into_owned();
        if !line.starts_with("+OK") {
            return Err("POP3: сервер отклонил команду".into());
        }
        Ok(line)
    }
    pub fn receive(&mut self, index: usize) -> Result<Vec<u8>, Fault> {
        self.wire.reset_idle();
        self.wire.command(&format!("RETR {index}"))?;
        self.ok()?;
        let mut data = Vec::new();
        loop {
            let line = self.wire.line()?;
            if line == b".\r\n" || line == b".\n" {
                break;
            }
            let line = if line.starts_with(b"..") {
                &line[1..]
            } else {
                &line
            };
            if data.len().saturating_add(line.len()) > MAX_FILE {
                return Err("POP3: письмо превышает 512 МиБ".into());
            }
            data.extend_from_slice(line);
        }
        Ok(data)
    }
    pub fn delete(&mut self, index: usize) -> Result<(), Fault> {
        self.wire.reset_idle();
        self.wire.command(&format!("DELE {index}"))?;
        self.ok()?;
        self.acknowledged = true;
        Ok(())
    }
    pub fn quit(&mut self) -> Result<(), Fault> {
        self.wire.reset_idle();
        self.wire.command("QUIT")?;
        self.ok()?;
        self.acknowledged = false;
        Ok(())
    }
}
pub(super) struct Message {
    pub id: String,
    pub index: usize,
    pub count: usize,
    pub files: Vec<(String, Vec<u8>)>,
}
pub(super) fn parse(bytes: &[u8]) -> Result<Option<Message>, Fault> {
    // mailparse descends recursively; cap the number of MIME headers before
    // invoking it, so hostile nesting cannot exhaust the executor stack.
    if bytes
        .windows(13)
        .filter(|w| w.eq_ignore_ascii_case(b"content-type:"))
        .take(129)
        .count()
        > 128
    {
        return Err("Слишком много MIME-частей в письме обмена".into());
    }
    let mail = mailparse::parse_mail(bytes).map_err(|e| format!("MIME: {e}"))?;
    let subject = mail.headers.get_first_value("Subject").unwrap_or_default();
    let mut marker = false;
    let mut files = Vec::new();
    fn parts(
        mail: &mailparse::ParsedMail<'_>,
        marker: &mut bool,
        files: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), Fault> {
        crate::execution::check()?;
        if mail.subparts.is_empty() {
            let disposition = mail.get_content_disposition();
            let name = disposition
                .params
                .get("filename")
                .or_else(|| mail.ctype.params.get("name"));
            if let Some(name) = name {
                if !super::packet_name(name) {
                    return Err(
                        "Письмо содержит вложение с недопустимым именем или расширением".into(),
                    );
                }
                files.push((
                    name.clone(),
                    mail.get_body_raw()
                        .map_err(|e| format!("MIME-вложение: {e}"))?,
                ));
            } else if mail.ctype.mimetype.starts_with("text/") {
                *marker |= mail
                    .get_body()
                    .map_err(|e| format!("MIME-текст: {e}"))?
                    .contains(MARKER);
            }
        } else {
            for part in &mail.subparts {
                parts(part, marker, files)?;
            }
        }
        Ok(())
    }
    // Unrelated mail is never removed. Look for the marker before validating
    // exchange-specific attachment names (other messages can contain anything).
    fn labelled(mail: &mailparse::ParsedMail<'_>) -> bool {
        (mail.ctype.mimetype.starts_with("text/")
            && mail.get_body().is_ok_and(|s| s.contains(MARKER)))
            || mail.subparts.iter().any(labelled)
    }
    if !labelled(&mail) {
        return Ok(None);
    }
    parts(&mail, &mut marker, &mut files)?;
    let values = subject.split('>').collect::<Vec<_>>();
    if values.len() != 3 || values[0].is_empty() || values[0].len() > 200 {
        return Err("Письмо обмена содержит неверный идентификатор/номер части".into());
    }
    let index = values[1]
        .parse::<usize>()
        .map_err(|_| "Неверный номер части письма")?;
    let count = values[2]
        .parse::<usize>()
        .map_err(|_| "Неверное число частей письма")?;
    if count == 0 || count > 10000 || index == 0 || index > count || files.is_empty() {
        return Err("Неверный комплект вложений обмена".into());
    }
    Ok(Some(Message {
        id: values[0].into(),
        index,
        count,
        files,
    }))
}
