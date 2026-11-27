//! KS file access with scoped log, deletion archive and attachment writes.
use super::*;
use std::path::{Component, Path, PathBuf};

#[derive(Debug)]
pub(super) struct WriteFile {
    pub name: String,
    pub charset: RefCell<String>,
    pub file: Rc<RefCell<Option<std::fs::File>>>,
    log_path: Option<PathBuf>,
    generation: std::cell::Cell<u64>,
}

#[cfg(test)]
mod log_tests {
    use super::*;
    #[test]
    fn xml_writer_can_append_fields_before_the_closing_root() {
        let path=std::env::temp_dir().join(format!("codex-xml-seek-{}",std::process::id()));
        let file=std::fs::OpenOptions::new().read(true).write(true).create_new(true).open(&path).unwrap();
        let mut runtime=Runtime::new("/test");
        runtime.scopes[0].borrow_mut().insert("f".into(),Value::LogFile(WriteFile::new("exchange.xml",file)));
        runtime.render(&super::super::parser::parse("f.Print('<root></root>'); f.reset(); f.skip(f.length()-7); var w=new XMLBaseTagWriter(f); w.Element('field',{value:'Суд & файл'}); f.Print('</root>'); f.close();").unwrap()).unwrap();
        let text=crate::db::decode(&std::fs::read(&path).unwrap());
        assert_eq!(text,"<root><field value=\"Суд &amp; файл\"></field></root>");
        super::super::xml_sax::events(&text).unwrap();
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn writable_file_charset_controls_encoded_bytes() {
        let path=std::env::temp_dir().join(format!("codex-charset-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        let file=std::fs::File::create(&path).unwrap();
        let mut runtime=Runtime::new("/test");
        runtime.scopes[0].borrow_mut().insert("f".into(),Value::LogFile(WriteFile::new("test.log",file)));
        runtime.render(&super::super::parser::parse("f.CharSet='windows-1251'; f.PrintS('Тест'); if (f.lastModified() <= 0 || f.isDirectory()) throw 'bad file probe'; f.CharSet='utf-8'; f.Print('Тест'); f.close();").unwrap()).unwrap();
        let mut expected=encoding_rs::WINDOWS_1251.encode("Тест").0.into_owned();
        expected.extend_from_slice("Тест".as_bytes());
        assert_eq!(std::fs::read(&path).unwrap(),expected);
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn ks_log_handle_reopens_after_daily_rotation() {
        let root=std::env::temp_dir().join(format!("codex-ks-logs-{}-{}",std::process::id(),std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(root.join(crate::runtime_logs::DIRECTORY)).unwrap();
        let root=root.canonicalize().unwrap();let logs=root.join(crate::runtime_logs::DIRECTORY);
        let mut runtime=Runtime::new("/test").with_data_root(root.to_str().unwrap());
        runtime.log_root=Some(logs.clone());
        let ast=super::super::parser::parse("var f=new File('Log/test.log', true); f.PrintS('before');").unwrap();
        runtime.render(&ast).unwrap();
        crate::runtime_logs::rotate_for_test(&logs,"2026-11-20","2026-11-21");
        runtime.render(&super::super::parser::parse("f.PrintS('after'); f.close();").unwrap()).unwrap();
        assert_eq!(std::fs::read(logs.join("test.log")).unwrap(),b"after");
        let segment=std::fs::read_dir(logs.join("OLD_Log")).unwrap().next().unwrap().unwrap().path();
        assert_eq!(std::fs::read(segment.join("test.log")).unwrap(),b"before");
        assert!(!root.join("Log").exists());
    }
}
impl WriteFile {
    fn new(name: &str, file: std::fs::File) -> Rc<Self> {
        Rc::new(Self { name: name.into(), charset: RefCell::new("windows-1251".into()), file: Rc::new(RefCell::new(Some(file))), log_path: None, generation: std::cell::Cell::new(0) })
    }
    fn log(name: &str, path: PathBuf, file: std::fs::File) -> Rc<Self> {
        Rc::new(Self { name: name.into(), charset: RefCell::new("windows-1251".into()), file: Rc::new(RefCell::new(Some(file))), log_path: Some(path), generation: std::cell::Cell::new(crate::runtime_logs::generation()) })
    }
    pub fn lock_log(&self) -> Result<Option<std::sync::MutexGuard<'static, ()>>, String> {
        let Some(path) = &self.log_path else { return Ok(None) };
        let guard = crate::runtime_logs::write_guard();
        if self.generation.get() != crate::runtime_logs::generation() && self.file.borrow().is_some() {
            *self.file.borrow_mut() = Some(std::fs::OpenOptions::new().read(true).append(true).create(true).open(path).map_err(|e|e.to_string())?);
            self.generation.set(crate::runtime_logs::generation());
        }
        Ok(Some(guard))
    }
    pub fn is_log(&self) -> bool { self.log_path.is_some() }
}

#[derive(Debug)]
pub(super) struct ReadFile {
    pub path: PathBuf,
    pub charset: RefCell<String>,
    pub stream: RefCell<Option<Rc<RefCell<InputStream>>>>,
}
impl ReadFile {
    pub fn new(path: PathBuf) -> Rc<Self> {
        Rc::new(Self { path, charset: RefCell::new("windows-1251".into()), stream: RefCell::new(None) })
    }
}

fn script_io(operation: &str, path: &Path, error: std::io::Error) -> Fault {
    Fault::Script(Value::String(format!(
        "{operation} {}: {error}",
        path.display()
    )))
}

// Match paths before touching the source filesystem: data/log and its parents
// need not exist. Parent components are resolved before deciding permissions.
fn log_relative(root: &Path, name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    let canonical_root = root;
    let root = PathBuf::from(super::super::path::script_path(&root.to_string_lossy()));
    let path = root.join(name.replace('\\', "/"));
    let mut normalized = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(part.as_os_str()),
        }
    }
    // SPD's DebugLog opens time.log at the data root. Redirect root-level
    // journals too, without ever opening the original file for writing.
    if normalized.parent() == Some(root.as_path())
        && normalized
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("log"))
    {
        return Some(PathBuf::from(format!(
            "root-{}",
            normalized.file_name()?.to_string_lossy()
        )));
    }
    let direct = (|| {
        let mut components = normalized.components();
        for expected in root.components() {
            let actual = components.next()?;
            let equal = if cfg!(windows) {
                actual.as_os_str().to_string_lossy().to_lowercase()
                    == expected.as_os_str().to_string_lossy().to_lowercase()
            } else {
                actual == expected
            };
            if !equal {
                return None;
            }
        }
        // The KS namespace spells this logical directory both "log" and
        // "Log". Redirect either spelling on Unix too, without making the
        // actual data-root prefix or journal filenames case-insensitive.
        if !matches!(components.next()?, Component::Normal(n) if n.eq_ignore_ascii_case("log") || n.eq_ignore_ascii_case(crate::runtime_logs::DIRECTORY)) {
            return None;
        }
        Some(components.collect())
    })();
    if direct.is_some() {
        return direct;
    }
    #[cfg(windows)]
    {
        // An absolute KS path can use an 8.3 spelling while data_root uses
        // the canonical long name. Resolve only the existing data prefix;
        // the logical log directory itself may not exist and is redirected.
        let components: Vec<_> = normalized.components().collect();
        let mut parent = PathBuf::new();
        for (i, component) in components.iter().enumerate() {
            if matches!(component, Component::Normal(n) if n.eq_ignore_ascii_case("log") || n.eq_ignore_ascii_case(crate::runtime_logs::DIRECTORY))
                && parent.canonicalize().ok().as_deref() == Some(canonical_root)
            {
                return Some(components[i + 1..].iter().collect());
            }
            parent.push(component.as_os_str());
        }
    }
    None
}

impl Runtime<'_> {
    // The vendor deletion form saves .kod recovery files in this one subtree.
    // Validate each existing ancestor, including junctions, before creating it.
    fn deletion_destination(&self, root: &Path, name: &str) -> Result<Option<PathBuf>, Fault> {
        if !self.document_write_allowed {
            return Ok(None);
        }
        let path = root.join(name.replace('\\', "/"));
        let components = path.components().collect::<Vec<_>>();
        if components.iter().any(|p| matches!(p, Component::ParentDir)) {
            return Err("Deletion archive path traversal".into());
        }
        // createPath passes an absolute KS spelling (C:/...), whereas Windows
        // canonicalize returns a verbatim path (\\?\C:\...). Resolve the
        // existing data prefix, also accepting its native 8.3 spelling.
        let mut parent = PathBuf::new();
        let mut suffix = None;
        for (i, part) in components.iter().enumerate() {
            if matches!(part, Component::Normal(p) if p.eq_ignore_ascii_case("DelObject"))
                && parent.canonicalize().ok().as_deref() == Some(root)
            {
                suffix = Some(&components[i + 1..]);
                break;
            }
            parent.push(part.as_os_str());
        }
        let Some(parts) = suffix else {
            return Ok(None);
        };
        let mut destination = root.join("DelObject");
        for part in std::iter::once(None).chain(parts.iter().copied().map(Some)) {
            if let Some(part) = part {
                let Component::Normal(p) = part else {
                    return Err("Deletion archive path traversal".into());
                };
                destination.push(p);
            }
            if std::fs::symlink_metadata(&destination).is_ok() {
                let resolved = destination
                    .canonicalize()
                    .map_err(|e| script_io("Deletion archive", &destination, e))?;
                if !resolved.starts_with(root.join("DelObject")) {
                    return Err("Deletion archive outside data root".into());
                }
            }
        }
        Ok(Some(destination))
    }
    fn file_root(&self) -> Result<PathBuf, Fault> {
        Path::new(self.data_root.as_ref().ok_or("File requires data root")?)
            .canonicalize()
            .map_err(|e| Fault::Host(format!("File data root: {e}")))
    }

    fn log_destination(&self, root: &Path, relative: &Path) -> Result<PathBuf, Fault> {
        let configured = self.log_root.as_ref().ok_or(
            "KODEKS_LOG_DIR is required for KS log writes; use a separate existing directory",
        )?;
        let log_root = configured
            .canonicalize()
            .map_err(|e| format!("KODEKS_LOG_DIR {}: {e}", configured.display()))?;
        if !log_root.is_dir() {
            return Err("KODEKS_LOG_DIR must be a directory".into());
        }
        crate::runtime_logs::validate_root(root, &log_root)?;
        // Server journals own these two names. A vendor KS file must never
        // truncate them through new File("Log/error.log", true).
        let reserved = relative.to_string_lossy();
        let renamed;
        let relative = if reserved.eq_ignore_ascii_case("error.log") || reserved.eq_ignore_ascii_case("access.log") {
            renamed = PathBuf::from(format!("ks-{}", reserved.to_ascii_lowercase()));
            renamed.as_path()
        } else { relative };
        let mut destination = log_root.clone();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                return Err("log path must stay within KODEKS_LOG_DIR".into());
            };
            destination.push(name);
            match std::fs::symlink_metadata(&destination) {
                Ok(_) => {
                    let resolved = destination
                        .canonicalize()
                        .map_err(|e| script_io("log path", &destination, e))?;
                    if !resolved.starts_with(&log_root) {
                        return Err(format!(
                            "log path outside configured directory: {}",
                            destination.display()
                        )
                        .into());
                    }
                    destination = resolved;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(script_io("log path", &destination, e)),
            }
        }
        Ok(destination)
    }

    fn schedule_destination(&self, root: &Path, name: &str) -> Result<Option<PathBuf>, Fault> {
        if self.schedule_export_allowed {
            // XMLpath comes from the configured catalog snapshot, never from
            // submitted form fields. Only the native daily schedule is writable.
            let configured = self.request_baseline.as_ref()
                .and_then(|(_,_,settings)| settings.get("XMLpath")).map(Value::text)
                .filter(|s| !s.is_empty());
            if let Some(configured) = configured {
                let directory = root.join(configured.replace('\\', "/"));
                let requested = root.join(name.replace('\\', "/"));
                let leaf = requested.file_name().and_then(|s|s.to_str()).unwrap_or("");
                if leaf.len() == 11 && leaf.starts_with("sud") && leaf.ends_with(".xml")
                    && leaf.as_bytes()[3..7].iter().all(u8::is_ascii_digit)
                    && !requested.components().any(|p|matches!(p,Component::ParentDir)) {
                    let directory = directory.canonicalize().map_err(|e|script_io("XMLpath",&directory,e))?;
                    let parent = requested.parent().ok_or("schedule file has no parent")?
                        .canonicalize().map_err(|e|script_io("Schedule",&requested,e))?;
                    if parent == directory && directory.is_dir() {
                        let destination = directory.join(leaf);
                        if let Ok(meta) = std::fs::symlink_metadata(&destination) {
                            if !meta.is_file() || meta.file_type().is_symlink()
                                || destination.canonicalize().map_err(|e|script_io("Schedule",&destination,e))?.parent() != Some(directory.as_path()) {
                                return Err("Schedule destination is not a regular file".into());
                            }
                        }
                        return Ok(Some(destination));
                    }
                }
            }
        }
        Ok(None)
    }

    pub(super) fn open_file(&self, name: &str, create: bool) -> Result<Value, Fault> {
        let root = self.file_root()?;
        if let Some(path) = self.exchange_path(name, create)? {
            if create {
                let file=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true)
                    .open(&path).map_err(|e|script_io("Exchange",&path,e))?;
                return Ok(Value::LogFile(WriteFile::new(name,file)));
            }
            std::fs::metadata(&path).map_err(|e|script_io("Exchange",&path,e))?;
            return Ok(Value::File(ReadFile::new(path)));
        }
        if create {
            if let Some(destination) = self.schedule_destination(&root, name)? {
                let file = std::fs::OpenOptions::new().read(true).write(true)
                    .create(true).truncate(true).open(&destination)
                    .map_err(|e|script_io("Schedule",&destination,e))?;
                return Ok(Value::LogFile(WriteFile::new(name, file)));
            }
        }
        if let Some(path) = self.attachment_destination(name)? {
            if create {
                if !self.attachment_write_allowed { return Err("attachment writes require authenticated registration".into()); }
                let file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true)
                    .open(&path).map_err(|e| script_io("Attachment",&path,e))?;
                return Ok(Value::LogFile(WriteFile::new(name, file)));
            }
            let _ = std::fs::metadata(&path).map_err(|e|script_io("Attachment",&path,e))?;
            return Ok(Value::File(ReadFile::new(path)));
        }
        if create {
            if let Some(path) = self.deletion_destination(&root, name)? {
                if !path
                    .extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("kod"))
                {
                    return Err("Deletion archive requires .kod".into());
                }
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&path)
                    .map_err(|e| script_io("Deletion archive", &path, e))?;
                return Ok(Value::LogFile(WriteFile::new(name, file)));
            }
        }
        if let Some(relative) = log_relative(&root, name) {
            if self.log_root.is_some() || create {
                let _guard = crate::runtime_logs::write_guard();
                let destination = self.log_destination(&root, &relative)?;
                // Probes and listings refer to the same directory as log writes.
                // No source log directory is required, and none is created.
                if !create {
                    let meta = std::fs::metadata(&destination)
                        .map_err(|e| script_io("log File", &destination, e))?;
                    if meta.is_dir() {
                        return Ok(Value::File(ReadFile::new(destination)));
                    }
                }
                if !relative
                    .extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("log"))
                {
                    return Err(format!("log File supports only .log files: {name}").into());
                }
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(create)
                    .truncate(create)
                    .open(&destination)
                    .map_err(|e| script_io("log File", &destination, e))?;
                return Ok(Value::LogFile(WriteFile::log(name, destination, file)));
            }
        }
        if create {
            return Err(format!(
                "File creation outside configured log directory is not implemented: {name}"
            )
            .into());
        }
        let path = root.join(name.replace('\\', "/"));
        let path = path
            .canonicalize()
            .map_err(|e| script_io("File", &path, e))?;
        if !path.starts_with(&root) {
            if root.starts_with(&path) && path.is_dir() {
                return Ok(Value::AncestorDirectory(path));
            }
            return Err(Fault::Script(Value::String(format!(
                "File outside data root: resolved={}, root={}",
                path.display(),
                root.display()
            ))));
        }
        Ok(Value::File(ReadFile::new(path)))
    }

    pub(super) fn log_path_operation(
        &self,
        operation: &str,
        values: &[Value],
    ) -> Result<Value, Fault> {
        let [value] = values else {
            return Err(format!("{operation} expects one path").into());
        };
        if !matches!(value, Value::String(_) | Value::FilePath(_)) {
            return Err(format!("{operation} requires a string or FilePath").into());
        }
        let name = value.text();
        let root = self.file_root()?;
        if let Some(path) = self.exchange_path(&name, operation == "Unlink")? {
            let result=match operation {
                "MkDir"=>std::fs::create_dir(&path),
                "Unlink"=>std::fs::remove_file(&path),
                "RmDir"=>std::fs::remove_dir(&path),
                _=>unreachable!(),
            };
            return Ok(Value::Bool(result.is_ok()));
        }
        if operation == "Unlink" {
            if let Some(path) = self.schedule_destination(&root, &name)? {
                return Ok(Value::Bool(std::fs::remove_file(path).is_ok()));
            }
        }
        if let Some(path) = self.attachment_destination(&name)? {
            if !self.attachment_write_allowed { return Err("attachment changes require authenticated registration".into()); }
            let result = match operation {
                "MkDir" => std::fs::create_dir(&path),
                "Unlink" => std::fs::remove_file(&path),
                "RmDir" => std::fs::remove_dir(&path),
                _ => unreachable!(),
            };
            return Ok(Value::Bool(result.is_ok()));
        }
        if operation == "MkDir" {
            if let Some(path) = self.deletion_destination(&root, &name)? {
                return Ok(Value::Bool(std::fs::create_dir(&path).is_ok()));
            }
        }
        let relative = log_relative(&root, &name).ok_or_else(|| {
            format!(
                "{operation} is only supported inside data_root/log; requested={name}; root={}",
                root.display()
            )
        })?;
        let destination = self.log_destination(&root, &relative)?;
        let _guard = crate::runtime_logs::write_guard();
        let result = match operation {
            "MkDir" => std::fs::create_dir(&destination),
            "RmDir" => std::fs::remove_dir(&destination),
            "Unlink" => {
                if !relative
                    .extension()
                    .is_some_and(|s| s.eq_ignore_ascii_case("log"))
                {
                    return Err(format!("Unlink is only supported for .log files: {name}").into());
                }
                std::fs::remove_file(&destination)
            }
            _ => unreachable!(),
        };
        drop(_guard);
        // Native MkDir wraps mkdir and returns false on failure (00675fc6).
        // A missing Unlink target is likewise a normal unsuccessful operation.
        if let Err(ref e) = result {
            if !matches!(
                e.kind(),
                std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotFound
            ) {
                crate::runtime_logs::error("KS log operation", &format!("{operation} {name} -> {}: {e}", destination.display()));
            }
        }
        Ok(Value::Bool(result.is_ok()))
    }
}
