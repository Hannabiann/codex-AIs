use super::super::{append, open_journal, tests::directory, Journal};
use super::*;

#[test]
fn logs_rotation_keeps_open_handle_and_recovers_pending_zip() {
    let root = directory("rotation");
    let path = root.join("error.log");
    let journal = Mutex::new(Journal {
        path: path.clone(),
        file: open_journal(&root, "error.log").unwrap(),
        generation: generation(),
    });
    append(&journal, "before\n").unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("nested/журнал.log"), vec![b'x'; 2 * 1024 * 1024]).unwrap();
    let mut day = "2026-11-20".to_string();
    {
        let _guard = IO.lock().unwrap();
        rotate(&root, &mut day, "2026-11-21").unwrap();
    }
    assert_eq!(day, "2026-11-21");
    append(&journal, "after\n").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "after\n");
    // A previous compression stopped before publication; keep sources and retry.
    let pending = fs::read_dir(root.join("OLD_Log"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let stem = pending
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix(".pending-")
        .unwrap();
    fs::write(
        root.join("OLD_Log").join(format!(".{stem}.zip.tmp")),
        b"partial",
    )
    .unwrap();
    archive_pending(&root).unwrap();
    archive_pending(&root).unwrap();
    let archives = fs::read_dir(root.join("OLD_Log"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(archives.len(), 1);
    assert!(archives[0].path().extension().is_some_and(|e| e == "zip"));
    assert!(archives[0].metadata().unwrap().len() < 50_000);
    println!("ZIP_CHECK={}", archives[0].path().display());
}
#[test]
fn logs_retention_only_owns_dated_zip_segments() {
    let root = directory("retention");
    let old = old_root(&root).unwrap();
    for name in [
        "2026-11-18-1-123.zip",
        "2026-11-19-1-123.zip",
        "2026-11-25-1-123.zip",
        "manual.zip",
        "2026-11-01-custom.zip",
        ".pending-2026-11-01-1-123",
        "тестовый.zip",
    ] {
        fs::write(old.join(name), b"preserve unless expired managed ZIP").unwrap();
    }
    prune(&root, "2026-11-26", 0).unwrap();
    assert!(old.join("2026-11-18-1-123.zip").exists());
    prune(&root, "2026-11-26", 7).unwrap();
    assert!(!old.join("2026-11-18-1-123.zip").exists());
    for name in [
        "2026-11-19-1-123.zip",
        "2026-11-25-1-123.zip",
        "manual.zip",
        "2026-11-01-custom.zip",
        ".pending-2026-11-01-1-123",
        "тестовый.zip",
    ] {
        assert!(old.join(name).exists(), "{name}");
    }
}

#[test]
fn logs_rotation_and_compression_during_parallel_appends() {
    use std::sync::{Arc, Barrier};
    let root = directory("parallel-rotation");
    let path = root.join("parallel.log");
    let journal = Arc::new(Mutex::new(Journal {
        path: path.clone(),
        file: open_journal(&root, "parallel.log").unwrap(),
        generation: generation(),
    }));
    let boundary = Arc::new(Barrier::new(5));
    let workers = (0..4)
        .map(|id| {
            let journal = journal.clone();
            let boundary = boundary.clone();
            std::thread::spawn(move || {
                for n in 0..100 {
                    append(&journal, &format!("{id}:{n}\n")).unwrap();
                }
                boundary.wait();
                boundary.wait();
                for n in 100..200 {
                    append(&journal, &format!("{id}:{n}\n")).unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    boundary.wait();
    {
        let _guard = IO.lock().unwrap();
        rotate(&root, &mut "2026-11-20".into(), "2026-11-21").unwrap();
    }
    boundary.wait();
    archive_pending(&root).unwrap();
    for worker in workers {
        worker.join().unwrap();
    }
    let current = fs::read_to_string(path).unwrap();
    assert_eq!(current.lines().count(), 400);
    assert_eq!(
        current
            .lines()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        400
    );
    println!("PARALLEL_ZIP_CHECK={}", root.display());
}
