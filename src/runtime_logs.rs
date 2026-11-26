//! Shared server journals and the redirected KS log directory.
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub const DIRECTORY: &str = "Log-codex-AIS";
#[path = "log_rotation.rs"]
mod rotation;
static ROOT: OnceLock<PathBuf> = OnceLock::new();
static ERROR: OnceLock<Mutex<Journal>> = OnceLock::new();
static ACCESS: OnceLock<Mutex<Journal>> = OnceLock::new();
struct Journal {
    path: PathBuf,
    file: File,
    generation: u64,
}

pub fn configured_root() -> Option<PathBuf> {
    ROOT.get()
        .cloned()
        .or_else(|| std::env::var_os("KODEKS_LOG_DIR").map(PathBuf::from))
}

pub fn initialize(data: &Path) -> Result<(), String> {
    let configured = std::env::var_os("KODEKS_LOG_DIR").map(PathBuf::from);
    let logs = prepare(data, configured.as_deref())?;
    ROOT.set(logs.clone())
        .map_err(|_| "Log directory already initialized")?;
    rotation::initialize(&logs)?;
    for (name, slot) in [("error.log", &ERROR), ("access.log", &ACCESS)] {
        slot.set(Mutex::new(Journal {
            path: logs.join(name),
            file: open_journal(&logs, name)?,
            generation: rotation::generation(),
        }))
        .map_err(|_| "Journal already initialized")?;
    }
    eprintln!("Log directory: {}", logs.display());
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        error("panic", &info.to_string());
        previous(info);
    }));
    Ok(())
}

pub fn start_rotation(days: usize) -> std::io::Result<()> {
    rotation::start(days)
}
pub fn write_guard() -> std::sync::MutexGuard<'static, ()> {
    rotation::write_guard()
}
pub fn generation() -> u64 {
    rotation::generation()
}

// A dedicated child is safe, but never allow KS logs to overwrite arbitrary
// data directories or place the data tree inside the log tree.
pub fn validate_root(data: &Path, logs: &Path) -> Result<(), String> {
    if data.starts_with(logs) || (logs.starts_with(data) && logs != data.join(DIRECTORY)) {
        return Err(format!(
            "Logs must use data/{DIRECTORY} or a separate directory outside the data tree"
        ));
    }
    Ok(())
}

fn prepare(data: &Path, configured: Option<&Path>) -> Result<PathBuf, String> {
    let data = data.canonicalize().map_err(|e| e.to_string())?;
    let target = if let Some(path) = configured {
        if path.as_os_str().is_empty() {
            return Err("KODEKS_LOG_DIR is empty".into());
        }
        std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path)
    } else {
        data.join(DIRECTORY)
    };
    let error = |e| {
        format!(
            "Log directory {}: {e}. Check write permissions or set KODEKS_LOG_DIR",
            target.display()
        )
    };
    // Resolve existing parents before creation, including junctions/symlinks.
    let mut ancestor = target.as_path();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(
            ancestor
                .file_name()
                .ok_or_else(|| error("invalid path".into()))?
                .to_os_string(),
        );
        ancestor = ancestor
            .parent()
            .ok_or_else(|| error("no existing parent".into()))?;
    }
    let mut resolved = ancestor.canonicalize().map_err(|e| error(e.to_string()))?;
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    validate_root(&data, &resolved).map_err(&error)?;
    std::fs::create_dir_all(&resolved).map_err(|e| error(e.to_string()))?;
    let resolved = resolved.canonicalize().map_err(|e| error(e.to_string()))?;
    validate_root(&data, &resolved).map_err(&error)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| error(e.to_string()))?
        .as_nanos();
    let probe = resolved.join(format!(".startup-{}-{nonce}", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|e| error(e.to_string()))?;
    drop(file);
    std::fs::remove_file(probe).map_err(|e| error(e.to_string()))?;
    Ok(resolved)
}

fn open_journal(root: &Path, name: &str) -> Result<File, String> {
    let path = root.join(name);
    if path.exists() {
        let resolved = path.canonicalize().map_err(|e| e.to_string())?;
        if resolved.parent() != Some(root) || !resolved.is_file() {
            return Err(format!("Invalid journal destination: {}", path.display()));
        }
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Journal {}: {e}", path.display()))
}

fn timestamp() -> String {
    use crate::ks::date::Date;
    if let (Ok(date), Ok(offset)) = (Date::now(), Date::offset()) {
        let [y, m, d, h, min, s, ms] = date.parts();
        let zone = -offset;
        format!(
            "{y:04}-{:02}-{d:02}T{h:02}:{min:02}:{s:02}.{ms:03}{}{:02}:{:02}",
            m + 1,
            if zone < 0 { '-' } else { '+' },
            zone.abs() / 60,
            zone.abs() % 60
        )
    } else {
        format!(
            "unix-ms:{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        )
    }
}

/// One physical line per event, including errors containing line breaks.
pub fn field(value: &str) -> String {
    let mut out = String::new();
    for c in value.chars().take(8192) {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if c.is_control() => out.push('?'),
            c => out.push(c),
        }
    }
    out
}

fn append(file: &Mutex<Journal>, line: &str) -> std::io::Result<()> {
    // No monitor/DB lock is held here. Each complete event is one append;
    // a process restart reopens the same file without truncating its history.
    let _gate = if std::thread::panicking() {
        rotation::try_write_guard()
            .ok_or_else(|| std::io::Error::other("Journal busy during panic"))?
    } else {
        write_guard()
    };
    let mut journal = if std::thread::panicking() {
        file.try_lock()
            .map_err(|_| std::io::Error::other("Journal busy during panic"))?
    } else {
        file.lock().unwrap_or_else(|e| e.into_inner())
    };
    if journal.generation != generation() {
        journal.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal.path)?;
        journal.generation = generation();
    }
    journal.file.write_all(line.as_bytes())
}

pub fn error(category: &str, message: &str) {
    let text = format!(
        "{} ERROR pid={} thread={:?} {}: {}\n",
        timestamp(),
        std::process::id(),
        std::thread::current().id(),
        field(category),
        field(message)
    );
    if let Some(file) = ERROR.get() {
        if let Err(e) = append(file, &text) {
            eprintln!("error.log write failed: {e}");
        }
    }
    eprint!("{text}");
}

pub fn access(message: &str) {
    if let Some(file) = ACCESS.get() {
        let text = format!("{} pid={} {}\n", timestamp(), std::process::id(), message);
        if let Err(e) = append(file, &text) {
            eprintln!("access.log write failed: {e}");
        }
    }
}

pub fn description() -> String {
    let mut html = format!("<p>Логи хранятся в папке с КОДЕКС: <code>/{DIRECTORY}/</code>.</p>");
    if std::env::var_os("KODEKS_LOG_DIR").is_some() {
        if let Some(path) = configured_root() {
            html.push_str(&format!("<p>На этом запуске каталог переопределён через KODEKS_LOG_DIR: <code>{}</code>.</p>", crate::monitor::escape(&path.display().to_string())));
        }
    }
    html.push_str("<p><code>error.log</code> — ошибки запросов, фоновых заданий и запуска; <code>access.log</code> — завершение запросов и их длительность. Здесь же сохраняются журналы KS. Серверные журналы записываются в UTF-8; время содержит часовой пояс сервера.</p><p>ZIP-архивы за прошлые дни: <code>OLD_Log/</code>. По умолчанию архивы хранятся 7 дней; срок задаётся в <a href='/admin?advanced'>расширенных настройках</a>.</p>");
    html
}

/// Keep the native settings form except its inapplicable kodweb log controls.
pub fn admin_html(html: String, object: &str) -> String {
    if object == "title" {
        return regex::Regex::new(r"(?i)</body>")
            .unwrap()
            .replace(&html, |_: &regex::Captures<'_>| {
                format!("{} </body>", description())
            })
            .into_owned();
    }
    let pattern = match object {
        "pref" => {
            r"(?is)<tr\b[^>]*>\s*<td\b[^>]*>\s*<font\b[^>]*>\s*<b>\s*Протокол:\s*</b>.*?(<tr\b[^>]*>\s*<td\b[^>]*>\s*<font\b[^>]*>\s*<b>\s*Почта:\s*</b>)"
        }
        "helppref" => r"(?is)<p>\s*<li>\s*<b>Параметры ведения протоколов</b>.*?</ul>",
        _ => return html,
    };
    let re = regex::Regex::new(pattern).unwrap();
    re.replace(&html, |caps: &regex::Captures<'_>| {
        if object == "pref" {
            // Native pref uses presence of format to also save ReservDaily.
            // Preserve its selected value without offering irrelevant controls.
            let selected = regex::Regex::new(r#"(?is)<option\b[^>]*value=["']?([01])["']?[^>]*\bselected\b"#).unwrap();
            let value = selected.captures(&caps[0]).and_then(|c| c.get(1).map(|m|m.as_str().to_owned())).unwrap_or_else(||"0".into());
            format!("<tr><td colspan=2><input type=hidden name=format value='{value}'><h3>Журналы codex-AIs</h3>{}</td></tr>{}", description(), &caps[1])
        } else {
            format!("<li><b>Журналы codex-AIs</b>{}</li>", description())
        }
    }).into_owned()
}

#[cfg(test)]
#[path = "runtime_logs_tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) fn rotate_for_test(root: &Path, from: &str, to: &str) {
    rotation::rotate_test(root, from, to);
}
