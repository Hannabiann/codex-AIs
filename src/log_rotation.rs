//! Move closed daily segments quickly; compress them outside the writer lock.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

static IO: Mutex<()> = Mutex::new(());
static DAY: OnceLock<Mutex<String>> = OnceLock::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
#[path = "log_zip.rs"]
mod zip;

pub(super) fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}
pub(super) fn try_write_guard() -> Option<std::sync::MutexGuard<'static, ()>> {
    IO.try_lock().ok()
}
fn today() -> Result<String, String> {
    let [y, m, d, ..] = crate::ks::date::Date::now()?.parts();
    Ok(format!("{y:04}-{:02}-{d:02}", m + 1))
}
fn day_number(day: &str) -> Option<i64> {
    if day.len() != 10
        || !day.bytes().enumerate().all(|(i, b)| {
            if i == 4 || i == 7 {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return None;
    }
    let date =
        crate::ks::date::Date::from_text(&format!("{}.{}.{}", &day[8..10], &day[5..7], &day[..4]))
            .ok()?;
    let [y, m, d, ..] = date.parts();
    (format!("{y:04}-{:02}-{d:02}", m + 1) == day).then_some((date.millis / 86400000.0) as i64)
}
fn old_root(root: &Path) -> Result<PathBuf, String> {
    let old = root.join("OLD_Log");
    fs::create_dir_all(&old).map_err(|e| e.to_string())?;
    let meta = fs::symlink_metadata(&old).map_err(|e| e.to_string())?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || old.canonicalize().map_err(|e| e.to_string())? != old
    {
        return Err("OLD_Log must be a real directory inside the log directory".into());
    }
    Ok(old)
}
pub(super) fn initialize(root: &Path) -> Result<(), String> {
    old_root(root)?;
    let now = today()?;
    let stored = match fs::read_to_string(root.join(".log-day")) {
        Ok(s) if day_number(s.trim()).is_some() => s.trim().to_owned(),
        Ok(_) => return Err("Invalid Log-codex-AIS/.log-day".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            crate::settings::atomic_write(&root.join(".log-day"), now.as_bytes())
                .map_err(|e| e.to_string())?;
            now.clone()
        }
        Err(e) => return Err(e.to_string()),
    };
    DAY.set(Mutex::new(stored))
        .map_err(|_| "Log rotation already initialized")?;
    let _guard = write_guard();
    Ok(())
}
pub(super) fn write_guard() -> std::sync::MutexGuard<'static, ()> {
    let guard = IO.lock().unwrap_or_else(|e| e.into_inner());
    if let (Some(root), Some(day), Ok(now)) = (super::ROOT.get(), DAY.get(), today()) {
        let mut day = day.lock().unwrap_or_else(|e| e.into_inner());
        if *day < now {
            if let Err(e) = rotate(root, &mut day, &now) {
                eprintln!("Log rotation: {e}");
            }
        }
    }
    guard
}
fn files(directory: &Path, result: &mut Vec<PathBuf>, active: bool) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.file_type().map_err(|e| e.to_string())?;
        if meta.is_symlink() || (active && entry.file_name() == "OLD_Log") {
            continue;
        }
        // Also exclude Windows junctions and other reparse aliases, even when
        // the platform does not classify them as ordinary symbolic links.
        if entry.path().canonicalize().map_err(|e| e.to_string())? != entry.path() {
            continue;
        }
        if meta.is_dir() {
            files(&entry.path(), result, active)?;
        } else if meta.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("log"))
        {
            result.push(entry.path());
        }
    }
    Ok(())
}
// Caller holds IO. Generation changes before the first rename, so surviving KS
// handles reopen the current file even if a later rename fails.
fn rotate(root: &Path, day: &mut String, now: &str) -> Result<(), String> {
    let old = old_root(root)?;
    let mut logs = Vec::new();
    files(root, &mut logs, true)?;
    if !logs.is_empty() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let pending = old.join(format!(".pending-{day}-{}-{nonce}", std::process::id()));
        fs::create_dir(&pending).map_err(|e| e.to_string())?;
        GENERATION.fetch_add(1, Ordering::AcqRel);
        for path in logs {
            let target = pending.join(path.strip_prefix(root).map_err(|e| e.to_string())?);
            fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            fs::rename(&path, &target).map_err(|e| format!("{}: {e}", path.display()))?;
        }
    }
    crate::settings::atomic_write(&root.join(".log-day"), now.as_bytes())
        .map_err(|e| e.to_string())?;
    *day = now.into();
    Ok(())
}
fn segment_name(name: &str) -> Option<&str> {
    if !name.is_ascii() || name.len() < 14 || day_number(&name[..10]).is_none() {
        return None;
    }
    let rest = name.strip_suffix(".zip").unwrap_or(name).get(11..)?;
    if name.as_bytes()[10] != b'-' {
        return None;
    }
    let (pid, nonce) = rest.split_once('-')?;
    (!pid.is_empty()
        && !nonce.is_empty()
        && pid.bytes().chain(nonce.bytes()).all(|b| b.is_ascii_digit()))
    .then_some(&name[..10])
}
fn archive_pending(root: &Path) -> Result<(), String> {
    let old = old_root(root)?;
    // Snapshot under IO: rotate() must finish moving the segment's files.
    let entries = {
        let _guard = IO.lock().unwrap_or_else(|e| e.into_inner());
        fs::read_dir(&old)
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    for entry in entries {
        if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name
            .strip_prefix(".pending-")
            .filter(|s| segment_name(s).is_some())
        else {
            continue;
        };
        let directory = entry.path();
        if directory.canonicalize().map_err(|e| e.to_string())? != directory {
            continue;
        }
        let destination = old.join(format!("{stem}.zip"));
        let mut entries = Vec::new();
        files(&directory, &mut entries, false)?;
        if !destination.exists() && !entries.is_empty() {
            let temp = old.join(format!(".{stem}.zip.tmp"));
            // A previous interrupted compression is disposable; source segments
            // remain until a complete, synced ZIP is atomically published.
            if temp.exists() {
                fs::remove_file(&temp).map_err(|e| e.to_string())?;
            }
            zip::write(&temp, &directory, &entries).map_err(|e| e.to_string())?;
            crate::settings::replace_file(&temp, &destination).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            fs::File::open(&old)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        if entries.is_empty() || destination.is_file() {
            // Only our validated pending directory is removed. Never follow
            // links or recursively remove an arbitrary configured data path.
            remove_segment(&directory)?;
        }
    }
    Ok(())
}
fn remove_segment(dir: &Path) -> Result<(), String> {
    if dir.canonicalize().map_err(|e| e.to_string())? != dir {
        return Err("Pending log directory changed; preserved".into());
    }
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if entry.path().canonicalize().map_err(|e| e.to_string())? != entry.path() {
            return Err("Unexpected link in pending log archive; preserved".into());
        }
        if kind.is_dir() {
            remove_segment(&entry.path())?;
        } else if kind.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("log"))
        {
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        } else {
            return Err("Unexpected file in pending log archive; preserved".into());
        }
    }
    fs::remove_dir(dir).map_err(|e| e.to_string())
}
fn prune(root: &Path, now: &str, days: usize) -> Result<(), String> {
    if days == 0 {
        return Ok(());
    }
    let cutoff = day_number(now).ok_or("Invalid rotation date")? - days as i64;
    for entry in fs::read_dir(old_root(root)?).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".zip") {
            continue;
        }
        if segment_name(&name)
            .and_then(day_number)
            .is_some_and(|d| d < cutoff)
        {
            fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}
pub(super) fn start(days: usize) -> std::io::Result<()> {
    let Some(root) = super::ROOT.get().cloned() else {
        return Ok(());
    };
    std::thread::Builder::new()
        .name("log-archives".into())
        .spawn(move || loop {
            {
                let _guard = write_guard();
            }
            let result = today()
                .and_then(|now| prune(&root, &now, days))
                .and_then(|_| archive_pending(&root));
            if let Err(e) = result {
                super::error("log archive", &e);
            }
            std::thread::sleep(std::time::Duration::from_secs(30));
        })?;
    Ok(())
}

#[cfg(test)]
#[path = "log_rotation_tests.rs"]
mod tests;

#[cfg(test)]
pub(super) fn rotate_test(root: &Path, from: &str, to: &str) {
    let _guard = IO.lock().unwrap();
    rotate(root, &mut from.to_string(), to).unwrap();
}
