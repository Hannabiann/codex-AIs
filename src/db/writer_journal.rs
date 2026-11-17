//! Undo/redo journal for sparse, durable native DB40 page publications.
//! One journal belongs to one volume; the caller owns its mutation/file locks.
use super::snapshot;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

const PAGE: usize = 4096;
const MAX_IMAGE: usize = 256 * 1024 * 1024;
const MAX_JOURNAL: usize = 128 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"CAISWAL1";
const PREPARED: &[u8; 8] = b"PREPARED";
const COMMIT: &[u8; 8] = b"COMMIT01";

pub(super) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    name.into()
}
pub(super) fn path(path: &Path) -> PathBuf {
    sidecar(path, ".codex-journal")
}
pub(super) fn exists(base: &Path) -> Result<bool, String> {
    path(base)
        .try_exists()
        .map_err(|e| format!("DB40 journal lookup: {e}"))
}
fn sync_parent(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )
    .and_then(|f| f.sync_all())
    .map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let size = file.metadata().map_err(|e| e.to_string())?.len();
    if size > limit as u64 {
        return Err("DB40 recovery file exceeds size limit".into());
    }
    let mut bytes = Vec::with_capacity(size as usize + 1);
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("DB40 recovery file grew while reading".into());
    }
    Ok(bytes)
}
fn temporary(base: &Path, kind: &str) -> Result<(PathBuf, File), String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..1000 {
        let name = sidecar(
            base,
            &format!(
                ".codex-{kind}-{}-{}.tmp",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
        );
        match OpenOptions::new().write(true).create_new(true).open(&name) {
            Ok(file) => return Ok((name, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("DB40 temporary file namespace exhausted; old files retained".into())
}
#[derive(Debug)]
struct Page {
    at: usize,
    old: Vec<u8>,
    new: Vec<u8>,
}
#[derive(Debug)]
struct Journal {
    old_len: usize,
    new_len: usize,
    old_crc: u32,
    new_crc: u32,
    pages: Vec<Page>,
    committed: bool,
}
fn part(bytes: &[u8], at: usize) -> &[u8] {
    bytes
        .get(at..bytes.len().min(at + PAGE))
        .unwrap_or_default()
}
impl Journal {
    fn between(old: &[u8], new: &[u8]) -> Self {
        let pages = (0..old.len().max(new.len()))
            .step_by(PAGE)
            .filter_map(|at| {
                let a = part(old, at);
                let b = part(new, at);
                (a != b).then(|| Page {
                    at,
                    old: a.to_vec(),
                    new: b.to_vec(),
                })
            })
            .collect();
        Self {
            old_len: old.len(),
            new_len: new.len(),
            old_crc: crc32fast::hash(old),
            new_crc: crc32fast::hash(new),
            pages,
            committed: false,
        }
    }
    fn encode(&self) -> Vec<u8> {
        let mut b = MAGIC.to_vec();
        b.extend((self.old_len as u64).to_le_bytes());
        b.extend((self.new_len as u64).to_le_bytes());
        b.extend(self.old_crc.to_le_bytes());
        b.extend(self.new_crc.to_le_bytes());
        b.extend((self.pages.len() as u32).to_le_bytes());
        b.extend((PAGE as u32).to_le_bytes());
        for p in &self.pages {
            b.extend((p.at as u64).to_le_bytes());
            b.extend((p.old.len() as u32).to_le_bytes());
            b.extend((p.new.len() as u32).to_le_bytes());
            b.extend(&p.old);
            b.extend(&p.new);
        }
        b.extend(PREPARED);
        b.extend(crc32fast::hash(&b).to_le_bytes());
        b
    }
    fn decode(b: &[u8]) -> Result<Self, String> {
        fn u64n(b: &[u8], at: usize) -> u64 {
            u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
        }
        fn u32n(b: &[u8], at: usize) -> u32 {
            u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
        }
        if b.len() < 52 || b.len() > MAX_JOURNAL || &b[..8] != MAGIC || u32n(b, 36) as usize != PAGE
        {
            return Err("invalid DB40 journal header".into());
        }
        let old_len = usize::try_from(u64n(b, 8)).map_err(|_| "journal length overflow")?;
        let new_len = usize::try_from(u64n(b, 16)).map_err(|_| "journal length overflow")?;
        let count = u32n(b, 32) as usize;
        if old_len > MAX_IMAGE || new_len > MAX_IMAGE || count > MAX_IMAGE / PAGE {
            return Err("DB40 journal image/count limit".into());
        }
        let mut pages = Vec::new();
        let mut pos = 40;
        for _ in 0..count {
            if pos + 16 > b.len() {
                return Err("truncated DB40 journal page".into());
            }
            let at = usize::try_from(u64n(b, pos)).map_err(|_| "journal offset overflow")?;
            let a = u32n(b, pos + 8) as usize;
            let c = u32n(b, pos + 12) as usize;
            pos += 16;
            if at % PAGE != 0
                || at >= old_len.max(new_len)
                || pages.last().is_some_and(|p: &Page| p.at >= at)
                || a != old_len.saturating_sub(at).min(PAGE)
                || c != new_len.saturating_sub(at).min(PAGE)
                || pos.checked_add(a + c).is_none_or(|end| end > b.len())
            {
                return Err("invalid DB40 journal page bounds/order".into());
            }
            pages.push(Page {
                at,
                old: b[pos..pos + a].to_vec(),
                new: b[pos + a..pos + a + c].to_vec(),
            });
            pos += a + c;
        }
        if pos + 12 > b.len()
            || &b[pos..pos + 8] != PREPARED
            || u32n(b, pos + 8) != crc32fast::hash(&b[..pos + 8])
        {
            return Err("DB40 journal checksum/trailer mismatch; retained for recovery".into());
        }
        pos += 12;
        let marker = commit_marker(u32n(b, pos - 4));
        let tail = &b[pos..];
        // A torn commit marker is not a committed transaction. Reject unrelated
        // trailing bytes instead of interpreting arbitrary corruption as undo.
        if tail.len() > marker.len() || tail != &marker[..tail.len()] {
            return Err("invalid DB40 commit marker; retained for recovery".into());
        }
        Ok(Self {
            old_len,
            new_len,
            old_crc: u32n(b, 24),
            new_crc: u32n(b, 28),
            pages,
            committed: tail.len() == marker.len(),
        })
    }
    fn image(&self, current: &[u8], new: bool) -> Result<Vec<u8>, String> {
        if current.len() > self.old_len.max(self.new_len) {
            return Err("DB40 recovery found unexpected file growth".into());
        }
        let mut image = current.to_vec();
        image.resize(if new { self.new_len } else { self.old_len }, 0);
        for page in &self.pages {
            let bytes = if new { &page.new } else { &page.old };
            if !bytes.is_empty() {
                image[page.at..page.at + bytes.len()].copy_from_slice(bytes);
            }
        }
        if crc32fast::hash(&image) != if new { self.new_crc } else { self.old_crc } {
            return Err("DB40 recovery found changes outside the journal; files retained".into());
        }
        Ok(image)
    }
}
fn commit_marker(checksum: u32) -> Vec<u8> {
    let mut b = COMMIT.to_vec();
    b.extend(checksum.to_le_bytes());
    b.extend(crc32fast::hash(&b).to_le_bytes());
    b
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Journal,
    BackupPage,
    Backup,
    BasePage,
    Base,
    Marker,
    Commit,
}
fn patch(
    file: &mut File,
    old: &[u8],
    new: &[u8],
    mut page_done: impl FnMut() -> Result<(), String>,
) -> Result<usize, String> {
    if new.len() > old.len() {
        file.set_len(new.len() as u64).map_err(|e| e.to_string())?;
    }
    let mut written = 0;
    for at in (0..new.len()).step_by(PAGE) {
        let b = part(new, at);
        if part(old, at) != b {
            file.seek(SeekFrom::Start(at as u64))
                .and_then(|_| file.write_all(b))
                .map_err(|e| e.to_string())?;
            written += b.len();
            page_done()?;
        }
    }
    if new.len() < old.len() {
        file.set_len(new.len() as u64).map_err(|e| e.to_string())?;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    Ok(written)
}
fn backup(
    base: &Path,
    old: &[u8],
    hook: &mut impl FnMut(Step) -> Result<(), String>,
) -> Result<(), String> {
    let target = sidecar(base, ".codex-backup");
    let base_file = File::open(base).map_err(|e| e.to_string())?;
    if target.try_exists().map_err(|e| e.to_string())? {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&target)
            .map_err(|e| e.to_string())?;
        if snapshot::identity(&file)? != snapshot::identity(&base_file)? {
            let previous = read_bounded(&target, MAX_IMAGE)?;
            snapshot::freeze(&file, &previous)?;
            patch(&mut file, &previous, old, || hook(Step::BackupPage))?;
            snapshot::retire(&file)?;
            return Ok(());
        }
    }
    // Never overwrite a backup hard-linked to the current base. Also used for
    // the first write; subsequent backups only update changed pages.
    let (temp, mut file) = temporary(base, "backup")?;
    file.write_all(old)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    crate::settings::replace_file(&temp, &target).map_err(|e| e.to_string())?;
    sync_parent(&target)
}

/// Return None for dense changes, which retain the full-image replacement path.
pub(super) fn plan(old: &[u8], new: &[u8]) -> Option<Vec<u8>> {
    if old.len().max(new.len()) > MAX_IMAGE {
        return None;
    }
    let journal = Journal::between(old, new);
    let changed: usize = journal
        .pages
        .iter()
        .map(|p| p.old.len() + p.new.len() + 16)
        .sum();
    if changed > MAX_JOURNAL - 68 || changed > old.len().max(1024 * 1024) / 2 {
        return None;
    }
    Some(journal.encode())
}
fn apply(
    base: &Path,
    old: &[u8],
    new: &[u8],
    encoded: &[u8],
    mut hook: impl FnMut(Step) -> Result<(), String>,
) -> Result<usize, String> {
    let started = std::time::Instant::now();
    let stable = path(base);
    if exists(base)? {
        return Err("DB40 pending journal must be recovered before writing".into());
    }
    let (temporary, mut journal) = temporary(base, "journal")?;
    journal
        .write_all(encoded)
        .and_then(|_| journal.sync_all())
        .map_err(|e| e.to_string())?;
    drop(journal);
    crate::settings::replace_file(&temporary, &stable).map_err(|e| e.to_string())?;
    sync_parent(&stable)?;
    let journal_ms = started.elapsed().as_micros();
    hook(Step::Journal)?;
    backup(base, old, &mut hook)?;
    hook(Step::Backup)?;
    let backup_ms = started.elapsed().as_micros();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(base)
        .map_err(|e| e.to_string())?;
    snapshot::freeze(&file, old)?;
    let written = patch(&mut file, old, new, || hook(Step::BasePage))?;
    hook(Step::Base)?;
    let base_ms = started.elapsed().as_micros();
    let mut journal = OpenOptions::new()
        .append(true)
        .open(&stable)
        .map_err(|e| e.to_string())?;
    let marker = commit_marker(u32::from_le_bytes(
        encoded[encoded.len() - 4..].try_into().unwrap(),
    ));
    journal.write_all(&marker[..5]).map_err(|e| e.to_string())?;
    hook(Step::Marker)?;
    journal
        .write_all(&marker[5..])
        .and_then(|_| journal.sync_all())
        .map_err(|e| e.to_string())?;
    snapshot::retire(&file)?;
    hook(Step::Commit)?;
    drop(journal);
    // A committed journal can safely remain after cleanup failure. The next
    // open/write will redo it before using this volume.
    if std::fs::remove_file(&stable).is_ok() {
        let _ = sync_parent(&stable);
    }
    if std::env::var_os("KODEKS_PROFILE_WRITES").is_some_and(|v| v == "1") {
        eprintln!("DB40 pages: journal_us={journal_ms}, backup_us={}, base_us={}, commit_us={}, journal_bytes={}", backup_ms-journal_ms, base_ms-backup_ms, started.elapsed().as_micros()-base_ms, encoded.len());
    }
    Ok(written)
}
pub(super) fn publish(
    base: &Path,
    old: &[u8],
    new: &[u8],
    encoded: &[u8],
) -> Result<usize, String> {
    match apply(base, old, new, encoded, |_| Ok(())) {
        Ok(n) => Ok(n),
        Err(error) => match recover(base) {
            Ok(Some(true)) => Ok(encoded.len()),
            Ok(_) => Err(format!("DB40 page publication rolled back: {error}")),
            Err(recovery) => Err(format!(
                "DB40 page publication: {error}; recovery: {recovery}. Keep {} and its backup",
                path(base).display()
            )),
        },
    }
}
/// Caller holds the publication write lock. Validate both reconstructed images
/// before touching files; repeated recovery after another crash is idempotent.
pub(super) fn recover(base: &Path) -> Result<Option<bool>, String> {
    if !exists(base)? {
        return Ok(None);
    }
    let encoded = read_bounded(&path(base), MAX_JOURNAL)?;
    let journal = Journal::decode(&encoded)?;
    let current = read_bounded(base, MAX_IMAGE)?;
    let old = journal.image(&current, false)?;
    let new = journal.image(&current, true)?;
    backup(base, &old, &mut |_| Ok(()))?;
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(base)
        .map_err(|e| e.to_string())?;
    // Existing readers were frozen before an in-process failed publication.
    // On restart there are no readers yet. Never expose an intermediate image.
    snapshot::freeze(&file, &old)?;
    patch(
        &mut file,
        &current,
        if journal.committed { &new } else { &old },
        || Ok(()),
    )?;
    snapshot::retire(&file)?;
    std::fs::remove_file(path(base)).map_err(|e| e.to_string())?;
    sync_parent(base)?;
    Ok(Some(journal.committed))
}
