//! Scoped file exchange for the stored court-transfer scripts.
use super::*;
use std::{
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
#[path = "exchange_archive.rs"]
mod archive;
#[path = "exchange_ftp.rs"]
mod ftp;
#[path = "exchange_mail.rs"]
mod mail;
#[path = "exchange_network.rs"]
mod network;
#[path = "exchange_pkg.rs"]
mod pkg;
#[path = "exchange_wire.rs"]
mod wire;
static SERIAL: AtomicU64 = AtomicU64::new(0);
#[cfg(test)]
#[path = "exchange_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct Access {
    pub request: bool,
    pub scheduled: bool,
    work: Option<PathBuf>,
    roots: Vec<PathBuf>,
    destinations: Vec<PathBuf>,
    archiver: Option<PathBuf>,
    backup: Option<PathBuf>,
    network: network::State,
    packet_size: Option<usize>,
    transfer_id: String,
    auto_errors: Vec<String>,
}
impl Access {
    fn cleanup_files(&mut self) -> Result<(), String> {
        if let Some(work) = self.work.take() {
            for entry in std::fs::read_dir(&work).map_err(|e| e.to_string())? {
                let entry = entry.map_err(|e| e.to_string())?;
                if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
                    return Err("Unexpected directory in exchange workspace".into());
                }
                std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            }
            std::fs::remove_dir(work).map_err(|e| e.to_string())?;
        }
        self.roots.clear();
        self.destinations.clear();
        Ok(())
    }
    pub(super) fn abort(&mut self) {
        // Fatal host errors/cancellation bypass KS finally. Drop connections
        // without POP3 QUIT, so uncommitted deletions are not acknowledged.
        self.network = network::State::default();
        if let Err(e) = self.cleanup_files() {
            crate::runtime_logs::error("exchange cleanup", &e);
        }
    }
}

/// The caller supplies the dispatcher target, never a script-mutable Request.
pub(super) fn scheduled_target(scheduled: bool, catalog: &str, target: &str) -> bool {
    scheduled && catalog.trim_end_matches('/') == "/20a"
        && matches!(target.to_ascii_lowercase().as_str(),
            "/20a/~senddocsauto" | "/20a/~senddocsautoget")
}

fn packet_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with(['.', '-'])
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        && (lower.ends_with(".7z")
            || lower.ends_with(".pkg")
            || lower.rsplit_once(".7z.").is_some_and(|(_, n)| {
                n.len() >= 3
                    && n.bytes().all(|b| b.is_ascii_digit())
                    && n.parse::<u32>().is_ok_and(|n| n > 0)
            }))
}

fn property(value: &Value, name: &str) -> Result<Value, Fault> {
    match value {
        Value::Object(o) | Value::ReadOnlyObject(o) | Value::Array(o) => {
            Ok(o.borrow().get(name).cloned().unwrap_or(Value::Null))
        }
        _ => Err("Exchange settings must be an object".into()),
    }
}
fn escaped(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn directory(base: &Path, name: &str) -> Result<PathBuf, Fault> {
    if name.trim().is_empty() {
        return Err("Не задан каталог обмена".into());
    }
    let path = base.join(super::super::path::script_path(name));
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("Каталог обмена содержит ..".into());
    }
    let path = path
        .canonicalize()
        .map_err(|e| format!("Каталог обмена {}: {e}", path.display()))?;
    if !path.is_dir() {
        return Err("Путь обмена не является каталогом".into());
    }
    Ok(path)
}
impl Runtime<'_> {
    pub(super) fn exchange_auto_court(&mut self, values: &[Value]) -> Result<Value, Fault> {
        if !self.exchange_access.scheduled || values.len() != 3 {
            return Err("Automatic court transfer requires a scheduled job".into());
        }
        let Value::Function(function) = self.get("autosend_court")? else {
            return Err("Automatic court callback missing".into());
        };
        let result = self.invoke(function, values.to_vec(), None, "autosend_court".into());
        if let Err(error) = result {
            // A cancelled job must stop. A failed destination must not prevent
            // the stock loop from sending to the remaining configured courts.
            if crate::execution::cancelled() { return Err(error); }
            let message = format!("court={}: {error}", values[0].text());
            let diagnostic = self.take_error_diagnostic();
            crate::runtime_logs::error_detail("scheduled court exchange", &message, &diagnostic);
            self.exchange_access.abort();
            self.exchange_access.auto_errors.push(message);
        }
        Ok(Value::Null)
    }
    pub(super) fn exchange_auto_finish(&mut self) -> Result<Value, Fault> {
        if !self.exchange_access.scheduled {
            return Err("Automatic exchange summary requires a scheduled job".into());
        }
        if self.exchange_access.auto_errors.is_empty() { return Ok(Value::Null); }
        Err(format!("Ошибки автоматической отправки: {}",
            std::mem::take(&mut self.exchange_access.auto_errors).join("; ")).into())
    }
    pub(super) fn exchange_xml_limit(&self) -> usize {
        if self.exchange_access.work.is_some() {
            64 * 1024 * 1024
        } else {
            4 * 1024 * 1024
        }
    }
    fn exchange_job(&self) -> bool {
        if self.exchange_access.scheduled { return true; }
        if !self.scheduled_request || self.path.trim_end_matches('/') != "/20a" {
            return false;
        }
        let Ok(Value::Object(p)) = self.get("mp_g_ascJobPrincipal") else {
            return false;
        };
        let p = p.borrow();
        let name = p.get("sJobUrlName").map(Value::text).unwrap_or_default();
        matches!(
            name.to_ascii_lowercase().as_str(),
            "~senddocsauto" | "~senddocsautoget"
        )
    }
    pub(super) fn exchange_begin_write(&mut self) -> Result<Value, Fault> {
        if !self.exchange_access.request && !self.exchange_job() {
            return Err("Изменения обмена требуют авторизованной отправки формы POST".into());
        }
        self.document_write_allowed = true;
        Ok(Value::Null)
    }
    pub(super) fn exchange_init(&mut self, values: &[Value]) -> Result<Value, Fault> {
        if !(1..=2).contains(&values.len()) {
            return Err("Exchange init expects exchanger[, court]".into());
        }
        let exchange = &values[0];
        let selected = values
            .get(1)
            .filter(|v| !v.text().is_empty())
            .map(Value::text);
        self.exchange_begin_write()?;
        let settings = property(exchange, "settings")?;
        let single = property(&settings, "settings")?;
        let transport = property(&single, "nm_exchange_type")?.text();
        if !matches!(transport.as_str(), "file" | "email" | "ftp") {
            return Err("Неизвестный транспорт обмена".into());
        }
        self.exchange_access.packet_size =
            if transport == "email" && property(&single, "nm_allow_packet")?.text() == "1" {
                let kib = property(&single, "nm_packet_size")?
                    .text()
                    .parse::<usize>()
                    .map_err(|_| "Неверный размер почтовой части")?;
                if !(1..=131072).contains(&kib) {
                    return Err("Размер почтовой части должен быть от 1 до 131072 КиБ".into());
                }
                Some(kib * 1024)
            } else {
                None
            };
        let base = Path::new(
            self.data_root
                .as_deref()
                .ok_or("Exchange data root missing")?,
        )
        .canonicalize()
        .map_err(|e| e.to_string())?;
        let root = property(exchange, "rootPath")?.text();
        if !matches!(root.as_str(), "swapsend" | "swapget") {
            return Err("Unexpected exchange work root".into());
        }
        let archiver = archive::find_archiver(&base)?;
        let inbox = if root == "swapget" && transport == "file" {
            Some(directory(&base, &property(&single, "popserver")?.text())?)
        } else {
            None
        };
        let courts = property(&settings, "courts")?;
        let count = property(&courts, "length")?.number()? as usize;
        let mut destinations = Vec::new();
        let mut recipients = Vec::new();
        for i in 0..count {
            if root == "swapget" {
                break;
            }
            let court = property(&courts, &i.to_string())?;
            if selected.as_ref().is_some_and(|id| {
                property(&court, "oid")
                    .map(|v| v.text() != *id)
                    .unwrap_or(true)
            }) {
                continue;
            }
            let name = property(&court, "email")?.text();
            if !name.is_empty() {
                if transport == "file" {
                    destinations.push(directory(&base, &name)?);
                } else {
                    recipients.push(name);
                }
            }
        }
        if root == "swapsend" && destinations.is_empty() && recipients.is_empty() {
            return Err("Не задан доступный каталог выбранного суда".into());
        }
        let parent = base.join(&root);
        std::fs::create_dir_all(&parent).map_err(|e| e.to_string())?;
        let parent = parent.canonicalize().map_err(|e| e.to_string())?;
        if parent.parent() != Some(base.as_path()) {
            return Err("Exchange work directory escapes data root".into());
        }
        let name = format!(
            "codex-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        );
        let work = parent.join(&name);
        std::fs::create_dir(&work).map_err(|e| e.to_string())?;
        let backup = base.join("BackUp");
        std::fs::create_dir_all(&backup).map_err(|e| e.to_string())?;
        let backup = backup.canonicalize().map_err(|e| e.to_string())?;
        if backup.parent() != Some(base.as_path()) {
            return Err("Exchange backup directory escapes data root".into());
        }
        let mut roots = destinations.clone();
        roots.extend(inbox);
        roots.extend([work.clone(), backup.clone()]);
        self.exchange_access.backup = Some(backup);
        self.exchange_access.network = network::State::default();
        if transport != "file" {
            self.exchange_access.network.config =
                Some(network::configure(&transport, &single, recipients)?);
        }
        self.exchange_access.work = Some(work.clone());
        self.exchange_access.roots = roots;
        self.exchange_access.destinations = destinations;
        self.exchange_access.archiver = Some(archiver);
        self.exchange_access.transfer_id = format!(
            "{}-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos(),
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        );
        if transport == "email" {
            self.assign_member(
                exchange.clone(),
                "sendId".into(),
                Value::String(self.exchange_access.transfer_id.clone()),
            )?;
        }
        for (key, value) in [
            ("path", format!("{root}/{name}")),
            (
                "fullPath",
                super::super::path::script_path(&work.to_string_lossy()),
            ),
            ("zip", "7za.exe".into()),
            ("packer", "packer.exe".into()),
            ("zipExt", ".7z".into()),
            ("packerExt", ".pkg".into()),
        ] {
            self.assign_member(exchange.clone(), key.into(), Value::String(value))?;
        }
        Ok(Value::Null)
    }
    pub(super) fn exchange_path(
        &self,
        name: &str,
        file_write: bool,
    ) -> Result<Option<PathBuf>, Fault> {
        if self.exchange_access.work.is_none() {
            return Ok(None);
        }
        let base = Path::new(
            self.data_root
                .as_deref()
                .ok_or("Exchange data root missing")?,
        );
        let name = super::super::path::script_path(name);
        if Path::new(&name)
            .components()
            .any(|c| matches!(c, Component::ParentDir))
        {
            return Err("Exchange path contains parent traversal".into());
        }
        let path = base.join(name);
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("Exchange path contains parent traversal".into());
        }
        for root in &self.exchange_access.roots {
            let spelling = PathBuf::from(super::super::path::script_path(&root.to_string_lossy()));
            let Ok(relative) = path.strip_prefix(&spelling) else {
                continue;
            };
            let mut checked = root.clone();
            for component in relative.components() {
                if component == Component::CurDir {
                    continue;
                }
                let Component::Normal(part) = component else {
                    return Err("Invalid exchange path component".into());
                };
                checked.push(part);
                match std::fs::symlink_metadata(&checked) {
                    Ok(meta) => {
                        if meta.file_type().is_symlink() {
                            return Err("Exchange symlinks are not supported".into());
                        }
                        let resolved = checked.canonicalize().map_err(|e| e.to_string())?;
                        if !resolved.starts_with(root) {
                            return Err("Exchange path escapes configured directory".into());
                        }
                        checked = resolved;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                    Err(e) => return Err(e.to_string().into()),
                }
            }
            if file_write
                && !checked.extension().is_some_and(|e| {
                    ["xml", "7z", "pkg"]
                        .iter()
                        .any(|s| e.eq_ignore_ascii_case(s))
                })
                && !checked
                    .file_name()
                    .is_some_and(|n| packet_name(&n.to_string_lossy()))
            {
                return Err("Exchange files must be XML, 7z or pkg".into());
            }
            return Ok(Some(checked));
        }
        Ok(None)
    }
    pub(super) fn exchange_cleanup(&mut self) -> Result<Value, Fault> {
        let network_result = network::finish(&mut self.exchange_access);
        let files_result = self.exchange_access.cleanup_files();
        network_result?;
        files_result?;
        Ok(Value::Null)
    }
    pub(super) fn exchange_pack(&mut self, values: &[Value]) -> Result<Value, Fault> {
        let oid = values.first().ok_or("Exchange pack expects case OID")?;
        let extension = values
            .get(1)
            .map(Value::text)
            .unwrap_or_else(|| ".7z".into());
        let oid = oid.number()?;
        if oid < 1.0 || oid > u32::MAX as f64 || oid.fract() != 0.0 {
            return Err("Invalid exchange case OID".into());
        }
        archive::pack(&self.exchange_access, oid as u32, &extension)?;
        Ok(Value::Null)
    }
    pub(super) fn exchange_unpack(&mut self, values: &[Value]) -> Result<Value, Fault> {
        let [name] = values else {
            return Err("Exchange unpack expects archive name".into());
        };
        archive::unpack(&self.exchange_access, &name.text())?;
        Ok(Value::Null)
    }
    pub(super) fn exchange_deliver(&self, values: &[Value]) -> Result<Value, Fault> {
        let [from, to] = values else {
            return Err("Exchange delivery expects two paths".into());
        };
        let source = self
            .exchange_path(&from.text(), false)?
            .ok_or("Exchange source outside work directory")?;
        let target = self
            .exchange_path(&to.text(), true)?
            .ok_or("Exchange destination not configured")?;
        if source.parent() != self.exchange_access.work.as_deref()
            || !self
                .exchange_access
                .destinations
                .iter()
                .any(|p| Some(p.as_path()) == target.parent())
        {
            return Err(
                "Exchange delivery must copy from workspace to a configured court directory".into(),
            );
        }
        archive::deliver(&source, &target)?;
        Ok(Value::Bool(true))
    }
    pub(super) fn exchange_auto_parse(&mut self, values: &[Value]) -> Result<Value, Fault> {
        let [Value::ReadStream(stream)] = values else {
            return Err("Exchange automatic settings require XML stream".into());
        };
        let input = stream.borrow();
        if input.closed {
            return Err("XML stream closed".into());
        }
        let source = crate::db::decode(&input.data[input.position..]);
        drop(input);
        let mut result = HashMap::new();
        let mut court = None;
        let mut depth = 0;
        for event in xml_sax::events(&source)? {
            match event {
                xml_sax::Event::Start(name, attrs) => {
                    depth += 1;
                    let attrs: HashMap<_, _> = attrs.into_iter().collect();
                    if depth == 1
                        && (name != "settings"
                            || attrs.get("xmlversion").map(String::as_str) != Some("1.2"))
                    {
                        return Err("Unsupported exchange settings XML version".into());
                    }
                    if depth == 2 && name == "court" {
                        court = attrs.get("oid").cloned();
                        if let Some(id) = &court {
                            result.insert(
                                id.clone(),
                                Value::Object(Rc::new(RefCell::new(HashMap::new()))),
                            );
                        }
                    }
                    if depth == 3 {
                        if let (Some(id), Some(value)) = (&court, attrs.get("value")) {
                            if let Some(Value::Object(o)) = result.get(id) {
                                o.borrow_mut().insert(name, Value::String(value.clone()));
                            }
                        }
                    }
                }
                xml_sax::Event::End(_) => {
                    if depth == 2 {
                        court = None;
                    }
                    depth -= 1;
                }
                _ => (),
            }
        }
        Ok(Value::Object(Rc::new(RefCell::new(result))))
    }
    pub(super) fn exchange_auto_xml(&self, values: &[Value]) -> Result<Value, Fault> {
        let [Value::Object(settings)] = values else {
            return Err("Exchange automatic settings require object".into());
        };
        let mut xml = String::from("<settings xmlversion=\"1.2\">");
        let settings = settings.borrow();
        let mut ids = settings.keys().collect::<Vec<_>>();
        ids.sort();
        for id in ids {
            let Value::Object(row) = &settings[id] else {
                return Err("Invalid court settings row".into());
            };
            xml.push_str(&format!("<court oid=\"{}\">", escaped(id)));
            let row = row.borrow();
            let mut keys = row.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                if !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') || key.is_empty() {
                    return Err("Invalid automatic setting name".into());
                }
                xml.push_str(&format!("<{key} value=\"{}\"/>", escaped(&row[key].text())));
            }
            xml.push_str("</court>");
        }
        xml.push_str("</settings>");
        xml_ole::from_source(xml, false)
    }
}

