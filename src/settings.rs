//! Rust-specific settings. Native config.db4 is never opened for writing here.
use std::{
    collections::HashSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub connections: usize,
    pub queue: usize,
    pub header_seconds: u64,
    pub body_seconds: u64,
    pub write_seconds: u64,
    pub history: usize,
    pub workers: usize,
    pub keep_alive_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            connections: 64,
            queue: 16,
            header_seconds: 10,
            body_seconds: 10,
            write_seconds: 15,
            history: 128,
            workers: 4,
            keep_alive_seconds: 60,
        }
    }
}
const FIELDS: [(&str, &str, usize, usize); 8] = [
    ("connections", "Одновременные HTTP-соединения", 8, 4096),
    ("queue", "Ожидающие запросы KS", 1, 256),
    ("header_seconds", "Чтение HTTP-заголовков, секунд", 1, 300),
    ("body_seconds", "Чтение тела запроса, секунд", 1, 300),
    ("write_seconds", "Отправка ответа, секунд", 1, 600),
    ("history", "Завершённые операции в мониторе", 16, 4096),
    ("workers", "Исполнители KS/DB", 1, 32),
    (
        "keep_alive_seconds",
        "Ожидание следующего HTTP-запроса, секунд (0 — закрывать)",
        0,
        600,
    ),
];
impl Settings {
    fn values(&self) -> [usize; 8] {
        [
            self.connections,
            self.queue,
            self.header_seconds as usize,
            self.body_seconds as usize,
            self.write_seconds as usize,
            self.history,
            self.workers,
            self.keep_alive_seconds as usize,
        ]
    }
    fn from_pairs(pairs: &[(String, String)]) -> Result<Self, String> {
        let mut values = [0; 8];
        let mut seen = HashSet::new();
        for (key, value) in pairs {
            let Some(index) = FIELDS.iter().position(|f| f.0 == key) else {
                return Err(format!("Неизвестный параметр: {key}"));
            };
            if !seen.insert(index) {
                return Err(format!("Повторный параметр: {key}"));
            }
            let n = value
                .parse::<usize>()
                .map_err(|_| format!("{}: требуется целое число", FIELDS[index].1))?;
            if !(FIELDS[index].2..=FIELDS[index].3).contains(&n) {
                return Err(format!(
                    "{}: допустимо {}–{}",
                    FIELDS[index].1, FIELDS[index].2, FIELDS[index].3
                ));
            }
            values[index] = n;
        }
        if seen.len() != FIELDS.len() {
            return Err("Указаны не все параметры Rust".into());
        }
        Ok(Self {
            connections: values[0],
            queue: values[1],
            header_seconds: values[2] as u64,
            body_seconds: values[3] as u64,
            write_seconds: values[4] as u64,
            history: values[5],
            workers: values[6],
            keep_alive_seconds: values[7] as u64,
        })
    }
    fn parse(text: &str) -> Result<Self, String> {
        let mut pairs = Vec::new();
        for line in text
            .trim_start_matches('\u{feff}')
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
        {
            let (key, value) = line.split_once('=').ok_or("Ожидается имя=значение")?;
            pairs.push((key.trim().to_owned(), value.trim().to_owned()));
        }
        // Old six-key files retain every saved setting and gain new defaults.
        for (key, value) in [
            ("workers", Self::default().workers as u64),
            ("keep_alive_seconds", Self::default().keep_alive_seconds),
        ] {
            if !pairs.iter().any(|(k, _)| k == key) {
                pairs.push((key.into(), value.to_string()));
            }
        }
        Self::from_pairs(&pairs)
    }
    fn encode(&self) -> String {
        let mut out = "# codex-AIs settings; apply on server restart\n".to_string();
        for (field, value) in FIELDS.iter().zip(self.values()) {
            out.push_str(&format!("{}={value}\n", field.0));
        }
        out
    }
}
pub struct Store {
    pub active: Settings,
    saved: Mutex<Settings>,
    path: PathBuf,
    token: String,
}
fn nonce() -> std::io::Result<String> {
    let mut bytes = [0u8; 32];
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(
                algorithm: *mut std::ffi::c_void,
                buffer: *mut u8,
                length: u32,
                flags: u32,
            ) -> i32;
        }
        if unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                2,
            )
        } < 0
        {
            return Err(std::io::Error::other("System random source failed"));
        }
    }
    #[cfg(unix)]
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    #[cfg(not(any(windows, unix)))]
    return Err(std::io::Error::other("System random source unavailable"));
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub(crate) fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
        }
        let from: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 1 | 8) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(source, destination)
    }
}
fn atomic_write(path: &Path, text: &[u8]) -> std::io::Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", nonce()?));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(text)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        #[cfg(unix)]
        std::fs::File::open(path.parent().unwrap())?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}
impl Store {
    pub fn load() -> std::io::Result<Self> {
        let exe = std::env::current_exe()?;
        Self::at(
            exe.parent()
                .ok_or_else(|| std::io::Error::other("Executable directory missing"))?
                .join("codex-AIs.settings"),
        )
    }
    fn at(path: PathBuf) -> std::io::Result<Self> {
        let active = match std::fs::File::open(&path) {
            Ok(file) => {
                let mut text = String::new();
                file.take(16385).read_to_string(&mut text)?;
                if text.len() > 16384 {
                    return Err(std::io::Error::other("Settings file exceeds 16 KiB"));
                }
                Settings::parse(&text)
                    .map_err(|e| std::io::Error::other(format!("{}: {e}", path.display())))?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            saved: Mutex::new(active.clone()),
            active,
            path,
            token: nonce()?,
        })
    }
    fn save(&self, settings: Settings) -> std::io::Result<()> {
        let mut saved = self.saved.lock().unwrap();
        // Backup the last valid settings before replacing the complete file.
        if self.path.exists() {
            let previous = std::fs::read(&self.path)?;
            atomic_write(&self.path.with_extension("settings.bak"), &previous)?;
        }
        atomic_write(&self.path, settings.encode().as_bytes())?;
        *saved = settings;
        Ok(())
    }
    pub fn response(&self, method: &str, fields: &[(String, String)]) -> crate::server::Response {
        let mut error = None;
        let mut status = "200 OK";
        if method == "POST" {
            if fields.iter().filter(|(k, _)| k == "csrf").count() != 1
                || !fields.iter().any(|(k, v)| k == "csrf" && v == &self.token)
            {
                status = "403 Forbidden";
                error = Some("Форма устарела. Обновите страницу и повторите сохранение.".into());
            } else {
                let pairs: Vec<_> = fields
                    .iter()
                    .filter(|(k, _)| k != "csrf")
                    .cloned()
                    .collect();
                match Settings::from_pairs(&pairs) {
                    Err(e) => {
                        status = "400 Bad Request";
                        error = Some(e);
                    }
                    Ok(settings) => match self.save(settings) {
                        Ok(()) => {
                            return crate::server::Response {
                                status: "303 See Other",
                                mime: "text/plain; charset=utf-8",
                                body: Vec::new(),
                                location: Some("/admin?advanced&saved=1".into()),
                                headers: vec![("Cache-Control".into(), "no-store".into())],
                            }
                        }
                        Err(e) => {
                            status = "500 Internal Server Error";
                            error = Some(format!("Настройки не сохранены: {e}"));
                        }
                    },
                }
            }
        }
        let saved = self.saved.lock().unwrap().clone();
        let escape = crate::monitor::escape;
        let mut html = String::from("<!doctype html><html lang=ru><meta charset=utf-8><title>Расширенные настройки · codex-AIs</title><style>body{font:16px Arial;margin:24px;background:#e3ddda;color:#222;max-width:1000px}table{border-collapse:collapse;background:white;width:100%}th,td{padding:12px;border:1px solid #aaa;text-align:left}input{width:9em;font:inherit}button{font:inherit;padding:10px 18px;margin-top:16px}.notice{padding:12px;background:#fff4c2}.error{color:#a00}code{overflow-wrap:anywhere}</style><p><a href='/admin/'>Администрирование</a> → Установки → Расширенные настройки</p><h1>Параметры codex-AIs</h1><p>Сохранённые значения применяются после перезапуска сервера. Сеть обслуживает Tokio; KS и доступ к данным выполняют отдельные исполнители. Их число задаётся параметром «Исполнители KS/DB». Число HTTP-соединений и длина очереди настраиваются независимо. Помимо общей очереди предусмотрено по одному резервному месту для владельца и выделенного интерфейса, если они есть.</p>");
        if let Some(error) = error {
            html.push_str(&format!("<p class=error>{}</p>", escape(&error)));
        }
        if saved != self.active {
            html.push_str("<p class=notice>Есть сохранённые изменения. Для их применения перезапустите codex-AIs.</p>");
        }
        html.push_str(&format!("<p>Файл: <code>{}</code>. Предыдущая версия сохраняется в <code>codex-AIs.settings.bak</code>.</p><form method=post action='/admin?advanced'><input type=hidden name=csrf value='{}'><table><tr><th>Параметр</th><th>Работает сейчас</th><th>После перезапуска</th></tr>", escape(&self.path.display().to_string()), self.token));
        for ((field, active), value) in FIELDS.iter().zip(self.active.values()).zip(saved.values())
        {
            html.push_str(&format!("<tr><td><label for='{}'>{}</label> ({}–{})</td><td>{active}</td><td><input required type=number id='{}' name='{}' min='{}' max='{}' value='{value}'></td></tr>", field.0, field.1, field.2, field.3, field.0, field.0, field.2, field.3));
        }
        html.push_str("</table><button type=submit>Сохранить настройки Rust</button></form><h2>Параметры оригинального сервера</h2><p>config.db4 используется для каталогов, томов и прав доступа и здесь не изменяется. Число исполнителей и очередь Rust задаются выше. По умолчанию работают 4 исполнителя: владелец каталогов/Word/At, интерфейс /20a и два исполнителя тяжёлых запросов. При 1 исполнителе работа последовательная, при 2 интерфейс разделяет владельца. Увеличение числа исполнителей увеличивает потребление памяти кэшами. Публикация DB4 согласована и последовательна; расчёты идут параллельно. Ограничение памяти старого Win32-процесса не применяется к Rust: кэши имеют собственные пределы, это не жёсткий предел всей памяти процесса.</p><p>HTTP/1.1 keep-alive включён по умолчанию, HTTP/1.0 — по запросу клиента. Таймаут выше относится только к ожиданию следующего запроса, в том числе Word /99, и не ограничивает длительность расчёта. Сжатие ответов пока не реализовано. Периодическую очистку нативного хэша заменяют ограниченные кэши Rust. Заголовок X-Real-IP не подменяет адрес клиента. Журналы KS задаются KODEKS_LOG_DIR; адрес и порт слушателя — KODEKS_HOST и KODEKS_PORT. Таймаут отправки ответа отсчитывается после вычисления результата и не ограничивает длительность отчёта.</p><p><a href='/admin?status'>Состояние сервера и очереди</a></p></html>");
        crate::server::Response {
            status,
            mime: "text/html; charset=utf-8",
            body: html.into_bytes(),
            location: None,
            headers: vec![
                ("Cache-Control".into(), "no-store".into()),
                // The original /admin/ shell opens settings in its own frame.
                ("X-Frame-Options".into(), "SAMEORIGIN".into()),
            ],
        }
    }
}
