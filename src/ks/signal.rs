//! Named manual-reset signals shared across KS contexts in this process.
//! Native Wait (006e51b0) uses seconds and returns true on timeout.
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, OnceLock, Weak},
    time::Duration,
};
#[derive(Debug, Default)]
pub struct Signal {
    on: Mutex<bool>,
    changed: Condvar,
}
static SIGNALS: OnceLock<Mutex<HashMap<String, Weak<Signal>>>> = OnceLock::new();
impl Signal {
    pub fn named(name: &str) -> Result<Arc<Self>, String> {
        if name.is_empty() || name.len() > 1024 || name.contains('\0') {
            return Err("invalid Signal name".into());
        }
        let mut pool = SIGNALS
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "signal pool poisoned")?;
        pool.retain(|_, v| v.strong_count() > 0);
        if let Some(s) = pool.get(name).and_then(Weak::upgrade) {
            return Ok(s);
        }
        if pool.len() >= 2048 {
            return Err("named Signal limit".into());
        }
        let signal = Arc::new(Self::default());
        pool.insert(name.into(), Arc::downgrade(&signal));
        Ok(signal)
    }
    pub fn switch(&self, on: bool) -> Result<(), String> {
        *self.on.lock().map_err(|_| "signal poisoned")? = on;
        if on {
            self.changed.notify_all();
        }
        Ok(())
    }
    pub fn wait(&self, seconds: u32) -> Result<bool, String> {
        let start = std::time::Instant::now();
        let limit = Duration::from_secs(seconds as u64);
        let mut state = self.on.lock().map_err(|_| "signal poisoned")?;
        while !*state {
            crate::execution::check()?;
            let Some(left) = limit.checked_sub(start.elapsed()) else {
                return Ok(true);
            };
            state = self
                .changed
                .wait_timeout(state, left.min(Duration::from_millis(50)))
                .map_err(|_| "signal poisoned")?
                .0;
        }
        Ok(false)
    }
}
