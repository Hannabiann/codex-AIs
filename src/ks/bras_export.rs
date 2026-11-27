//! Host operations used by the stored BRAS exporter, without Windows batch files.
use super::*;
use std::{
    io::Write,
    path::{Component, Path, PathBuf},
};
const KEY: &str = "SOFTWARE\\Kodeks\\Web\\Настройки\\Обмен с БРАС";

impl Runtime<'_> {
    pub(super) fn bras_job_principal(&self) -> bool {
        if !self.scheduled_request || self.path.trim_end_matches('/') != "/20a" {
            return false;
        }
        let Ok(Value::Object(p)) = self.get("mp_g_ascJobPrincipal") else {
            return false;
        };
        let p = p.borrow();
        let name = p.get("sJobUrlName").map(Value::text).unwrap_or_default();
        let mode = p.get("nJobPagemode").map(Value::text).unwrap_or_default();
        (name.eq_ignore_ascii_case("gixport") && mode == "1")
            || (name.eq_ignore_ascii_case("~brasExchangeTasks") && mode == "export")
    }
    pub(super) fn bras_export_date(&mut self, id: u32, args: &[Value]) -> Result<bool, Fault> {
        if !self.bras_export_allowed {
            return Ok(false);
        }
        let [Value::String(name), Value::Number(1.0), Value::String(value)] = args else {
            return Ok(false);
        };
        if !matches!(
            name.as_str(),
            "Дата последнего успешного экспорта" | "Время последнего запуска"
        ) {
            return Ok(false);
        }
        let reg = self.registry.as_ref().ok_or("Registry unavailable")?;
        if reg.open(KEY)? != Some(id) {
            return Ok(false);
        }
        let root = reg.root;
        let path =
            Path::new(self.data_root.as_deref().ok_or("Data root unavailable")?).join("config.db4");
        loop {
            crate::execution::check()?;
            let mut tx = crate::db::writer::registry::Transaction::open(&path, root)?;
            tx.set(id, name, "REG_SZ", Some(value))?;
            let next = tx.registry.clone();
            match tx.commit() {
                Ok(_) => {
                    self.registry = Some(next);
                    self.settings_generation = u64::MAX;
                    return Ok(true);
                }
                Err(e) if e == "Настройки изменились. Откройте форму заново." => {
                    std::thread::yield_now()
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub(super) fn bras_write_xml(&mut self, values: &[Value]) -> Result<Value, Fault> {
        if !self.bras_export_allowed {
            return Err("BRAS file export requires its scheduled job".into());
        }
        let [Value::String(stem), Value::ReadStream(input)] = values else {
            return Err("BRAS export expects a filename stem and XML stream".into());
        };
        if stem.is_empty()
            || stem.len() > 180
            || !stem
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
        {
            return Err("Invalid BRAS export filename".into());
        }
        let reg = self.registry.as_ref().ok_or("Registry unavailable")?;
        let key = reg.open(KEY)?.ok_or("BRAS settings missing")?;
        let root = Path::new(self.data_root.as_deref().ok_or("Data root unavailable")?);
        let mut destinations = Vec::new();
        for setting in [
            "Каталог для сохранения xml",
            "Каталог для хранения копии результатов выгрузки для сторонних ресурсов",
        ] {
            let name = reg
                .query(key, setting)?
                .map(|v| v.value.as_str())
                .unwrap_or("");
            if name.is_empty() {
                return Err(format!("BRAS setting {setting} is empty").into());
            }
            let dir = root.join(super::super::path::script_path(name));
            if dir.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err("BRAS directory contains parent traversal".into());
            }
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("BRAS directory {}: {e}", dir.display()))?;
            let dir = dir.canonicalize().map_err(|e| e.to_string())?;
            if !destinations.contains(&dir) {
                destinations.push(dir);
            }
        }
        let mut compressed = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        {
            let input = input.borrow();
            if input.closed {
                return Err("BRAS XML stream is closed".into());
            }
            // Native _writeXml resets the stream before copying it.
            for chunk in input.data.chunks(64 * 1024) {
                crate::execution::check()?;
                compressed.write_all(chunk).map_err(|e| e.to_string())?;
            }
        }
        let bytes = compressed.finish().map_err(|e| e.to_string())?;
        for dir in destinations {
            crate::execution::check()?;
            publish(&dir, stem, &bytes)?;
        }
        let end = input.borrow().data.len();
        input.borrow_mut().position = end;
        Ok(Value::Null)
    }
}

fn publish(dir: &Path, stem: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    // A consumer sees only complete .gz files. A crash leaves a .part file,
    // never a truncated delivery; existing deliveries are not overwritten.
    for number in 0..1000 {
        let name = if number == 0 {
            format!("{stem}.xml.gz")
        } else {
            format!("{stem}-{number}.xml.gz")
        };
        let final_path = dir.join(&name);
        if final_path.exists() {
            continue;
        }
        let temporary = dir.join(format!(".{name}.{}.part", std::process::id()));
        let mut file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        };
        let result = (|| {
            file.write_all(bytes).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
            drop(file);
            publish_complete(&temporary, &final_path)
                .map_err(|e| format!("BRAS publish {}: {e}", final_path.display()))?;
            #[cfg(unix)]
            std::fs::File::open(dir)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            Ok(final_path)
        })();
        let _ = std::fs::remove_file(&temporary);
        return result;
    }
    Err("BRAS export filename collision".into())
}

// Windows delivery directories can be SMB shares: do not require hard-link
// support there. Neither implementation replaces an existing delivery.
#[cfg(windows)]
pub(super) fn publish_complete(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(from: *const u16, to: *const u16, flags: u32) -> i32;
    }
    let from: Vec<_> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<_> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // MOVEFILE_WRITE_THROUGH, without MOVEFILE_REPLACE_EXISTING.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), 8) } == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}
#[cfg(not(windows))]
pub(super) fn publish_complete(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::hard_link(from, to)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn publication_preserves_existing_delivery_and_leaves_no_partial_file() {
        let dir = std::env::temp_dir().join(format!(
            "codex-bras-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let first = publish(&dir, "case", b"complete first").unwrap();
        let second = publish(&dir, "case", b"complete second").unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"complete first");
        assert_eq!(std::fs::read(&second).unwrap(), b"complete second");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        assert!(publish_complete(&first, &second).is_err());
        assert_eq!(std::fs::read(&first).unwrap(), b"complete first");
        assert_eq!(std::fs::read(&second).unwrap(), b"complete second");
        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn http_or_unrelated_jobs_cannot_enable_bras_writes() {
        let mut r = Runtime::new("/20a");
        let p = Rc::new(RefCell::new(HashMap::from([
            ("sJobUrlName".into(), Value::String("gixport".into())),
            ("nJobPagemode".into(), Value::String("1".into())),
        ])));
        r.scopes[0]
            .borrow_mut()
            .insert("mp_g_ascJobPrincipal".into(), Value::Object(p.clone()));
        assert!(!r.bras_job_principal());
        assert!(r.bras_write_xml(&[]).is_err());
        r.scheduled_request = true;
        assert!(r.bras_job_principal());
        p.borrow_mut()
            .insert("sJobUrlName".into(), Value::String("unrelated".into()));
        assert!(!r.bras_job_principal());
    }
}
