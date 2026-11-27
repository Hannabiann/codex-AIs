//! Only stateless court-exchange entry points may leave the At owner. General
//! At closures and ~mp_at still depend on that owner's KS globals/job queue.
use super::{RequestContext, RequestJob};
use std::{collections::{HashMap, VecDeque}, sync::{Arc, Mutex}};
use crate::monitor::{Monitor, State};
use tokio::sync::{mpsc, Semaphore};

pub(super) fn accepts(path: &str) -> bool {
    ["/20a/~sendDocsAuto", "/20a/~sendDocsAutoGet"]
        .iter().any(|p| path.eq_ignore_ascii_case(p))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase { Pending, Running, Cancelled, Finished }

pub(super) struct Execution(Arc<Mutex<Phase>>);
impl Execution {
    pub(super) fn start(&self) -> bool {
        let mut phase = self.0.lock().unwrap();
        if *phase != Phase::Pending { return false; }
        *phase = Phase::Running;
        true
    }
}
impl Drop for Execution {
    fn drop(&mut self) { *self.0.lock().unwrap() = Phase::Finished; }
}
struct Entry {
    id: u64,
    path: String,
    minute: Option<u16>,
    phase: Arc<Mutex<Phase>>,
}

#[derive(Default)]
pub(super) struct Pending {
    entries: HashMap<String, Entry>,
    waiting: VecDeque<RequestJob>,
}
impl Pending {
    pub(super) fn enqueue(&mut self, path: String, minute: Option<u16>, monitor: &Arc<Monitor>) {
        assert!(accepts(&path));
        let key = path.to_ascii_lowercase();
        if self.entries.get(&key).is_some_and(|e| *e.phase.lock().unwrap() != Phase::Finished) {
            return; // At most one outstanding send and one receive, including queue wait.
        }
        let id = monitor.start(&path, "At", true);
        monitor.group(id, "Тяжёлые");
        let phase = Arc::new(Mutex::new(Phase::Pending));
        self.entries.insert(key, Entry { id, path: path.clone(), minute, phase: phase.clone() });
        self.waiting.push_back(RequestJob {
            id,
            request: RequestContext { target: path, method: "At".into(), ..Default::default() },
            authorization: None,
            // An At job has no HTTP consumer. Only the private background marker
            // allows a worker to execute despite a closed response channel.
            reply: tokio::sync::oneshot::channel().0,
            control: Arc::new(Default::default()),
            queued: None,
            background: Some(Execution(phase)),
        });
    }

    pub(super) fn flush(&mut self, sender: &mpsc::Sender<RequestJob>, queue: &Arc<Semaphore>,
        monitor: &Arc<Monitor>, time: impl Fn(&str) -> Option<u16>) {
        self.entries.retain(|_, entry| {
            let mut phase = entry.phase.lock().unwrap();
            if *phase == Phase::Pending && entry.minute.is_some() && time(&entry.path) != entry.minute {
                *phase = Phase::Cancelled;
                monitor.finish(entry.id, "расписание изменено до исполнения", State::Cancelled);
            }
            *phase != Phase::Finished
        });
        while let Some(mut job) = self.waiting.pop_front() {
            if *job.background.as_ref().unwrap().0.lock().unwrap() == Phase::Cancelled { continue; }
            let Ok(permit) = queue.clone().try_acquire_owned() else {
                self.waiting.push_front(job);
                break;
            };
            job.queued = Some(permit);
            match sender.try_send(job) {
                Ok(()) => (),
                Err(mpsc::error::TrySendError::Full(mut job)) => {
                    job.queued.take();
                    self.waiting.push_front(job);
                    break;
                },
                Err(mpsc::error::TrySendError::Closed(job)) => {
                    monitor.finish(job.id, "очередь тяжёлых исполнителей закрыта", State::Error);
                    crate::runtime_logs::error("At", "Court exchange worker queue closed");
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn monitor() -> Arc<Monitor> {
        Arc::new(Monitor::new(Default::default(), crate::worker_config::Mode::Manual(4).resolve(1)))
    }
    #[test]
    fn scheduled_exchange_waits_for_shared_admission_without_using_word_queue() {
        let monitor = monitor();
        let layout = crate::pool_layout::Layout::new(4, 1).unwrap();
        let (workers, inboxes) = super::super::worker_channels(layout, 2);
        let queue = Arc::new(Semaphore::new(1));
        let permit = queue.clone().try_acquire_owned().unwrap();
        let mut p = Pending::default();
        let path = "/20a/~sendDocsAuto";
        p.enqueue(path.into(), Some(600), &monitor);
        p.enqueue(path.to_ascii_lowercase(), Some(600), &monitor);
        p.flush(&workers[3].sender, &queue, &monitor, |_| Some(600));
        assert_eq!(p.waiting.len(), 1);
        assert!(inboxes[0].lock().unwrap().try_recv().is_err());
        assert!(inboxes[3].lock().unwrap().try_recv().is_err());
        drop(permit);
        p.flush(&workers[3].sender, &queue, &monitor, |_| Some(600));
        let mut job = inboxes[3].lock().unwrap().try_recv().unwrap();
        assert!(job.reply.is_closed());
        drop(job.queued.take());
        assert!(job.background.as_ref().unwrap().start());
        p.enqueue(path.into(), Some(600), &monitor);
        assert!(p.waiting.is_empty());
        // Disabling a schedule cancels only unstarted work.
        p.flush(&workers[3].sender, &queue, &monitor, |_| None);
        assert_eq!(*job.background.as_ref().unwrap().0.lock().unwrap(), Phase::Running);
        drop(job);
        p.enqueue(path.into(), Some(600), &monitor);
        assert_eq!(p.waiting.len(), 1);
    }
    #[test]
    fn queued_exchange_is_cancelled_when_schedule_is_disabled_or_replaced() {
        for changed in [None, Some(601)] {
            for admitted in [false, true] {
                let monitor = monitor();
                let (tx, mut rx) = mpsc::channel(2);
                let queue = Arc::new(Semaphore::new(usize::from(admitted)));
                let mut p = Pending::default();
                p.enqueue("/20a/~sendDocsAutoGet".into(), Some(600), &monitor);
                p.flush(&tx, &queue, &monitor, |_| Some(600));
                p.flush(&tx, &queue, &monitor, |_| changed);
                if admitted {
                    let job = rx.try_recv().unwrap();
                    assert!(!job.background.as_ref().unwrap().start());
                } else {
                    assert!(rx.try_recv().is_err());
                    assert!(p.waiting.is_empty());
                }
            }
        }
    }
    #[test]
    fn full_channel_retries_and_keeps_fifo_order() {
        let monitor = monitor();
        let (tx, mut rx) = mpsc::channel(1);
        let queue = Arc::new(Semaphore::new(2));
        let mut p = Pending::default();
        p.enqueue("/20a/~sendDocsAuto".into(), None, &monitor);
        p.enqueue("/20a/~sendDocsAutoGet".into(), None, &monitor);
        p.flush(&tx, &queue, &monitor, |_| None);
        assert_eq!(p.waiting.len(), 1);
        assert_eq!(queue.available_permits(), 1);
        let first = rx.try_recv().unwrap();
        assert_eq!(first.request.target, "/20a/~sendDocsAuto");
        drop(first);
        p.flush(&tx, &queue, &monitor, |_| None);
        let second = rx.try_recv().unwrap();
        assert_eq!(second.request.target, "/20a/~sendDocsAutoGet");
        drop(second);
        assert_eq!(queue.available_permits(), 2);
        assert!(p.waiting.is_empty());
        for path in ["/20a/~mp_at", "/99/~sendDocsAuto", "/20a/~sendDocsAuto?x", "/20a?~sendDocsAuto"] {
            assert!(!accepts(path));
        }
    }
}
