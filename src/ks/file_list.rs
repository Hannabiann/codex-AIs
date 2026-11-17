use std::{path::Path, time::UNIX_EPOCH};
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub size: u64,
    pub modified: f64,
}
#[derive(Debug)]
pub enum Error {
    Io(String),
    Limit(String),
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}
pub fn list(path: &Path, mask: &str, mode: u32) -> Result<Vec<Entry>, Error> {
    if mask.len() > 1024 || mask.contains('/') || mask.contains('\\') || mask.contains('\0') {
        return Err(Error::Limit(
            "File.list requires a filename mask of at most 1024 bytes".into(),
        ));
    }
    if mode > 3 {
        return Err(Error::Limit(
            "File.list supported modes: 0/1 files, 2 directories, 3 both".into(),
        ));
    }
    let mode = if mode == 0 { 1 } else { mode };
    let mask = if mask == "*.*" { "*" } else { mask };
    let mut pattern = String::from("(?i)^");
    for c in mask.chars() {
        match c {
            '*' => pattern.push_str(".*"),
            '?' => pattern.push('.'),
            _ => pattern.push_str(&regex::escape(&c.to_string())),
        }
    }
    pattern.push('$');
    let re = regex::Regex::new(&pattern).map_err(|e| Error::Limit(e.to_string()))?;
    let mut out = Vec::new();
    for (scanned, item) in std::fs::read_dir(path)?.enumerate() {
        if scanned >= 100_000 {
            return Err(Error::Limit(
                "File.list directory entry budget exceeded".into(),
            ));
        }
        let item = item?;
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| Error::Limit("File.list filename is not Unicode".into()))?;
        if !re.is_match(&name) {
            continue;
        }
        let meta = std::fs::metadata(item.path())?;
        if (meta.is_dir() && mode & 2 == 0) || (!meta.is_dir() && mode & 1 == 0) {
            continue;
        }
        if out.len() >= 10000 {
            return Err(Error::Limit("File.list result limit (10000)".into()));
        }
        let time = meta
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Limit("File.list timestamp before 1970 is not supported".into()))?;
        // Native FileTimeToDosDateTime supplies two-second resolution.
        out.push(Entry {
            name,
            size: meta.len(),
            modified: ((time.as_secs() / 2) * 2000) as f64,
        });
    }
    Ok(out)
}
