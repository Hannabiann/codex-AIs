//! Named locks shared by KS contexts in this process. ScriptMutex re-entry
//! through the same handle is idempotent (native FUN_00772ef0).
use std::{
    collections::HashMap,
    sync::{Arc, Condvar, Mutex, OnceLock, Weak},
    thread::ThreadId,
    time::{Duration, Instant},
};
#[derive(Debug, Default)]
struct State {
    owner: Option<ThreadId>,
    holds: u32,
}
#[derive(Debug, Default)]
struct Lock {
    state: Mutex<State>,
    changed: Condvar,
}
type Pool = Mutex<HashMap<String, Weak<Lock>>>;
static POOL: OnceLock<Pool> = OnceLock::new();
#[derive(Debug)]
pub struct NamedMutex {
    lock: Arc<Lock>,
    held: bool,
}
impl NamedMutex {
    pub fn unnamed() -> Self {
        Self {
            lock: Arc::new(Lock::default()),
            held: false,
        }
    }
    pub fn new(name: &str) -> Result<Self, String> {
        if name.is_empty() || name.len() > 1024 || name.contains('\0') {
            return Err("invalid Mutex name".into());
        }
        let mut pool = POOL
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "mutex registry poisoned")?;
        pool.retain(|_, v| v.strong_count() > 0);
        let lock = if let Some(v) = pool.get(name).and_then(Weak::upgrade) {
            v
        } else {
            if pool.len() >= 1024 {
                return Err("named mutex limit".into());
            }
            let v = Arc::new(Lock::default());
            pool.insert(name.into(), Arc::downgrade(&v));
            v
        };
        Ok(Self { lock, held: false })
    }
    pub fn acquire(&mut self, timeout: Option<Duration>) -> Result<bool, String> {
        if self.held {
            return Ok(true);
        }
        let owner = std::thread::current().id();
        let start = Instant::now();
        let mut s = self.lock.state.lock().map_err(|_| "mutex poisoned")?;
        while s.owner.is_some_and(|o| o != owner) {
            crate::execution::check()?;
            s = if let Some(limit) = timeout {
                let Some(left) = limit.checked_sub(start.elapsed()) else {
                    return Ok(false);
                };
                let (next, result) = self
                    .lock
                    .changed
                    .wait_timeout(s, left.min(Duration::from_millis(50)))
                    .map_err(|_| "mutex poisoned")?;
                if result.timed_out() && start.elapsed() >= limit {
                    return Ok(false);
                }
                next
            } else {
                self.lock
                    .changed
                    .wait_timeout(s, Duration::from_millis(50))
                    .map_err(|_| "mutex poisoned")?
                    .0
            };
        }
        s.owner = Some(owner);
        s.holds = s.holds.checked_add(1).ok_or("mutex recursion limit")?;
        self.held = true;
        Ok(true)
    }
    pub fn release(&mut self) -> Result<(), String> {
        if !self.held {
            return Ok(());
        }
        let mut s = self.lock.state.lock().map_err(|_| "mutex poisoned")?;
        if s.owner != Some(std::thread::current().id()) {
            return Err("mutex belongs to another thread".into());
        }
        s.holds -= 1;
        self.held = false;
        if s.holds == 0 {
            s.owner = None;
            self.lock.changed.notify_one();
        }
        Ok(())
    }
}
impl Drop for NamedMutex {
    fn drop(&mut self) {
        let _ = self.release();
    }
}
