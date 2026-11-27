//! FTP control/data connections; publish completed files with RNFR/RNTO.
use super::{
    wire::{self, Endpoint, Wire, MAX_FILE},
    Fault,
};
pub(super) struct Ftp {
    control: Wire,
    endpoint: Endpoint,
    protected: bool,
}
pub(super) fn path(value: &str) -> Result<String, Fault> {
    wire::clean(value)?;
    let value = value.replace('\\', "/");
    if value.split('/').any(|p| p == "..") || value.contains(['*', '?']) {
        return Err("Недопустимый путь FTP".into());
    }
    Ok(format!("/{}", value.trim_matches('/')))
}
impl Ftp {
    pub fn connect(server: &str, user: &str, password: &str) -> Result<Self, Fault> {
        let endpoint = Endpoint::parse(server, "ftp", 21, 990)?;
        let mut control = Wire::connect(&endpoint)?;
        control.expect(&[220])?;
        if endpoint.starttls {
            control.command("AUTH TLS")?;
            control.expect(&[234])?;
            control.tls(&endpoint.host)?;
        }
        control.command(&format!("USER {}", wire::clean(user)?))?;
        let (code, _) = control.reply()?;
        if code == 331 {
            control.command(&format!("PASS {}", wire::clean(password)?))?;
            control.expect(&[230])?;
        } else if code != 230 {
            return Err(format!("FTP: отказ авторизации ({code})").into());
        }
        let protected = endpoint.tls || endpoint.starttls;
        if protected {
            control.command("PBSZ 0")?;
            control.expect(&[200])?;
            control.command("PROT P")?;
            control.expect(&[200])?;
        }
        control.command("TYPE I")?;
        control.expect(&[200])?;
        Ok(Self {
            control,
            endpoint,
            protected,
        })
    }
    fn passive(&mut self) -> Result<Endpoint, Fault> {
        self.control.reset_idle();
        self.control.command("EPSV")?;
        let (code, text) = self.control.reply()?;
        let port = if code == 229 {
            let value = text
                .split_once('(')
                .and_then(|(_, s)| s.split_once(')'))
                .ok_or("FTP: неверный EPSV")?
                .0;
            let separator = value.chars().next().ok_or("FTP: пустой EPSV")?;
            value
                .split(separator)
                .nth(3)
                .ok_or("FTP: неверный EPSV порт")?
                .parse::<u16>()
                .map_err(|_| "FTP: неверный порт")?
        } else {
            self.control.command("PASV")?;
            let text = self.control.expect(&[227])?;
            let value = text
                .split_once('(')
                .and_then(|(_, s)| s.split_once(')'))
                .ok_or("FTP: неверный PASV")?
                .0;
            let numbers = value
                .split(',')
                .map(str::parse::<u8>)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| "FTP: неверный PASV адрес")?;
            if numbers.len() != 6 {
                return Err("FTP: неверный PASV адрес".into());
            }
            u16::from(numbers[4]) * 256 + u16::from(numbers[5])
        };
        if port == 0 {
            return Err("FTP: нулевой пассивный порт".into());
        }
        // Ignore the PASV advertised IP: a data connection must use the same
        // peer as the control channel, including servers behind NAT.
        Ok(Endpoint {
            host: self.control.peer_ip()?,
            port,
            tls: false,
            starttls: false,
        })
    }
    fn data(&mut self, command: &str, optional: bool) -> Result<Option<Wire>, Fault> {
        let endpoint = self.passive()?;
        let mut data = Wire::connect(&endpoint)?;
        self.control.command(command)?;
        let (code, _) = self.control.reply()?;
        if optional && matches!(code, 500 | 501 | 502 | 504) {
            return Ok(None);
        }
        if !matches!(code, 125 | 150) {
            return Err(format!("FTP: передача отклонена ({code})").into());
        }
        if self.protected {
            data.tls_from(&self.control, &self.endpoint.host)?;
        }
        Ok(Some(data))
    }
    fn read_data(&mut self, command: &str, limit: usize) -> Result<Vec<u8>, Fault> {
        self.read_optional_data(command, limit, false)?
            .ok_or_else(|| "FTP: нет канала данных".into())
    }
    fn read_optional_data(
        &mut self,
        command: &str,
        limit: usize,
        optional: bool,
    ) -> Result<Option<Vec<u8>>, Fault> {
        let Some(mut data) = self.data(command, optional)? else {
            return Ok(None);
        };
        let bytes = data.bytes(limit)?;
        data.finish()?;
        drop(data);
        self.control.expect(&[226, 250])?;
        Ok(Some(bytes))
    }
    pub fn list(&mut self, folder: &str) -> Result<Vec<(String, bool)>, Fault> {
        let folder = path(folder)?;
        self.control.command(&format!("CWD {folder}"))?;
        self.control.expect(&[250])?;
        // MLSD is unambiguous. Older Windows/Unix servers may only offer LIST.
        let bytes = match self.read_optional_data("MLSD", 4 * 1024 * 1024, true)? {
            Some(b) => b,
            // Only a command rejection permits fallback. TLS/read failures
            // must not be mistaken for lack of MLSD support or hide errors.
            None => {
                return parse_list(&String::from_utf8_lossy(
                    &self.read_data("LIST", 4 * 1024 * 1024)?,
                ))
            }
        };
        let mut out = Vec::new();
        for line in String::from_utf8_lossy(&bytes).lines() {
            let Some((facts, name)) = line.split_once(' ') else {
                continue;
            };
            let facts = facts.to_ascii_lowercase();
            if facts.contains("type=cdir;") || facts.contains("type=pdir;") {
                continue;
            }
            if !(facts.contains("type=file;") || facts.contains("type=dir;")) {
                return Err("FTP: неизвестный тип записи MLSD".into());
            }
            validate_name(name)?;
            out.push((name.into(), facts.contains("type=dir;")));
        }
        Ok(out)
    }
    pub fn get(&mut self, name: &str) -> Result<Vec<u8>, Fault> {
        self.read_data(&format!("RETR {}", path(name)?), MAX_FILE)
    }
    pub fn delete(&mut self, name: &str) -> Result<(), Fault> {
        self.control.reset_idle();
        self.control.command(&format!("DELE {}", path(name)?))?;
        self.control.expect(&[250])?;
        Ok(())
    }
    pub fn put(&mut self, folder: &str, name: &str, bytes: &[u8]) -> Result<(), Fault> {
        validate_name(name)?;
        let folder = path(folder)?;
        if self.list(&folder)?.iter().any(|(n, _)| n == name) {
            return Err("FTP: файл назначения уже существует".into());
        }
        let temporary = format!(
            "{folder}/.codex-{}-{}.part",
            std::process::id(),
            super::SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let target = format!("{folder}/{name}");
        let result = (|| -> Result<(), Fault> {
            let mut data = self
                .data(&format!("STOR {temporary}"), false)?
                .ok_or("FTP: нет канала данных")?;
            data.send(bytes)?;
            data.finish()?;
            drop(data);
            self.control.expect(&[226, 250])?;
            self.control.command(&format!("SIZE {temporary}"))?;
            let size = self.control.expect(&[213])?;
            if size[4..].trim().parse::<usize>().ok() != Some(bytes.len()) {
                return Err("FTP: неполная загрузка пакета".into());
            }
            if self.list(&folder)?.iter().any(|(n, _)| n == name) {
                return Err("FTP: имя пакета занято другим отправителем".into());
            }
            self.control.command(&format!("RNFR {temporary}"))?;
            self.control.expect(&[350])?;
            self.control.command(&format!("RNTO {target}"))?;
            self.control.expect(&[250])?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.delete(&temporary);
        }
        result
    }
    pub fn quit(&mut self) -> Result<(), Fault> {
        self.control.reset_idle();
        self.control.command("QUIT")?;
        self.control.expect(&[221])?;
        Ok(())
    }
}
fn validate_name(name: &str) -> Result<(), Fault> {
    wire::clean(name)?;
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\\']) {
        return Err("FTP: некорректное имя в списке файлов".into());
    }
    Ok(())
}
fn parse_list(text: &str) -> Result<Vec<(String, bool)>, Fault> {
    let unix = regex::Regex::new(
        r"^([d-])[rwxstST-]{9}\s+\d+\s+\S+\s+\S+\s+\d+\s+\S+\s+\d+\s+\S+\s+(.+)$",
    )
    .unwrap();
    let dos =
        regex::Regex::new(r"^\d{2}-\d{2}-\d{2,4}\s+\d{2}:\d{2}(?:AM|PM)\s+(<DIR>|\d+)\s+(.+)$")
            .unwrap();
    let mut result = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with("total "))
    {
        let (name, dir) = if let Some(c) = unix.captures(line) {
            (c[2].to_owned(), &c[1] == "d")
        } else if let Some(c) = dos.captures(line) {
            (c[2].to_owned(), &c[1] == "<DIR>")
        } else {
            return Err("FTP: неподдерживаемый формат LIST".into());
        };
        if matches!(name.as_str(), "." | "..") {
            continue;
        }
        validate_name(&name)?;
        result.push((name, dir));
    }
    Ok(result)
}
