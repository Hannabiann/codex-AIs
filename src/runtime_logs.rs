//! Resolve KS logs once, before starting the request workers.
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

static ROOT: OnceLock<PathBuf> = OnceLock::new();

pub fn configured_root() -> Option<PathBuf> {
    ROOT.get()
        .cloned()
        .or_else(|| std::env::var_os("KODEKS_LOG_DIR").map(PathBuf::from))
}

pub fn initialize(data: &Path) -> Result<(), String> {
    let configured = std::env::var_os("KODEKS_LOG_DIR").map(PathBuf::from);
    let logs = prepare(data, configured.as_deref())?;
    eprintln!("KS log directory: {}", logs.display());
    ROOT.set(logs)
        .map_err(|_| "KS log directory already initialized".into())
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
        let mut name = data.file_name().ok_or("Set KODEKS_LOG_DIR to a separate directory when the data directory is a filesystem root")?.to_os_string();
        name.push(".codex-AIs-logs");
        data.with_file_name(name)
    };
    let error = |e| {
        format!("KS log directory {}: {e}. Set KODEKS_LOG_DIR to a writable directory outside the data tree", target.display())
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
    if resolved.starts_with(&data) || data.starts_with(&resolved) {
        return Err(error("log and data directories must not overlap".into()));
    }
    std::fs::create_dir_all(&resolved).map_err(|e| error(e.to_string()))?;
    let resolved = resolved.canonicalize().map_err(|e| error(e.to_string()))?;
    if resolved.starts_with(&data) || data.starts_with(&resolved) {
        return Err(error("log and data directories must not overlap".into()));
    }
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
