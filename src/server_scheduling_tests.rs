use super::*;

fn job(id: u64) -> RequestJob {
    RequestJob {
        id,
        request: RequestContext::default(),
        authorization: None,
        reply: tokio::sync::oneshot::channel().0,
        control: Arc::new(Default::default()),
        queued: Arc::new(tokio::sync::Semaphore::new(1))
            .try_acquire_owned()
            .unwrap(),
    }
}

fn send(workers: &[Worker], index: usize, id: u64) {
    assert!(workers[index].sender.try_send(job(id)).is_ok());
}

fn dispatcher(count: usize, limit: usize) -> (Dispatcher, Vec<WorkerInbox>) {
    let (workers, inboxes) = worker_channels(
        crate::pool_layout::Layout::new(count, 1).unwrap(),
        limit + 1,
    );
    (
        Dispatcher {
            layout: crate::pool_layout::Layout::new(count, 1).unwrap(),
            workers,
            queue: Arc::new(tokio::sync::Semaphore::new(limit)),
            ais_paths: vec!["/court".into()],
            owner_reserve: Arc::new(tokio::sync::Semaphore::new(1)),
            interface_reserve: Arc::new(tokio::sync::Semaphore::new(1)),
            static_cache: Arc::new(static_cache::Cache::default()),
            static_checks: Arc::new(tokio::sync::Semaphore::new(4)),
        },
        inboxes,
    )
}

fn submit(dispatcher: &Dispatcher, id: u64, target: &str) -> Result<(), ()> {
    dispatcher.submit(
        id,
        RequestContext {
            method: "GET".into(),
            target: target.into(),
            ..Default::default()
        },
        None,
        tokio::sync::oneshot::channel().0,
        Arc::new(Default::default()),
    )
}

#[test]
fn configured_pool_has_one_shared_bulk_receiver_for_one_to_32_workers() {
    for count in 1..=32 {
        let (dispatcher, inboxes) = dispatcher(count, 4);
        for index in 0..count {
            let owner = if index >= 4 { 3 } else { index };
            assert!(Arc::ptr_eq(&inboxes[index], &inboxes[owner]));
            if index > 0 && index < 4 {
                assert!(!Arc::ptr_eq(&inboxes[index], &inboxes[index - 1]));
            }
        }
        let bulk = match count {
            1 => 0,
            2 => 1,
            3 => 2,
            _ => 3,
        };
        assert!(submit(&dispatcher, 1, "/court?StatOtchet221").is_ok());
        assert!(submit(&dispatcher, 2, "/court?DeleteObject").is_ok());
        assert_eq!(next_job(&inboxes[bulk], None, None).unwrap().id, 1);
        assert_eq!(next_job(&inboxes[bulk], None, None).unwrap().id, 2);
    }
}

#[test]
fn idle_bulk_workers_take_reports_and_deletions_in_shared_fifo_order() {
    let (dispatcher, inboxes) = dispatcher(6, 8);
    for (id, target) in [
        (1, "/court?StatOtchet221"),
        (2, "/court?DeleteObject"),
        (3, "/court?StatOtchet221"),
        (4, "/court?DeleteObject"),
    ] {
        assert!(submit(&dispatcher, id, target).is_ok());
    }
    let running: Vec<_> = (3..6)
        .map(|i| next_job(&inboxes[i], None, None).unwrap())
        .collect();
    assert_eq!(
        running.iter().map(|job| job.id).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    // A worker chooses again after completing its job; no type quota or preemption.
    drop(running);
    assert_eq!(next_job(&inboxes[5], None, None).unwrap().id, 4);
}

#[test]
fn bulk_fifo_precedes_search_and_interface_help() {
    let (workers, inboxes) = worker_channels(crate::pool_layout::Layout::new(6, 1).unwrap(), 4);
    for (index, id) in [(3, 1), (2, 2), (1, 3)] {
        send(&workers, index, id);
    }
    for id in 1..=3 {
        assert_eq!(
            next_job(&inboxes[5], Some(&inboxes[2]), Some(&inboxes[1]))
                .unwrap()
                .id,
            id
        );
    }
    send(&workers, 3, 4);
    assert!(next_job(&inboxes[0], None, None).is_err());
    assert!(next_job(&inboxes[1], None, None).is_err());
    assert!(next_job(&inboxes[2], None, Some(&inboxes[1])).is_err());
}

#[test]
fn full_bulk_queue_preserves_owner_and_interface_admission_reserves() {
    let (dispatcher, inboxes) = dispatcher(6, 1);
    assert!(submit(&dispatcher, 1, "/court?StatOtchet221").is_ok());
    assert!(submit(&dispatcher, 2, "/court?DeleteObject").is_err());
    assert!(submit(&dispatcher, 3, "/court?d").is_ok());
    assert!(submit(&dispatcher, 4, "/99?getfile").is_ok());
    let cancelled = next_job(&inboxes[4], None, None).unwrap();
    assert!(cancelled.reply.is_closed());
    drop(cancelled);
    assert_eq!(dispatcher.queue.available_permits(), 1);
    assert!(submit(&dispatcher, 5, "/court?DeleteObject").is_ok());
    assert!(submit(&dispatcher, 6, "/court?StatOtchet221").is_err());
}

#[test]
fn route_classification_keeps_search_and_owner_separate_from_shared_bulk() {
    let request = RequestContext {
        method: "GET".into(),
        ..Default::default()
    };
    assert_eq!(lane("/20a?DeleteObject", &request), Lane::Heavy);
    assert_eq!(lane("/20a?StatOtchet221", &request), Lane::Heavy);
    assert_eq!(lane("/20a?pfind", &request), Lane::Search);
    assert_eq!(lane("/20a?ispravlenieForm", &request), Lane::Interface);
    assert_eq!(lane("/99?signdoc", &request), Lane::Owner);
    assert_eq!(lane("/20a?unknownSave", &request), Lane::Owner);
}

#[test]
fn multiple_interface_executors_share_ui_fifo_but_not_owner_or_search() {
    for ui in 2..=8 {
        let layout = crate::pool_layout::Layout::new(ui + 5, ui).unwrap();
        let (workers, inboxes) = worker_channels(layout, 16);
        let d = Dispatcher {
            layout,
            workers,
            queue: Arc::new(tokio::sync::Semaphore::new(16)),
            ais_paths: vec!["/court".into()],
            owner_reserve: Arc::new(tokio::sync::Semaphore::new(1)),
            interface_reserve: Arc::new(tokio::sync::Semaphore::new(1)),
            static_cache: Arc::new(static_cache::Cache::default()),
            static_checks: Arc::new(tokio::sync::Semaphore::new(4)),
        };
        for id in 1..=ui as u64 {
            assert!(submit(&d, id, "/court?d").is_ok());
        }
        assert!(submit(&d, 90, "/99?signdoc").is_ok());
        assert!(submit(&d, 91, "/court?pfind").is_ok());
        assert!(submit(&d, 92, "/court?DeleteObject").is_ok());
        for i in 1..=ui {
            assert!(Arc::ptr_eq(&inboxes[i], &inboxes[1]));
            assert_eq!(next_job(&inboxes[i], None, None).unwrap().id, i as u64);
        }
        assert_eq!(next_job(&inboxes[0], None, None).unwrap().id, 90);
        assert_eq!(
            next_job(&inboxes[layout.search], None, None).unwrap().id,
            91
        );
        assert_eq!(
            next_job(&inboxes[layout.total - 1], None, None).unwrap().id,
            92
        );
    }
}

#[test]
fn interface_busy_count_keeps_other_concurrent_executor_busy() {
    let count = std::sync::atomic::AtomicUsize::new(0);
    let a = InterfaceBusy::enter(&count);
    let b = InterfaceBusy::enter(&count);
    assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), 2);
    drop(a);
    assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), 1);
    drop(b);
    assert_eq!(count.load(std::sync::atomic::Ordering::Acquire), 0);
}
