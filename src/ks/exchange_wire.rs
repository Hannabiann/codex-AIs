//! Cancellable, bounded protocol I/O. Credentials never enter error messages.
use super::Fault;
use rustls::pki_types::{pem::PemObject, CertificateDer};
use std::{
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    sync::Arc,
    time::{Duration, Instant},
};

fn tls_config(ca_file: Option<&std::path::Path>) -> Result<Arc<rustls::ClientConfig>, Fault> {
    let mut roots =
        rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = ca_file {
        let mut pem = Vec::new();
        std::fs::File::open(path)
            .map_err(|e| format!("TLS CA: {e}"))?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut pem)
            .map_err(|e| format!("TLS CA: {e}"))?;
        if pem.len() > 1024 * 1024 {
            return Err("TLS CA: файл превышает 1 МиБ".into());
        }
        let mut count = 0;
        for cert in CertificateDer::pem_slice_iter(&pem) {
            let cert = cert.map_err(|e| format!("TLS CA: некорректный PEM: {e}"))?;
            roots
                .add(cert)
                .map_err(|e| format!("TLS CA: некорректный сертификат: {e}"))?;
            count += 1;
            if count > 256 {
                return Err("TLS CA: слишком много сертификатов".into());
            }
        }
        if count == 0 {
            return Err("TLS CA: в PEM нет сертификатов".into());
        }
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

pub(super) const MAX_FILE: usize = 512 * 1024 * 1024;
const IDLE: Duration = Duration::from_secs(60);
#[cfg(test)]
#[path = "exchange_tls_tests.rs"]
mod tests;
#[derive(Clone, Debug)]
pub(super) struct Endpoint {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub starttls: bool,
}
pub(super) fn clean(value: &str) -> Result<&str, Fault> {
    if value.contains(['\r', '\n', '\0']) {
        return Err("Недопустимый перевод строки в параметре обмена".into());
    }
    Ok(value)
}
impl Endpoint {
    pub fn parse(value: &str, protocol: &str, port: u16, tls_port: u16) -> Result<Self, Fault> {
        clean(value)?;
        let (scheme, address) = value
            .trim()
            .split_once("://")
            .unwrap_or((protocol, value.trim()));
        let tls = scheme == format!("{protocol}s");
        let starttls = scheme == format!("{protocol}+starttls");
        if !(scheme == protocol || tls || starttls)
            || address.contains(['/', '@', '?', '#', ' '])
            || address.is_empty()
        {
            return Err(format!("Некорректный адрес сервера {protocol}").into());
        }
        let default = if tls { tls_port } else { port };
        let (host, port) = if let Some(rest) = address.strip_prefix('[') {
            let (host, rest) = rest.split_once(']').ok_or("Некорректный IPv6 адрес")?;
            (
                host,
                if rest.is_empty() {
                    default
                } else {
                    rest.strip_prefix(':')
                        .ok_or("Некорректный порт")?
                        .parse()
                        .map_err(|_| "Некорректный порт")?
                },
            )
        } else if let Some((host, number)) = address.rsplit_once(':') {
            if host.contains(':') {
                return Err("IPv6 адрес должен быть в квадратных скобках".into());
            }
            (host, number.parse().map_err(|_| "Некорректный порт")?)
        } else {
            (address, default)
        };
        if host.is_empty() || port == 0 {
            return Err("Не задан сервер или порт обмена".into());
        }
        Ok(Self {
            host: host.into(),
            port,
            tls,
            starttls,
        })
    }
}
enum Transport {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}
impl Read for Transport {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.read(b),
            Self::Tls(s) => s.read(b),
        }
    }
}
impl Write for Transport {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(s) => s.write(b),
            Self::Tls(s) => s.write(b),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(s) => s.flush(),
            Self::Tls(s) => s.flush(),
        }
    }
}
pub(super) struct Wire {
    stream: Option<Transport>,
    tls_config: Option<Arc<rustls::ClientConfig>>,
    buffered: Vec<u8>,
    cursor: usize,
    progress: Instant,
}
fn retry(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
impl Wire {
    pub fn connect(endpoint: &Endpoint) -> Result<Self, Fault> {
        let addresses = (endpoint.host.as_str(), endpoint.port)
            .to_socket_addrs()
            .map_err(|e| format!("DNS {}: {e}", endpoint.host))?;
        let mut stream = None;
        for address in addresses.take(8) {
            crate::execution::check()?;
            if let Ok(s) = TcpStream::connect_timeout(&address, Duration::from_secs(5)) {
                stream = Some(s);
                break;
            }
        }
        let stream = stream.ok_or_else(|| {
            format!(
                "Не удалось подключиться к {}:{}",
                endpoint.host, endpoint.port
            )
        })?;
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .map_err(|e| e.to_string())?;
        let mut result = Self {
            stream: Some(Transport::Plain(stream)),
            tls_config: None,
            buffered: Vec::new(),
            cursor: 0,
            progress: Instant::now(),
        };
        if endpoint.tls {
            result.tls(&endpoint.host)?;
        }
        Ok(result)
    }
    pub fn peer_ip(&self) -> Result<String, Fault> {
        let s = match self.stream.as_ref().ok_or("Соединение закрыто")? {
            Transport::Plain(s) => s,
            Transport::Tls(s) => &s.sock,
        };
        Ok(s.peer_addr().map_err(|e| e.to_string())?.ip().to_string())
    }
    pub fn tls(&mut self, host: &str) -> Result<(), Fault> {
        let ca_file = std::env::var_os("KODEKS_EXCHANGE_CA_FILE").filter(|s| !s.is_empty());
        let config = tls_config(ca_file.as_deref().map(std::path::Path::new))?;
        self.tls_with_config(host, config)
    }
    /// FTPS data channels share only their own control session's cache, not
    /// credentials or TLS tickets from unrelated exchange requests.
    pub fn tls_from(&mut self, control: &Self, host: &str) -> Result<(), Fault> {
        let config = control
            .tls_config
            .clone()
            .ok_or("FTP: управляющий канал не использует TLS")?;
        self.tls_with_config(host, config)
    }
    fn tls_with_config(
        &mut self,
        host: &str,
        config: Arc<rustls::ClientConfig>,
    ) -> Result<(), Fault> {
        if self.cursor != self.buffered.len() {
            return Err("Непрочитанные данные перед TLS".into());
        }
        let Some(Transport::Plain(socket)) = self.stream.take() else {
            return Err("TLS уже включён".into());
        };
        let name = rustls::pki_types::ServerName::try_from(host.to_owned())
            .map_err(|_| "Некорректное имя TLS сервера")?;
        let connection =
            rustls::ClientConnection::new(config.clone(), name).map_err(|e| e.to_string())?;
        self.tls_config = Some(config);
        self.stream = Some(Transport::Tls(Box::new(rustls::StreamOwned::new(
            connection, socket,
        ))));
        Ok(())
    }
    /// Signal an authenticated TLS EOF before closing an FTP data socket.
    /// A bare TCP close can be a truncated transfer and must not imply success.
    pub fn finish(&mut self) -> Result<(), Fault> {
        let protected = matches!(self.stream, Some(Transport::Tls(_)));
        if let Some(Transport::Tls(s)) = self.stream.as_mut() {
            s.conn.send_close_notify();
        }
        self.send(&[])?;
        if protected {
            // Drain the peer's close_notify (and TLS 1.3 tickets). Dropping a
            // socket with unread records can send RST and discard an upload.
            let mut unexpected = [0; 1];
            if self.read(&mut unexpected)? != 0 {
                return Err("FTP: неожиданные данные при завершении TLS".into());
            }
            return Ok(());
        }
        let socket = match self.stream.as_ref().ok_or("Соединение закрыто")? {
            Transport::Plain(s) => s,
            Transport::Tls(s) => &s.sock,
        };
        socket
            .shutdown(std::net::Shutdown::Write)
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    fn check(&self) -> Result<(), Fault> {
        crate::execution::check()?;
        if self.progress.elapsed() > IDLE {
            return Err("Сервер обмена не передаёт данные более 60 секунд".into());
        }
        Ok(())
    }
    pub fn send(&mut self, mut data: &[u8]) -> Result<(), Fault> {
        while !data.is_empty() {
            self.check()?;
            match self
                .stream
                .as_mut()
                .ok_or("Соединение закрыто")?
                .write(data)
            {
                Ok(0) => return Err("Сервер закрыл соединение при записи".into()),
                Ok(n) => {
                    self.progress = Instant::now();
                    data = &data[n..];
                }
                Err(e) if retry(&e) => (),
                Err(e) => return Err(format!("Запись в соединение обмена: {e}").into()),
            }
        }
        loop {
            self.check()?;
            match self.stream.as_mut().unwrap().flush() {
                Ok(()) => break,
                Err(e) if retry(&e) => (),
                Err(e) => return Err(e.to_string().into()),
            }
        }
        Ok(())
    }
    pub fn command(&mut self, command: &str) -> Result<(), Fault> {
        clean(command)?;
        self.send(format!("{command}\r\n").as_bytes())
    }
    fn read(&mut self, b: &mut [u8]) -> Result<usize, Fault> {
        loop {
            self.check()?;
            match self.stream.as_mut().ok_or("Соединение закрыто")?.read(b) {
                Ok(n) => {
                    if n > 0 {
                        self.progress = Instant::now();
                    }
                    return Ok(n);
                }
                Err(e) if retry(&e) => (),
                Err(e) => return Err(format!("Чтение соединения обмена: {e}").into()),
            }
        }
    }
    pub fn line(&mut self) -> Result<Vec<u8>, Fault> {
        let mut line = Vec::new();
        loop {
            if self.cursor == self.buffered.len() {
                let mut buffer = [0; 8192];
                let n = self.read(&mut buffer)?;
                if n == 0 {
                    return Err("Сервер закрыл соединение до завершения ответа".into());
                }
                self.buffered.clear();
                self.buffered.extend_from_slice(&buffer[..n]);
                self.cursor = 0;
            }
            let tail = &self.buffered[self.cursor..];
            let n = tail
                .iter()
                .position(|b| *b == b'\n')
                .map(|i| i + 1)
                .unwrap_or(tail.len());
            line.extend_from_slice(&tail[..n]);
            self.cursor += n;
            if line.len() > 1024 * 1024 {
                return Err("Слишком длинная строка протокола обмена".into());
            }
            if line.last() == Some(&b'\n') {
                return Ok(line);
            }
        }
    }
    pub fn reply(&mut self) -> Result<(u16, String), Fault> {
        let first = String::from_utf8_lossy(&self.line()?).into_owned();
        let bytes = first.as_bytes();
        if bytes.len() < 4 || !bytes[..3].iter().all(u8::is_ascii_digit) {
            return Err("Некорректный ответ сервера обмена".into());
        }
        let code = first[..3].parse::<u16>().unwrap();
        let continuation = bytes[3] == b'-';
        let mut lines = first;
        if continuation {
            for _ in 0..1000 {
                let line = String::from_utf8_lossy(&self.line()?).into_owned();
                let last = line.starts_with(&format!("{code} "));
                lines.push_str(&line);
                if last {
                    return Ok((code, lines));
                }
            }
            return Err("Слишком длинный ответ сервера".into());
        }
        Ok((code, lines))
    }
    pub fn expect(&mut self, codes: &[u16]) -> Result<String, Fault> {
        let (code, text) = self.reply()?;
        if !codes.contains(&code) {
            return Err(format!("Сервер обмена ответил кодом {code}, ожидается {codes:?}").into());
        }
        Ok(text)
    }
    pub fn bytes(&mut self, limit: usize) -> Result<Vec<u8>, Fault> {
        let mut result = self.buffered[self.cursor..].to_vec();
        self.cursor = self.buffered.len();
        loop {
            let mut buffer = [0; 65536];
            let n = self.read(&mut buffer)?;
            if n == 0 {
                return Ok(result);
            }
            if result.len().saturating_add(n) > limit {
                return Err("Превышен размер получаемого файла".into());
            }
            result.extend_from_slice(&buffer[..n]);
        }
    }
    pub fn reset_idle(&mut self) {
        self.progress = Instant::now();
    }
}
