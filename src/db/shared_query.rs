//! Immutable query data shared by engines mounting the same canonical root.
//! KS variables, authorization, document edits and file readers remain local.
use super::indexed_query::{RangeCache, ReverseLinks};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock, Weak},
    time::Duration,
};

pub(super) fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum BuildKey {
    Reverse(PathBuf, u16, u64),
    Range(PathBuf, u16, u64, String),
}

#[derive(Debug, Default)]
pub(super) struct SharedQueries {
    pub(super) reverse_links: Mutex<ReverseLinks>,
    pub(super) ranges: Mutex<RangeCache>,
    flights: Mutex<BTreeMap<BuildKey, Weak<Flight>>>,
}

impl SharedQueries {
    pub(super) fn for_root(root: &Path) -> Result<Arc<Self>, String> {
        static ROOTS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<SharedQueries>>>> = OnceLock::new();
        let mut roots = crate::execution::lock(ROOTS.get_or_init(Default::default))?;
        let key = path_key(root);
        if let Some(cache) = roots.get(&key).and_then(Weak::upgrade) {
            return Ok(cache);
        }
        roots.retain(|_, cache| cache.strong_count() != 0);
        let cache = Arc::new(Self::default());
        roots.insert(key, Arc::downgrade(&cache));
        Ok(cache)
    }

    /// No data-cache lock is held while waiting or building. A failed or
    /// cancelled builder drops its lease; a waiter can then retry the lookup.
    pub(super) fn build(&self, key: BuildKey) -> Result<BuildLease, String> {
        let flight = {
            let mut flights = crate::execution::lock(&self.flights)?;
            if flights.len() >= 4096 {
                flights.retain(|_, flight| flight.strong_count() != 0);
            }
            if let Some(flight) = flights.get(&key).and_then(Weak::upgrade) {
                flight
            } else {
                let flight = Arc::new(Flight::default());
                flights.insert(key, Arc::downgrade(&flight));
                flight
            }
        };
        let mut busy = crate::execution::lock(&flight.busy)?;
        while *busy {
            crate::execution::check()?;
            busy = flight
                .changed
                .wait_timeout(busy, Duration::from_millis(25))
                .map_err(|_| "query build lock poisoned")?
                .0;
        }
        crate::execution::check()?;
        *busy = true;
        drop(busy);
        Ok(BuildLease(flight))
    }
}

#[derive(Debug, Default)]
struct Flight {
    busy: Mutex<bool>,
    changed: Condvar,
}
pub(super) struct BuildLease(Arc<Flight>);
impl Drop for BuildLease {
    fn drop(&mut self) {
        // The mutex protects only the flag, not the builder's work. Unwinding
        // or cancellation outside this critical section cannot poison it.
        if let Ok(mut busy) = self.0.busy.lock() {
            *busy = false;
            self.0.changed.notify_all();
        }
    }
}
