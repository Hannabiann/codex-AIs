//! Rust settings file; the separate network form writes the native HTTP port.
#[path = "network_settings.rs"]
mod network;
#[path = "settings_html.rs"]
mod html;
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
    pub workers: crate::worker_config::Mode,
    pub interface_workers: usize,
    pub log_retention_days: usize,
    pub keep_alive_seconds: u64,
    pub listen_scope: crate::listen_config::Scope,
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
            workers: crate::worker_config::Mode::Auto,
            interface_workers: 1,
            log_retention_days: 7,
            keep_alive_seconds: 60,
            listen_scope: crate::listen_config::Scope::All,
        }
    }
}
const FIELDS: [(&str, &str, usize, usize); 11] = [
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
    ("listen_scope", "Сетевые интерфейсы", 0, 0),
    ("interface_workers", "Исполнители интерфейса (входят в общее число)", 1, 8),
    ("log_retention_days", "Хранить ZIP-архивы логов, дней (0 — бессрочно)", 0, 3650),
];
impl Settings {
    fn values(&self) -> [String; 11] {
        [
            self.connections.to_string(),
            self.queue.to_string(),
            self.header_seconds.to_string(),
            self.body_seconds.to_string(),
            self.write_seconds.to_string(),
            self.history.to_string(),
            self.workers.to_string(),
            self.keep_alive_seconds.to_string(),
            self.listen_scope.text().into(),
            self.interface_workers.to_string(),
            self.log_retention_days.to_string(),
        ]
    }
    fn from_pairs(pairs: &[(String, String)]) -> Result<Self, String> {
        let mut values = [0; 11];
        let mut workers = crate::worker_config::Mode::Auto;
        let mut listen_scope = crate::listen_config::Scope::All;
        let mut seen = HashSet::new();
        for (key, value) in pairs {
            let Some(index) = FIELDS.iter().position(|f| f.0 == key) else {
                return Err(format!("Неизвестный параметр: {key}"));
            };
            if !seen.insert(index) {
                return Err(format!("Повторный параметр: {key}"));
            }
            if key == "workers" {
                workers = crate::worker_config::Mode::parse(value)?;
                continue;
            }
            if key == "listen_scope" {
                listen_scope = crate::listen_config::Scope::parse(value)?;
                continue;
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
        crate::pool_layout::Layout::new(workers.resolve(values[9]).workers, values[9])?;
        Ok(Self {
            interface_workers: values[9],
            log_retention_days: values[10],
            connections: values[0],
            queue: values[1],
            header_seconds: values[2] as u64,
            body_seconds: values[3] as u64,
            write_seconds: values[4] as u64,
            history: values[5],
            workers,
            keep_alive_seconds: values[7] as u64,
            listen_scope,
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
        // Accept the retired local 0.30 preview setting, but never write it back.
        let old_themes: Vec<_> = pairs.iter().filter(|(k,_)|k=="admin_theme").collect();
        if old_themes.len()>1 || old_themes.iter().any(|(_,v)| !matches!(v.as_str(), "auto"|"light"|"dark")) {
            return Err("Некорректный прежний параметр admin_theme".into());
        }
        pairs.retain(|(k,_)|k!="admin_theme");
        // Old six-key files retain every saved setting and gain new defaults.
        for (key, value) in [
            ("workers", Self::default().workers.to_string()),
            ("interface_workers", Self::default().interface_workers.to_string()),
            ("log_retention_days", Self::default().log_retention_days.to_string()),
            ("listen_scope", Self::default().listen_scope.text().into()),
            (
                "keep_alive_seconds",
                Self::default().keep_alive_seconds.to_string(),
            ),
        ] {
            if !pairs.iter().any(|(k, _)| k == key) {
                pairs.push((key.into(), value));
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
    pub worker_selection: crate::worker_config::Selection,
    pub host: crate::listen_config::Host,
    saved: Mutex<Settings>,
    path: PathBuf,
    token: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retired_preview_theme_does_not_prevent_startup() {
        let settings=Settings::default();
        for theme in ["auto", "light", "dark"] {
            let read=Settings::parse(&(settings.encode()+&format!("admin_theme={theme}\n"))).unwrap();
            assert_eq!(read, settings);
            assert!(!read.encode().contains("admin_theme"));
        }
        assert!(Settings::parse(&(settings.encode()+"admin_theme=invalid\n")).is_err());
        assert!(Settings::parse(&(settings.encode()+"admin_theme=auto\nadmin_theme=dark\n")).is_err());
    }
    #[test]
    fn old_settings_keep_one_interface_and_roundtrip_new_count() {
        let old="connections=64\nqueue=16\nheader_seconds=10\nbody_seconds=10\nwrite_seconds=15\nhistory=128\nworkers=8\n";
        let mut s=Settings::parse(old).unwrap();assert_eq!(s.interface_workers,1);
        assert_eq!(s.log_retention_days,7);
        s.interface_workers=2;
        assert_eq!(Settings::parse(&s.encode()).unwrap(),s);
        assert!(Settings::parse(&(old.to_owned()+"interface_workers=6\n")).is_err());
        assert!(Settings::parse(&(old.to_owned()+"interface_workers=2\ninterface_workers=2\n")).is_err());
    }

    #[test]
    fn auto_stays_symbolic_and_applies_only_after_restart() {
        let directory=std::env::temp_dir().join(format!("codex-auto-settings-{}",nonce().unwrap()));
        std::fs::create_dir(&directory).unwrap();
        let path=directory.join("codex-AIs.settings");
        let manual=Settings { workers:crate::worker_config::Mode::Manual(4), ..Settings::default() };
        std::fs::write(&path,manual.encode()).unwrap();
        let store=Store::at(path.clone()).unwrap();
        assert_eq!(store.worker_selection.workers,4);
        assert!(store.worker_selection.explanation.contains("Ручная настройка"));
        let auto=Settings { interface_workers:2, ..Settings::default() };
        store.save(auto.clone()).unwrap();
        assert_eq!(store.worker_selection.workers,4);
        assert_eq!(store.active,manual);
        assert!(std::fs::read_to_string(&path).unwrap().contains("workers=auto\n"));
        assert_eq!(Settings::parse(&std::fs::read_to_string(path.with_extension("settings.bak")).unwrap()).unwrap(),manual);
        let restarted=Store::at(path.clone()).unwrap();
        assert_eq!(restarted.active,auto);
        let layout=crate::pool_layout::Layout::new(restarted.worker_selection.workers,2).unwrap();
        assert_eq!(layout.interfaces,2);
        assert!(restarted.worker_selection.explanation.contains("Автоматически"));
        assert_eq!(restarted.worker_selection.workers,auto.workers.resolve(2).workers);
        let page=String::from_utf8(restarted.response("GET",&[]).body).unwrap();
        assert!(page.contains("число тяжёлых исполнителей равно доступным физическим ядрам"));
        assert!(!page.contains("3, 4 или 6"));
        let monitor=crate::monitor::Monitor::new(auto.clone(),restarted.worker_selection.clone());
        let page=monitor.html();
        assert!(page.contains(&format!("data-workers=\"{}\"",restarted.worker_selection.workers)));
        assert!(page.contains("data-interface-workers=\"2\""));
        assert!(page.contains(&format!("data-heavy-workers=\"{}\"",restarted.worker_selection.workers-layout.bulk)));
        assert!(page.contains("SMT/HT"));
        drop(restarted);drop(store);
        std::fs::remove_file(path.with_extension("settings.bak")).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
pub(crate) fn nonce() -> std::io::Result<String> {
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
pub(crate) fn atomic_write(path: &Path, text: &[u8]) -> std::io::Result<()> {
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
        if let Some(path) = std::env::var_os("KODEKS_SETTINGS_FILE") {
            let path = PathBuf::from(path);
            if !path.is_absolute() || !path.is_file() {
                return Err(std::io::Error::other("KODEKS_SETTINGS_FILE must name an existing absolute settings file"));
            }
            return Self::at(path);
        }
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
        let mut selection = active.workers.resolve(active.interface_workers);
        let layout = crate::pool_layout::Layout::new(selection.workers, active.interface_workers)
            .map_err(std::io::Error::other)?;
        selection.explanation.push(' ');
        selection.explanation.push_str(&layout.description());
        Ok(Self {
            host: crate::listen_config::host(active.listen_scope).map_err(std::io::Error::other)?,
            worker_selection: selection,
            saved: Mutex::new(active.clone()),
            active,
            path,
            token: nonce()?,
        })
    }
    pub fn advanced(
        &self,
        engine: &crate::db::Db4Engine,
        method: &str,
        fields: &[(String, String)],
    ) -> crate::server::Response {
        if fields.iter().any(|(k, v)| k == "action" && v == "set-port") {
            return network::save(engine, &self.token, method, fields);
        }
        let mut response = self.response(method, fields);
        if response.mime.starts_with("text/html") {
            let form = network::form(engine, &self.token).unwrap_or_else(|e| {
                format!(
                    "<p class=error>Не удалось прочитать порт: {}</p>",
                    crate::monitor::escape(&e)
                )
            });
            let mut html = String::from_utf8(response.body).unwrap();
            html = html.replace("<!--native-port-form-->", &form);
            response.body = html.into_bytes();
        }
        response
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
        let html = self.render(&saved, error.as_deref());
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
