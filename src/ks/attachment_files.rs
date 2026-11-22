//! Filesystem operations used by the native registration attachment scripts.
//! Shell.Execute is interpreted as a small file-operation grammar, never a shell.
use super::*;
use std::path::{Component, Path, PathBuf};

fn resolved(path: &Path) -> Result<PathBuf, Fault> {
    if std::fs::symlink_metadata(path).is_ok() {
        return path
            .canonicalize()
            .map_err(|e| Fault::Script(Value::String(e.to_string())));
    }
    let parent = path.parent().ok_or("attachment path has no parent")?;
    Ok(resolved(parent)?.join(path.file_name().ok_or("attachment path has no name")?))
}

impl Runtime<'_> {
    pub(super) fn attachment_temp_base(&self) -> Result<PathBuf, Fault> {
        Ok(Path::new(
            self.data_root
                .as_deref()
                .ok_or("attachment data root missing")?,
        )
        .join(".codex-upload-temp"))
    }

    fn attachment_roots(&self) -> Result<Vec<PathBuf>, Fault> {
        let data = Path::new(
            self.data_root
                .as_deref()
                .ok_or("attachment data root missing")?,
        );
        let registry = self
            .registry
            .as_ref()
            .ok_or("attachment registry missing")?;
        let mut roots = vec![self.attachment_temp_base()?];
        let mut keys = vec![registry.root];
        // Settings are inherited from Web and overridden by the active catalog.
        for &key in &registry.key(registry.root)?.children {
            if registry.query(key, "Path")?.is_some_and(|e| {
                e.value
                    .trim_end_matches('/')
                    .eq_ignore_ascii_case(self.path.trim_end_matches('/'))
            }) {
                keys.push(key);
            }
        }
        for &key in &keys {
            if let Some(e) = registry.query(key, "FileDocPath")? {
                if !e.value.trim().is_empty() {
                    roots.push(data.join(e.value.replace('\\', "/")));
                }
            }
        }
        // Monthly repositories and the optional upload staging directory.
        let basic_name = self.get("REG_DOC_BASICPATH_NAME")?.text();
        let mut pending = keys
            .into_iter()
            .flat_map(|key| registry.key(key).unwrap().children.clone())
            .collect::<Vec<_>>();
        let mut seen = std::collections::HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let key = registry.key(id)?;
            for value in &key.values {
                if !value.value.trim().is_empty()
                    && (value.name == "Путь к временной папке для хранения файлов"
                        || !basic_name.is_empty() && value.name == basic_name)
                {
                    roots.push(data.join(value.value.replace('\\', "/")));
                }
            }
            pending.extend(&key.children);
        }
        let data = data.canonicalize().map_err(|e| e.to_string())?;
        let mut allowed = Vec::new();
        for root in roots {
            let root = resolved(&root)?;
            // A repository cannot grant writes to the entire data root or disk.
            if root.parent().is_none() || data.starts_with(&root) {
                return Err("attachment repository overlaps data root".into());
            }
            if root.is_file() {
                return Err("attachment repository is not a directory".into());
            }
            if !allowed.contains(&root) {
                allowed.push(root);
            }
        }
        Ok(allowed)
    }

    pub(super) fn attachment_destination(&self, name: &str) -> Result<Option<PathBuf>, Fault> {
        if self.registry.is_none() {
            return Ok(None);
        }
        let spelling = super::super::path::script_path(name);
        let name = spelling.as_str();
        if name.contains('\0')
            || name
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '*' | '?'))
        {
            return Err("invalid attachment path".into());
        }
        let data = Path::new(
            self.data_root
                .as_deref()
                .ok_or("attachment data root missing")?,
        );
        let path = data.join(name.replace('\\', "/"));
        if path.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err("attachment path traversal".into());
        }
        for c in path.components() {
            if let Component::Normal(s) = c {
                let s = s.to_string_lossy();
                if s.contains(':') || s.ends_with([' ', '.']) {
                    return Err("invalid attachment path component".into());
                }
                let base = s.split('.').next().unwrap_or("").to_ascii_uppercase();
                if matches!(base.as_str(), "CON" | "NUL" | "PRN" | "AUX")
                    || (base.starts_with("COM") || base.starts_with("LPT"))
                        && base.len() == 4
                        && matches!(base.as_bytes()[3], b'1'..=b'9')
                {
                    return Err("attachment device name is not permitted".into());
                }
            }
        }
        let path = resolved(&path)?;
        Ok(self
            .attachment_roots()?
            .into_iter()
            .any(|base| path.starts_with(base))
            .then_some(path))
    }

    pub(super) fn attachment_shell(&self, values: &[Value]) -> Result<Value, Fault> {
        if !self.attachment_write_allowed {
            return Err("attachment operations require authenticated registration".into());
        }
        let command = values
            .first()
            .ok_or("Shell.Execute expects a command")?
            .text();
        if values.len() > 6 {
            return Err("Shell.Execute arguments".into());
        }
        // Parse quoted file operands; reject command chaining and shell expansion.
        let mut words = Vec::new();
        let mut remaining = command.trim();
        while !remaining.is_empty() {
            if let Some(quoted) = remaining.strip_prefix('"') {
                let (word, tail) = quoted.split_once('"').ok_or("unclosed file operand")?;
                words.push(word.to_string());
                if !tail.is_empty() && !tail.starts_with(char::is_whitespace) {
                    return Err("invalid file operand".into());
                }
                remaining = tail.trim_start();
            } else {
                let end = remaining
                    .find(char::is_whitespace)
                    .unwrap_or(remaining.len());
                words.push(remaining[..end].to_string());
                remaining = remaining[end..].trim_start();
            }
        }
        let get = |name: &str| {
            self.attachment_destination(name)?
                .ok_or_else(|| Fault::Host("file operation outside attachment repositories".into()))
        };
        let operation = words
            .first()
            .map(|s| s.to_ascii_lowercase())
            .unwrap_or_default();
        match (operation.as_str(), words.as_slice()) {
            ("copy", [_, binary, overwrite, source, target]) if binary.eq_ignore_ascii_case("/b") && overwrite.eq_ignore_ascii_case("/y") => {
                let strip = |s: &str| s.strip_suffix("\\*.*").or_else(|| s.strip_suffix("/*.*")).map(str::to_owned).ok_or("attachment copy expects a directory mask");
                let source = get(&strip(source)?)?; let target = get(&strip(target)?)?;
                std::fs::create_dir_all(&target).map_err(|e| e.to_string())?;
                for entry in std::fs::read_dir(&source).map_err(|e| e.to_string())? {
                    crate::execution::check()?;
                    let entry=entry.map_err(|e|e.to_string())?;
                    if !entry.file_type().map_err(|e|e.to_string())?.is_file() { continue; }
                    let from=get(&entry.path().to_string_lossy())?;
                    let to=get(&target.join(entry.file_name()).to_string_lossy())?;
                    let temporary=target.join(format!(".codex-copy-{}",crate::settings::nonce().map_err(|e|e.to_string())?));
                    let result=(|| -> Result<(),String> {
                        use std::io::Write;
                        let mut input=std::fs::File::open(&from).map_err(|e|e.to_string())?;
                        let mut output=std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|e|e.to_string())?;
                        std::io::copy(&mut input,&mut output).map_err(|e|e.to_string())?;
                        output.flush().and_then(|_| output.sync_all()).map_err(|e|e.to_string())?;
                        drop(output);
                        std::fs::rename(&temporary,&to).map_err(|e|e.to_string())?;
                        #[cfg(unix)] std::fs::File::open(&target).and_then(|f|f.sync_all()).map_err(|e|e.to_string())?;
                        Ok(())
                    })();
                    if let Err(e)=result { let _=std::fs::remove_file(&temporary); return Err(Fault::Script(Value::String(e))); }
                }
            }
            ("rmdir", [_, name]) => { std::fs::remove_dir(get(name)?).map_err(|e|e.to_string())?; }
            ("move", [_, source, target]) => { std::fs::rename(get(source)?,get(target)?).map_err(|e|e.to_string())?; }
            _ => return Err("Shell.Execute supports only registration attachment copy, move and empty-directory removal".into()),
        }
        Ok(Value::Number(0.0))
    }
}
