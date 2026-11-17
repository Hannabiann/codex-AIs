//! Cooperative cancellation belongs to one HTTP operation, never to a thread.
use std::{
    cell::{Cell, RefCell},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
};
#[derive(Default)]
pub struct Control {
    cancelled: AtomicBool,
    commits: AtomicU64,
}
impl Control {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    pub fn commits(&self) -> u64 {
        self.commits.load(Ordering::Acquire)
    }
}
thread_local! {
    static CURRENT: RefCell<Option<Arc<Control>>> = const { RefCell::new(None) };
    static CRITICAL: Cell<usize> = const { Cell::new(0) };
}
pub struct Scope(Option<Arc<Control>>);
impl Scope {
    pub fn enter(control: Arc<Control>) -> Self {
        Self(CURRENT.with(|s| s.replace(Some(control))))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|s| s.replace(self.0.take()));
    }
}
pub fn cancelled() -> bool {
    CURRENT.with(|s| s.borrow().as_ref().is_some_and(|c| c.cancelled()))
}
pub fn check() -> Result<(), String> {
    if CRITICAL.with(|c| c.get() == 0) && cancelled() {
        Err("Запрос отменён: клиент отключился".into())
    } else {
        Ok(())
    }
}
/// Finish one already-started atomic DB4 publication, then observe cancellation.
pub struct Publication;
impl Publication {
    pub fn enter() -> Result<Self, String> {
        check()?;
        CRITICAL.with(|c| c.set(c.get() + 1));
        Ok(Self)
    }
}
impl Drop for Publication {
    fn drop(&mut self) {
        CRITICAL.with(|c| c.set(c.get() - 1));
    }
}
pub fn committed() {
    CURRENT.with(|s| {
        if let Some(c) = s.borrow().as_ref() {
            c.commits.fetch_add(1, Ordering::AcqRel);
        }
    });
}
pub fn lock<T>(mutex: &std::sync::Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, String> {
    loop {
        check()?;
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err("execution lock poisoned".into())
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
        }
    }
}
