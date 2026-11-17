//! Preserve file-backed readers when a publication overwrites native pages.
//! Call register/freeze/retire while holding the publication lock.
use std::{
    collections::HashMap,
    fs::File,
    sync::{Arc, Mutex, OnceLock, Weak},
};

#[derive(Debug, Default)]
pub(in crate::db) struct Snapshot {
    pub bytes: OnceLock<Arc<Vec<u8>>>,
}
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct Identity(u64, u64);

pub(super) fn identity(file: &File) -> Result<Identity, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = file.metadata().map_err(|e| e.to_string())?;
        Ok(Identity(m.dev(), m.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        #[derive(Default)]
        struct Info {
            attributes: u32,
            created: [u32; 2],
            accessed: [u32; 2],
            written: [u32; 2],
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut Info) -> i32;
        }
        let mut info = Info::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(Identity(
            info.volume as u64,
            (info.index_high as u64) << 32 | info.index_low as u64,
        ))
    }
}
fn readers() -> &'static Mutex<HashMap<Identity, Weak<Snapshot>>> {
    static READERS: OnceLock<Mutex<HashMap<Identity, Weak<Snapshot>>>> = OnceLock::new();
    READERS.get_or_init(Default::default)
}
pub(in crate::db) fn register(file: &File) -> Result<Arc<Snapshot>, String> {
    let id = identity(file)?;
    let mut readers = readers()
        .lock()
        .map_err(|_| "DB40 snapshot lock poisoned")?;
    if let Some(snapshot) = readers.get(&id).and_then(Weak::upgrade) {
        return Ok(snapshot);
    }
    if readers.len() >= 4096 {
        readers.retain(|_, value| value.strong_count() != 0);
    }
    let snapshot = Arc::new(Snapshot::default());
    readers.insert(id, Arc::downgrade(&snapshot));
    Ok(snapshot)
}
pub(super) fn freeze(file: &File, old: &[u8]) -> Result<(), String> {
    let id = identity(file)?;
    let readers = readers()
        .lock()
        .map_err(|_| "DB40 snapshot lock poisoned")?;
    if let Some(snapshot) = readers.get(&id).and_then(Weak::upgrade) {
        snapshot.bytes.get_or_init(|| Arc::new(old.to_vec()));
    }
    Ok(())
}
pub(super) fn retire(file: &File) -> Result<(), String> {
    let id = identity(file)?;
    readers()
        .lock()
        .map_err(|_| "DB40 snapshot lock poisoned")?
        .remove(&id);
    Ok(())
}
