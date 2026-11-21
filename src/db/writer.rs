//! DB40 record encoding. The commit path is kept separate from the KS API.
use super::fields::{Field, Record, Value, MAX_FIELD};
use std::io::Write;
#[path = "writer_counters.rs"]
pub(crate) mod counters;
#[path = "writer_settings.rs"]
pub(crate) mod settings;

/// VirtualText containing one unformatted run. Native ToStream requires
/// the object descriptor even when there are no styles or embedded images.
pub(crate) fn plain_text(data: Vec<u8>) -> Result<Value, String> {
    let length = u32::try_from(data.len()).map_err(|_| "Text is too large")?;
    let mut run = vec![1, 0, 1, 0, 0, 0];
    run.extend(length.to_le_bytes());
    run.extend([0, 0]);
    let mut metadata = vec![12, 0, 12, 0];
    metadata.extend(crc16(&run).to_le_bytes());
    metadata.extend(run);
    Ok(Value::TextWithMetadata { data, metadata })
}

pub(super) fn crc16(bytes: &[u8]) -> u16 {
    const fn table() -> [u16; 256] {
        let mut result = [0u16; 256];
        let mut i = 0;
        while i < 256 {
            let mut high = (i as u16) << 8;
            let mut j = 0;
            while j < 8 {
                high = if high & 0x8000 != 0 {
                    high.wrapping_shl(1) ^ 0x1021
                } else {
                    high.wrapping_shl(1)
                };
                j += 1;
            }
            result[i] = high;
            i += 1;
        }
        result
    }
    const TABLE: [u16; 256] = table();
    let mut crc = 0u16;
    for &byte in bytes {
        crc = crc.wrapping_shl(8) ^ TABLE[(crc >> 8) as usize] ^ byte as u16;
    }
    crc
}
fn short(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), String> {
    out.extend(
        u16::try_from(bytes.len())
            .map_err(|_| "DB40 short string is too long")?
            .to_le_bytes(),
    );
    out.extend_from_slice(bytes);
    Ok(())
}
fn text(out: &mut Vec<u8>, data: &[u8]) -> Result<(), String> {
    let mut stream = (data.len() as u32).to_le_bytes().to_vec();
    stream.extend_from_slice(data);
    for chunk in stream.chunks(65000) {
        // Native framing allows uncompressed blocks (packed == plain).
        let n = chunk.len() as u16;
        out.extend(n.to_le_bytes());
        out.extend(n.to_le_bytes());
        out.extend(crc16(chunk).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    Ok(())
}
fn zlib(out: &mut Vec<u8>, data: &[u8]) -> Result<(), String> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).map_err(|e| e.to_string())?;
    let packed = encoder.finish().map_err(|e| e.to_string())?;
    out.extend((data.len() as u32).to_le_bytes());
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in packed.chunks(1024) {
        for &byte in chunk {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        out.extend((chunk.len() as u16).to_le_bytes());
        out.extend(((b << 16) | a).to_le_bytes());
        out.extend_from_slice(chunk);
    }
    if packed.len() % 1024 == 0 {
        out.extend(0u16.to_le_bytes());
        out.extend(((b << 16) | a).to_le_bytes());
    }
    Ok(())
}
pub(crate) fn encode_record(record: &Record) -> Result<Vec<u8>, String> {
    if record.fields.is_empty() || record.fields.len() > u16::MAX as usize {
        return Err("DB40 field count limit".into());
    }
    let mut out = Vec::new();
    out.extend(record.area.to_le_bytes());
    out.extend((record.fields.len() as u16).to_le_bytes());
    out.extend(record.flags.to_le_bytes());
    for Field {
        id,
        wire_type,
        value,
    } in record.fields.iter()
    {
        out.extend(wire_type.to_le_bytes());
        out.extend(id.to_le_bytes());
        match (wire_type, value) {
            (1, Value::Date(v)) => out.extend(v),
            (2 | 3, Value::String(v)) => short(&mut out, v)?,
            (5, Value::Integer(v)) => out.extend(v.to_le_bytes()),
            (12, Value::Timestamp(v)) => out.extend(v),
            (13, Value::Decimal(v)) => out.extend(v),
            (4, Value::Text(v)) => {
                text(&mut out, v)?;
                out.extend(0u32.to_le_bytes());
            }
            (4, Value::TextWithMetadata { data, metadata }) => {
                text(&mut out, data)?;
                out.extend((metadata.len() as u32).to_le_bytes());
                out.extend(metadata);
            }
            (8 | 14, Value::Binary { data, kind, file }) => {
                if data.len() > MAX_FIELD {
                    return Err("DB40 binary size limit".into());
                }
                if *wire_type == 14 {
                    zlib(&mut out, data)?;
                    out.extend(kind.ok_or("compressed binary kind missing")?.to_le_bytes());
                } else {
                    let tagged = data.len() as u32 | if kind.is_some() { 0x8000_0000 } else { 0 };
                    out.extend(tagged.to_le_bytes());
                    out.extend(data);
                    if let Some(kind) = kind {
                        out.extend(kind.to_le_bytes());
                    }
                }
                if *kind == Some(0) {
                    let file = file.as_ref().ok_or("DB40 file metadata missing")?;
                    out.extend(file.timestamp.to_le_bytes());
                    short(&mut out, &file.source)?;
                    short(&mut out, &file.name)?;
                } else if file.is_some() {
                    return Err("unexpected DB40 file metadata".into());
                }
            }
            _ => return Err(format!("cannot encode DB40 field {id}, type {wire_type}")),
        }
        if out.len() > MAX_FIELD {
            return Err("DB40 record size limit".into());
        }
    }
    // Validate the inverse before this byte sequence can enter a volume.
    let parsed = super::fields::parse_record(&out).map_err(|e| e.to_string())?;
    if parsed.area != record.area || parsed.flags != record.flags || parsed.fields != record.fields
    {
        return Err("DB40 record round-trip mismatch".into());
    }
    Ok(out)
}

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex, OnceLock,
};
use std::{collections::BTreeMap, path::Path};
#[path = "writer_journal.rs"]
mod journal;
#[path = "writer_snapshot.rs"]
pub(super) mod snapshot;
static EPOCH: AtomicU64 = AtomicU64::new(0);
// Serialise native volume mutations, but never hold the cache metadata mutex
// during candidate construction, backup or fsync: other workers can keep reading.
static MUTATIONS: Mutex<()> = Mutex::new(());
static PUBLICATION: std::sync::RwLock<()> = std::sync::RwLock::new(());
pub(super) fn open_guard() -> Result<std::sync::RwLockReadGuard<'static, ()>, String> {
    PUBLICATION
        .read()
        .map_err(|_| "DB40 publication lock poisoned".into())
}
/// Recover before taking the mutation mutex in writer entry points. Readers
/// wait here if another worker is publishing a journal for the same volume.
pub(super) fn ensure_recovered(path: &Path) -> Result<(), String> {
    if !journal::exists(path)? {
        return Ok(());
    }
    let _mutation = crate::execution::lock(&MUTATIONS)?;
    let _volume = volume_lock(path)?;
    recover_locked(path)
}
fn recover_locked(path: &Path) -> Result<(), String> {
    if !journal::exists(path)? {
        return Ok(());
    }
    let _publication = PUBLICATION
        .write()
        .map_err(|_| "DB40 publication lock poisoned")?;
    if journal::recover(path)?.is_some() {
        published_generation(
            path,
            std::fs::metadata(path).map_err(|e| e.to_string())?.len(),
        )?;
    }
    Ok(())
}
pub(crate) fn recover_directory(root: &Path) -> Result<usize, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let mut pending = vec![root];
    let mut recovered = 0;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|e| e == "codex-journal")
            {
                let base = entry.path().with_extension("");
                ensure_recovered(&base)?;
                recovered += 1;
            }
        }
    }
    Ok(recovered)
}
#[derive(Default)]
struct Writes {
    sizes: BTreeMap<String, u64>,
    generations: BTreeMap<String, u64>,
    reserved: BTreeMap<String, u32>,
}
fn writes() -> &'static Mutex<Writes> {
    static STATE: OnceLock<Mutex<Writes>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(Writes::default()))
}
fn path_key(path: &Path) -> String {
    let key = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        key.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        key
    }
}
pub(crate) fn epoch() -> u64 {
    EPOCH.load(Ordering::Acquire)
}
pub(crate) fn committed_size(path: &Path) -> Option<u64> {
    if epoch() == 0 {
        return None;
    }
    writes().lock().ok()?.sizes.get(&path_key(path)).copied()
}
pub(crate) fn generation(path: &Path) -> u64 {
    writes()
        .lock()
        .ok()
        .and_then(|s| s.generations.get(&path_key(path)).copied())
        .unwrap_or(0)
}
/// Call before releasing PUBLICATION: readers and shared query caches must
/// observe the new generation together with the newly published file image.
fn published_generation(path: &Path, size: u64) -> Result<(), String> {
    let mut state = writes().lock().map_err(|_| "DB40 writer lock poisoned")?;
    state.sizes.insert(path_key(path), size);
    let generation = EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    state.generations.insert(path_key(path), generation);
    Ok(())
}
pub(crate) fn reserve(path: &Path) -> Result<u32, String> {
    ensure_recovered(path)?;
    let _mutation = crate::execution::lock(&MUTATIONS)?;
    if journal::exists(path)? {
        let _volume = volume_lock(path)?;
        recover_locked(path)?;
    }
    let mut index = super::mainkey::MainKey::open(path)?;
    let (header, _) = index.writer_parts();
    let max = index.max_oid()?.unwrap_or(u32at(&header, 7));
    let mut state = writes().lock().map_err(|_| "DB40 writer lock poisoned")?;
    let next = max
        .max(*state.reserved.get(&path_key(path)).unwrap_or(&0))
        .checked_add(1)
        .ok_or("DB40 OID space exhausted")?;
    state.reserved.insert(path_key(path), next);
    Ok(next)
}
/// One atomic replacement of the authoritative native volume. A rolling native
/// DB4 backup is synced before publication; an incomplete temporary image is
/// never mounted. The original server must remain stopped during Rust writes.
pub(crate) fn commit(
    path: &Path,
    record: &Record,
    expected: Option<&Record>,
) -> Result<(), String> {
    commit_change(
        path,
        record
            .integer(1024)
            .ok_or("DB40 document identity missing")?,
        Some(record),
        false,
        expected,
    )
}
pub(crate) fn delete(
    path: &Path,
    oid: u32,
    hole: bool,
    expected: Option<&Record>,
) -> Result<(), String> {
    commit_change(path, oid, None, hole, expected)
}
fn commit_change(
    path: &Path,
    oid: u32,
    record: Option<&Record>,
    hole: bool,
    expected: Option<&Record>,
) -> Result<(), String> {
    ensure_recovered(path)?;
    let _mutation = crate::execution::lock(&MUTATIONS)?;
    let _publication = crate::execution::Publication::enter()?;
    let started = std::time::Instant::now();
    let _lock = volume_lock(path)?;
    recover_locked(path)?;
    check_volume_budget(path)?;
    let actual = {
        let mut reader = super::mainkey::MainKey::open(path)?;
        match reader.lookup(oid)? {
            Some(e) if e.block != u32::MAX => Some(reader.record(e)?),
            _ => None,
        }
    };
    if actual.as_ref() != expected {
        return Err(
            "Документ уже изменён другим сохранением. Откройте актуальную редакцию.".into(),
        );
    }
    let old = std::sync::Arc::new(std::fs::read(path).map_err(|e| e.to_string())?);
    let bytes = if let Some(record) = record {
        index_delta::candidate_from_image(path, &[record.clone()], &[], old.clone())?
    } else {
        index_delta::candidate_from_image(path, &[], &[(oid, hole)], old.clone())?
    };
    let bytes = std::sync::Arc::new(bytes);
    let mut check = super::mainkey::MainKey::from_image(bytes.clone())?;
    let entry = check.lookup_including_deleted(oid)?;
    let valid = match (record, entry) {
        (Some(record), Some(entry)) if entry.block != u32::MAX => check.record(entry)? == *record,
        (None, Some(entry)) => hole && entry.block == u32::MAX,
        (None, None) => !hole,
        _ => false,
    };
    if !valid {
        return Err("DB40 transaction verification failed".into());
    }
    drop(check);
    publish_checked(path, old, bytes, started)
}

fn publish_checked(
    path: &Path,
    old: std::sync::Arc<Vec<u8>>,
    bytes: std::sync::Arc<Vec<u8>>,
    started: std::time::Instant,
) -> Result<(), String> {
    let prepared = started.elapsed();
    let temporary = journal::sidecar(path, &format!(".codex-{}.tmp", std::process::id()));
    let backup = journal::sidecar(path, ".codex-backup");
    let backup_tmp = journal::sidecar(path, &format!(".codex-backup-{}.tmp", std::process::id()));
    let plan = journal::plan(&old, &bytes);
    let mut published_bytes = bytes.len();
    let mut created = Vec::new();
    let result = (|| -> Result<(), String> {
        if let Some(encoded) = &plan {
            let _publish = PUBLICATION
                .write()
                .map_err(|_| "DB40 publication lock poisoned")?;
            if !matches_image(path, &old)? {
                return Err("DB40 changed outside this transaction".into());
            }
            published_bytes = journal::publish_shared(path, &old, &bytes, encoded)?;
            published_generation(path, bytes.len() as u64)?;
        } else {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
                .map_err(|e| format!("DB40 transaction temporary file: {e}"))?;
            created.push(temporary.clone());
            f.write_all(&bytes)
                .and_then(|_| f.sync_all())
                .map_err(|e| e.to_string())?;
            drop(f);
            prepare_backup(path, &backup_tmp, &old)?;
            created.push(backup_tmp.clone());
            if !matches_image(path, &old)? {
                return Err("DB40 changed outside this transaction".into());
            }
            crate::settings::replace_file(&backup_tmp, &backup)
                .map_err(|e| format!("DB40 backup publication: {e}"))?;
            let _publish = PUBLICATION
                .write()
                .map_err(|_| "DB40 publication lock poisoned")?;
            publish_volume_with(&temporary, path, &backup, &old, &bytes, replace_volume)?;
            #[cfg(unix)]
            std::fs::File::open(path.parent().ok_or("DB40 parent missing")?)
                .and_then(|f| f.sync_all())
                .map_err(|e| e.to_string())?;
            published_generation(path, bytes.len() as u64)?;
        }
        crate::execution::committed();
        Ok(())
    })();
    // Only files created by this transaction are eligible for cleanup.
    // Existing stale temporary files cause create_new to fail and are retained.
    let safe_cleanup = result.is_ok()
        || std::fs::read(path).ok().is_some_and(|data| {
            data.as_slice() == old.as_slice() || data.as_slice() == bytes.as_slice()
        });
    if safe_cleanup {
        for file in created {
            let _ = std::fs::remove_file(file);
        }
    }
    if ["KODEKS_PROFILE", "KODEKS_PROFILE_WRITES"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|v| v == "1"))
    {
        eprintln!(
            "DB40 write: volume={}, prepare_ms={}, total_ms={}, bytes={}, mode={}, published_bytes={}, success={}",
            path.file_name().unwrap_or_default().to_string_lossy(),
            prepared.as_millis(),
            started.elapsed().as_millis(),
            bytes.len(),
            if plan.is_some() { "pages" } else { "replace" },
            published_bytes,
            result.is_ok()
        );
    }
    result
}
// Preserve the full external-change check without allocating another complete
// volume just to compare it. The publication and volume locks are held by caller.
fn matches_image(path: &Path, expected: &[u8]) -> Result<bool, String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().map_err(|e| e.to_string())?.len() != expected.len() as u64 {
        return Ok(false);
    }
    let mut buffer = [0u8; 65536];
    for chunk in expected.chunks(buffer.len()) {
        file.read_exact(&mut buffer[..chunk.len()])
            .map_err(|e| e.to_string())?;
        if &buffer[..chunk.len()] != chunk {
            return Ok(false);
        }
    }
    Ok(file.read(&mut buffer[..1]).map_err(|e| e.to_string())? == 0)
}
/// Atomic replacement never modifies the old inode. A second directory entry
/// can therefore retain that exact version without rewriting the whole volume.
/// Filesystems without hard links retain the synced-copy fallback.
fn prepare_backup(source: &Path, destination: &Path, bytes: &[u8]) -> Result<(), String> {
    prepare_backup_with(source, destination, bytes, |a, b| std::fs::hard_link(a, b))
}
fn prepare_backup_with(
    source: &Path,
    destination: &Path,
    bytes: &[u8],
    link: impl FnOnce(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    match link(source, destination) {
        Ok(()) => {
            // Also cover a volume last written by kodweb before it was stopped.
            let synced = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(destination)
                .and_then(|file| file.sync_all());
            if let Err(e) = synced {
                let _ = std::fs::remove_file(destination);
                return Err(format!("DB40 linked backup sync: {e}"));
            }
            Ok(())
        }
        Err(_) => {
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(destination)
                .map_err(|e| format!("DB40 backup temporary file: {e}"))?;
            let result = file.write_all(bytes).and_then(|_| file.sync_all());
            drop(file);
            if let Err(e) = result {
                let _ = std::fs::remove_file(destination);
                return Err(format!("DB40 backup copy: {e}"));
            }
            Ok(())
        }
    }
}
fn check_volume_budget(path: &Path) -> Result<(), String> {
    if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 256 * 1024 * 1024 {
        return Err("active DB40 write volume exceeds 256 MiB transaction budget".into());
    }
    Ok(())
}
/// Windows can fail between the two renames inside ReplaceFileW. Keep its
/// rollback name and our already-synced backup until a complete image is again
/// present at the authoritative path. Never overwrite an unexpected image.
fn publish_volume_with(
    source: &Path,
    destination: &Path,
    backup: &Path,
    old: &[u8],
    new: &[u8],
    publish: impl FnOnce(&Path, &Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let rollback = source.with_extension("rollback");
    if rollback.try_exists().map_err(|e| e.to_string())? {
        return Err(format!(
            "DB40 recovery file already exists: {}",
            rollback.display()
        ));
    }
    let outcome = publish(source, destination, &rollback);
    let original_error = match outcome {
        Ok(()) => {
            let _ = std::fs::remove_file(&rollback);
            return Ok(());
        }
        Err(e) => e,
    };
    match std::fs::read(destination) {
        Ok(data) if data==new=>{let _=std::fs::remove_file(&rollback);return Ok(());}
        Ok(data) if data==old=>{let _=std::fs::remove_file(&rollback);}
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{
            let recovery=(||->Result<(),String>{
                let saved=std::fs::read(&rollback).or_else(|_|std::fs::read(backup)).map_err(|e|e.to_string())?;
                if saved!=old{return Err("recovery image differs from the pre-transaction volume".into());}
                let temporary=source.with_extension("recover");
                let mut file=std::fs::OpenOptions::new().create_new(true).write(true).open(&temporary).map_err(|e|e.to_string())?;
                file.write_all(&saved).and_then(|_|file.sync_all()).map_err(|e|e.to_string())?;
                drop(file);
                crate::settings::replace_file(&temporary,destination).map_err(|e|e.to_string())?;
                if std::fs::read(destination).map_err(|e|e.to_string())?!=old{return Err("restored DB40 verification failed".into());}
                let _=std::fs::remove_file(&rollback);
                Ok(())
            })();
            if let Err(recovery_error)=recovery {
                return Err(format!("DB40 publication: {original_error}; recovery: {recovery_error}. Retained backup {}, candidate {}, rollback {}",backup.display(),source.display(),rollback.display()));
            }
        }
        _=>return Err(format!("DB40 publication: {original_error}; unexpected destination state. Retained backup {}, candidate {}, rollback {}",backup.display(),source.display(),rollback.display())),
    }
    Err(format!(
        "DB40 volume publication failed; original volume retained/restored: {original_error}"
    ))
}
fn volume_lock(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    let file = options
        .open(journal::sidecar(path, ".codex-lock"))
        .map_err(|e| format!("DB40 volume is busy: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "DB40 volume is busy: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(file)
}
fn replace_volume(source: &Path, destination: &Path, rollback: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        extern "system" {
            fn ReplaceFileW(
                replaced: *const u16,
                replacement: *const u16,
                backup: *const u16,
                flags: u32,
                exclude: *mut std::ffi::c_void,
                reserved: *mut std::ffi::c_void,
            ) -> i32;
        }
        let from: Vec<_> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let to: Vec<_> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        let previous: Vec<_> = rollback.as_os_str().encode_wide().chain(Some(0)).collect();
        // Retain the destination ACL and allow existing shared-delete readers
        // to finish against their old immutable snapshot.
        if unsafe {
            ReplaceFileW(
                to.as_ptr(),
                from.as_ptr(),
                previous.as_ptr(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = rollback;
        std::fs::rename(source, destination)
    }
}
pub(crate) fn canonicalize(record: &mut Record) {
    std::sync::Arc::make_mut(&mut record.fields)
        .sort_by_key(|f| (matches!(f.wire_type, 4 | 8 | 14), f.wire_type, f.id));
    record.flags = record
        .fields
        .iter()
        .filter(|f| !matches!(f.wire_type, 4 | 8 | 14))
        .count() as u16;
}
fn u16at(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(b[p..p + 2].try_into().unwrap())
}
fn u32at(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}

struct Image {
    bytes: Vec<u8>,
    unit: usize,
    maps: usize,
    capacity: usize,
    span: usize,
    next: usize,
    // Earliest allocation bit that may be free. Scan the native bitmap lazily;
    // materializing every free 128-byte block in a BTreeSet dominated small writes.
    first_free: usize,
}
impl Image {
    fn new(bytes: Vec<u8>) -> Result<Self, String> {
        Self::for_version(bytes, 0x614)
    }
    fn for_version(bytes: Vec<u8>, version: u16) -> Result<Self, String> {
        if bytes.len() < 289 || &bytes[..4] != b"DB40" || u16at(&bytes, 5) != version {
            return Err(format!("writer requires DB40 {version:#x}"));
        }
        let unit = u16at(&bytes, 19) as usize;
        let maps = u16at(&bytes, 21) as usize;
        if unit != 128 || maps != 16 {
            return Err("unsupported DB40 write allocation geometry".into());
        }
        let capacity = unit * maps * 8;
        let span = (unit * 8 + 1) * unit * maps;
        let n = bytes.len() - 289;
        let next = n / span * capacity + (n % span).saturating_sub(unit * maps).div_ceil(unit);
        Ok(Self {
            bytes,
            unit,
            maps,
            capacity,
            span,
            next,
            first_free: 1,
        })
    }
    fn offset(&self, block: usize) -> usize {
        289 + block / self.capacity * self.span + (self.maps + block % self.capacity) * self.unit
    }
    fn release_frame(&mut self, id: u32, tag: u8) -> Result<(), String> {
        let start = id as usize;
        let at = self.offset(start);
        let h = self
            .bytes
            .get(at..at + 7)
            .ok_or("DB40 released frame outside volume")?;
        if h[4] != tag && !(tag == b'L' && h[4] == b'S') {
            return Err("DB40 released frame type mismatch".into());
        }
        let blocks = (u32at(h, 0) as usize + 7).div_ceil(self.unit);
        if start == 0
            || blocks == 0
            || start
                .checked_add(blocks)
                .map_or(true, |end| end > self.next)
        {
            return Err("DB40 released frame bounds".into());
        }
        for id in start..start + blocks {
            let map = 289 + id / self.capacity * self.span;
            let at = map + (id % self.capacity) / 8;
            let mask = 1 << (id % 8);
            if self.bytes[at] & mask == 0 {
                return Err("DB40 overlapping/free replacement frame".into());
            }
            self.bytes[at] &= !mask;
            self.first_free = self.first_free.min(id);
        }
        Ok(())
    }
    fn frame(&mut self, tag: u8, payload: &[u8]) -> Result<(u32, u16), String> {
        // K and D are native mutable directories, with a zero checksum slot.
        let crc = if matches!(tag, b'K' | b'D') {
            0
        } else {
            crc16(payload)
        };
        let mut bytes = (payload.len() as u32).to_le_bytes().to_vec();
        bytes.push(tag);
        bytes.extend(crc.to_le_bytes());
        bytes.extend(payload);
        let blocks = bytes.len().div_ceil(self.unit);
        let mut run = 0;
        let mut reuse = None;
        let mut id = self.first_free;
        let mut first = self.next;
        while id < self.next {
            let map = 289 + id / self.capacity * self.span;
            let bits = self.bytes[map + (id % self.capacity) / 8];
            if bits & (1 << (id % 8)) != 0 {
                run = 0;
                // Fully allocated bitmap bytes need no per-block traversal.
                id += if bits == 255 { 8 - id % 8 } else { 1 };
                continue;
            }
            first = first.min(id);
            if blocks <= self.capacity && id % self.capacity == 0 {
                run = 0;
            }
            run += 1;
            if run == blocks {
                reuse = Some(id + 1 - blocks);
                break;
            }
            id += 1;
        }
        self.first_free = first;
        let start = if let Some(start) = reuse {
            start
        } else {
            if blocks <= self.capacity && self.next % self.capacity + blocks > self.capacity {
                let aligned = self.next.div_ceil(self.capacity) * self.capacity;
                self.next = aligned;
            }
            let start = self.next;
            self.next = self.next.checked_add(blocks).ok_or("DB40 block overflow")?;
            start
        };
        if self.next > u32::MAX as usize || self.offset(self.next) > 256 * 1024 * 1024 {
            return Err("active DB40 write volume exceeds 256 MiB transaction budget".into());
        }
        self.bytes
            .resize(self.offset(self.next).max(self.bytes.len()), 0);
        for i in 0..blocks {
            let id = start + i;
            let map = 289 + id / self.capacity * self.span;
            self.bytes[map + (id % self.capacity) / 8] |= 1 << (id % 8);
            let p = self.offset(id);
            let begin = i * self.unit;
            let end = (begin + self.unit).min(bytes.len());
            self.bytes[p..p + self.unit].fill(0);
            self.bytes[p..p + end - begin].copy_from_slice(&bytes[begin..end]);
        }
        if start == self.first_free {
            self.first_free = start + blocks;
        }
        Ok((start as u32, crc))
    }
    fn tree(
        &mut self,
        area: u16,
        attr: u16,
        items: &[Vec<u8>],
        variable: bool,
    ) -> Result<u32, String> {
        if items.is_empty() {
            return Ok(0);
        }
        let largest = items.iter().map(Vec::len).max().unwrap();
        let leaf = (4089 - 7) / (largest + if variable { 2 } else { 0 });
        let branch = (4089 - 11) / (largest + 4 + if variable { 2 } else { 0 });
        if leaf < 2 || branch < 2 {
            return Err("DB40 index key exceeds node capacity".into());
        }
        let mut capacity = leaf;
        let mut height = 0;
        while items.len() > capacity {
            capacity = capacity
                .checked_mul(branch + 1)
                .and_then(|n| n.checked_add(branch))
                .ok_or("index capacity overflow")?;
            height += 1;
        }
        self.tree_level(area, attr, items, variable, height, leaf, branch)
    }
    fn tree_level(
        &mut self,
        area: u16,
        attr: u16,
        items: &[Vec<u8>],
        variable: bool,
        height: usize,
        leaf: usize,
        branch: usize,
    ) -> Result<u32, String> {
        let mut keys = Vec::new();
        let mut children = Vec::new();
        if height == 0 {
            keys.extend(items.iter().cloned());
        } else {
            let mut child_capacity = leaf;
            for _ in 1..height {
                child_capacity = child_capacity * (branch + 1) + branch;
            }
            let count = (items.len() + 1).div_ceil(child_capacity + 1).max(2);
            if count > branch + 1 {
                return Err("DB40 tree fanout overflow".into());
            }
            let remaining = items.len() - (count - 1);
            let mut at = 0;
            for i in 0..count {
                let n = remaining / count + usize::from(i < remaining % count);
                children.push(self.tree_level(
                    area,
                    attr,
                    &items[at..at + n],
                    variable,
                    height - 1,
                    leaf,
                    branch,
                )?);
                at += n;
                if i + 1 < count {
                    keys.push(items[at].clone());
                    at += 1;
                }
            }
        }
        self.node(area, attr, &keys, &children, variable)
    }
    fn node(
        &mut self,
        area: u16,
        attr: u16,
        keys: &[Vec<u8>],
        children: &[u32],
        variable: bool,
    ) -> Result<u32, String> {
        if !children.is_empty() && children.len() != keys.len() + 1 {
            return Err("invalid DB40 node fanout".into());
        }
        let mut body = Vec::new();
        body.extend(attr.to_le_bytes());
        body.extend(area.to_le_bytes());
        body.extend((keys.len() as u16).to_le_bytes());
        body.push(u8::from(!children.is_empty()));
        if variable {
            let mut offsets = Vec::new();
            let mut entries = Vec::new();
            for (i, key) in keys.iter().enumerate() {
                if !children.is_empty() {
                    entries.extend(children[i].to_le_bytes());
                }
                entries.extend(key);
                offsets.extend((entries.len() as u16).to_le_bytes());
            }
            if !children.is_empty() {
                entries.extend(children.last().unwrap().to_le_bytes());
            }
            body.extend(offsets);
            body.extend(entries);
        } else {
            for (i, key) in keys.iter().enumerate() {
                if !children.is_empty() {
                    body.extend(children[i].to_le_bytes());
                }
                body.extend(key);
            }
            if !children.is_empty() {
                body.extend(children.last().unwrap().to_le_bytes());
            }
        }
        if body.len() > 4089 {
            return Err("DB40 index node overflow".into());
        }
        body.resize(4089, 0);
        Ok(self.frame(b'L', &body)?.0)
    }
}
fn linked_items(groups: BTreeMap<u32, Vec<u32>>) -> Vec<Vec<u8>> {
    let mut result = Vec::new();
    for (key, mut ids) in groups {
        ids.sort_unstable();
        ids.dedup();
        let mut at = 0;
        while at < ids.len() {
            let mut b = key.to_le_bytes().to_vec();
            b.extend(ids[at].to_le_bytes());
            b.push(1);
            at += 1;
            while at < ids.len() && b[8] < 255 {
                let mut delta = ids[at] - ids[at - 1];
                let mut digits = vec![(delta & 127) as u8];
                delta >>= 7;
                while delta > 0 {
                    digits.push((delta & 127) as u8 | 128);
                    delta >>= 7;
                }
                if b.len() + digits.len() > 255 {
                    break;
                }
                b.extend(digits.into_iter().rev());
                b[8] += 1;
                at += 1;
            }
            result.push(b);
        }
    }
    result
}
fn text_key(bytes: &[u8]) -> Vec<u8> {
    encoding_rs::WINDOWS_1251
        .encode(&super::decode(bytes).to_uppercase())
        .0
        .into_owned()
}
fn oid_map(ids: impl Iterator<Item = u32>) -> Vec<u8> {
    let mut groups = BTreeMap::<u32, Vec<u16>>::new();
    for oid in ids {
        groups
            .entry(oid >> 11)
            .or_default()
            .push((oid & 2047) as u16);
    }
    let mut out = Vec::new();
    for (key, ids) in groups {
        let mut compressed = Vec::new();
        let mut previous = 0u16;
        for &id in &ids {
            let mut n = id - previous;
            previous = id;
            let mut digits = vec![(n & 127) as u8];
            n >>= 7;
            while n > 0 {
                digits.push((n & 127) as u8 | 128);
                n >>= 7;
            }
            compressed.extend(digits.into_iter().rev());
        }
        compressed.push(0);
        if ids[0] != 0 && compressed.len() + 2 < 256 {
            out.extend((key | 0x80000000).to_le_bytes());
            out.extend((compressed.len() as u16).to_le_bytes());
            out.extend(compressed);
        } else {
            out.extend(key.to_le_bytes());
            let mut bits = [0u8; 256];
            for id in ids {
                bits[id as usize / 8] |= 1 << (id % 8);
            }
            out.extend(bits);
        }
    }
    out.extend(u32::MAX.to_le_bytes());
    out.resize((out.len() + 7).div_ceil(1024) * 1024 - 7, 0);
    out
}
fn decimal_key(value: &[u8; 34]) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    for (src, n) in [(&value[..26], 25), (&value[26..], 7)] {
        out.extend((0..n).map(|i| src[if i == n - 1 { i } else { i ^ 1 }]));
    }
    out
}
fn decimal_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    fn normalized(b: &[u8]) -> (i8, i64, [u8; 24]) {
        let negative = b[0] == 255;
        let mut m: [u8; 24] = b[1..25].try_into().unwrap();
        if negative {
            let mut carry = 1u16;
            for v in m.iter_mut().rev() {
                let n = (!*v) as u16 + carry;
                *v = n as u8;
                carry = n >> 8;
            }
        }
        if m.iter().all(|v| *v == 0) {
            return (0, 0, m);
        }
        let mut e = if b[25] == 255 { -1i64 } else { 0 };
        for v in &b[26..32] {
            e = (e << 8) | *v as i64;
        }
        while m[0] & 128 == 0 {
            let mut carry = 0;
            for v in m.iter_mut().rev() {
                let next = *v >> 7;
                *v = (*v << 1) | carry;
                carry = next;
            }
            e -= 1;
        }
        (if negative { -1 } else { 1 }, e, m)
    }
    let (sa, ea, ma) = normalized(a);
    let (sb, eb, mb) = normalized(b);
    sa.cmp(&sb).then_with(|| {
        let order = (ea, ma).cmp(&(eb, mb));
        if sa < 0 {
            order.reverse()
        } else {
            order
        }
    })
}
fn field_items(
    records: &BTreeMap<u32, Record>,
    area: u16,
    field: &super::schema::Field,
) -> Result<(Vec<Vec<u8>>, bool), String> {
    let mut entries = Vec::new();
    let mut links = BTreeMap::<u32, Vec<u32>>::new();
    for (&oid, record) in records.iter().filter(|(_, r)| r.area == area) {
        for f in record
            .fields
            .iter()
            .filter(|f| f.id == field.id && f.wire_type == field.kind)
        {
            let mut item = Vec::new();
            match &f.value {
                Value::String(v) if matches!(field.kind, 2 | 3) => {
                    item.extend(oid.to_le_bytes());
                    item.extend(if field.kind == 2 {
                        text_key(v)
                    } else {
                        v.clone()
                    });
                }
                Value::Integer(v) if field.kind == 5 => {
                    links.entry(*v).or_default().push(oid);
                    continue;
                }
                Value::Date(v) => {
                    item.extend(v);
                    item.extend(oid.to_le_bytes());
                }
                Value::Timestamp(v) => {
                    item.extend(oid.to_le_bytes());
                    item.extend(v);
                }
                Value::Decimal(v) => {
                    crate::ks::runtime::decimal::Decimal::from_native(v)?;
                    item.extend(oid.to_le_bytes());
                    item.extend(decimal_key(v));
                }
                _ => {
                    return Err(format!(
                        "DB40 index writer does not support {area}/{}, native type {}",
                        field.id, field.kind
                    ))
                }
            }
            entries.push(item);
        }
    }
    if field.kind == 5 {
        return Ok((linked_items(links), true));
    }
    sort_field_items(field.kind, entries)
}
fn sort_field_items(kind: u16, mut entries: Vec<Vec<u8>>) -> Result<(Vec<Vec<u8>>, bool), String> {
    if kind == 3 {
        let numeric = entries
            .iter()
            .map(|e| crate::ks::sql::numeric_sort_key(&super::decode(&e[4..])))
            .collect::<Option<Vec<_>>>();
        if let Some(keys) = numeric {
            let mut keyed = keys.into_iter().zip(entries).collect::<Vec<_>>();
            keyed.sort_unstable_by(|(ak, a), (bk, b)| {
                ak.cmp(bk).then_with(|| u32at(a, 0).cmp(&u32at(b, 0)))
            });
            let mut entries = keyed.into_iter().map(|(_, e)| e).collect::<Vec<_>>();
            entries.dedup();
            return Ok((entries, true));
        }
    }
    entries.sort_by(|a, b| match kind {
        2 => a[4..]
            .cmp(&b[4..])
            .then_with(|| u32at(a, 0).cmp(&u32at(b, 0))),
        3 => crate::ks::sql::numeric_cmp(&super::decode(&a[4..]), &super::decode(&b[4..]))
            .unwrap()
            .then_with(|| u32at(a, 0).cmp(&u32at(b, 0))),
        1 => (u16at(a, 0), a[2], a[3])
            .cmp(&(u16at(b, 0), b[2], b[3]))
            .then_with(|| u32at(a, 4).cmp(&u32at(b, 4))),
        12 => a[4..]
            .cmp(&b[4..])
            .then_with(|| u32at(a, 0).cmp(&u32at(b, 0))),
        13 => decimal_cmp(&a[4..], &b[4..]).then_with(|| u32at(a, 0).cmp(&u32at(b, 0))),
        _ => a.cmp(b),
    });
    entries.dedup();
    Ok((entries, matches!(kind, 2 | 3)))
}
/// Build an independent candidate image. Publishing is deliberately a separate
/// operation: all indexes and native interoperability must be verified first.
pub(crate) fn candidate(path: &Path, changes: &[Record]) -> Result<Vec<u8>, String> {
    candidate_changes(path, changes, &[])
}
#[path = "writer_index.rs"]
mod index_delta;
use index_delta::candidate_changes;
