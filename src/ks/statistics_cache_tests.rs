use super::*;

#[test]
fn statistics_cache_requires_authenticated_home_page() {
    for (target, auth, expected) in [
        ("/20a", true, true),
        ("/20a/?", true, true),
        ("/20a?default&mode=1", true, true),
        ("/20a?default&mode=6", true, true),
        ("/20a?default", false, false),
        ("/20a?adminsettings", true, false),
        ("/99?default", true, false),
        ("/20a?ispravlenieForm", true, false),
    ] {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        assert_eq!(
            request_allowed(path, query.split('&').next().unwrap(), "GET", auth, false),
            expected,
            "{target}"
        );
    }
    assert!(!request_allowed("/20a", "default", "HEAD", true, false));
    assert!(!request_allowed("/20a", "default", "GET", true, true));
}

#[test]
#[ignore = "Requires CODEX_TEST_DATA_ROOT; writes only an isolated config.db4 copy"]
fn statistics_cache_config_roundtrip_and_races() {
    let base = std::env::var("CODEX_TEST_DATA_ROOT").expect("CODEX_TEST_DATA_ROOT");
    let engine = crate::db::Db4Engine::init_from_config_root(&base).unwrap();
    let root = engine.registry.root;
    let directory = engine
        .registry
        .key(root)
        .unwrap()
        .children
        .iter()
        .copied()
        .find(|&id| {
            engine
                .registry
                .query(id, "Path")
                .unwrap()
                .is_some_and(|e| e.value.trim_end_matches('/') == "/20a")
        })
        .unwrap();
    let catalog = key_path(&engine.registry, directory).unwrap();
    let leaf = format!("{catalog}\\Настройки\\Статистики сервера\\Общие\\ПроверкаКэшаПоказателя");
    let temp = std::env::temp_dir().join(format!(
        "codex-cache-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&temp).unwrap();
    let file = temp.join("config.db4");
    std::fs::copy(std::path::Path::new(&base).join("config.db4"), &file).unwrap();
    let runtime = || {
        let reg = Transaction::open(&file, root).unwrap().registry;
        let mut rt = Runtime::new("/20a")
            .with_data_root(&temp.to_string_lossy())
            .with_registry(&reg)
            .with_application_config(&Default::default());
        rt.statistics_cache_allowed = true;
        rt
    };
    let stage = move |rt: &mut Runtime<'_>, path: &str, value: &str, date: &str| {
        rt.call_host(
            Value::RegistryMethod(root, "CreateKey".into()),
            vec![Value::String(path.into())],
            None,
            "CreateKey".into(),
        )
        .unwrap();
        let id = rt.registry.as_ref().unwrap().open(path).unwrap().unwrap();
        for (name, text) in [("Значение", value), ("Дата последнего обновления", date)]
        {
            rt.call_host(
                Value::RegistryMethod(id, "SetValue".into()),
                vec![
                    Value::String(name.into()),
                    Value::Number(1.0),
                    Value::String(text.into()),
                ],
                None,
                "SetValue".into(),
            )
            .unwrap();
        }
    };
    let pair = |path: &str| {
        let reg = Transaction::open(&file, root).unwrap().registry;
        let id = reg.open(path).unwrap().unwrap();
        (
            reg.query(id, "Значение").unwrap().unwrap().value.clone(),
            reg.query(id, "Дата последнего обновления")
                .unwrap()
                .unwrap()
                .value
                .clone(),
        )
    };
    let original = std::fs::read(&file).unwrap();
    let mut rt = runtime();
    let long = "значение ".repeat(80);
    stage(&mut rt, &leaf, &long, "26.11.2026 17:00:00");
    assert_eq!(std::fs::read(&file).unwrap(), original);
    rt.commit_statistics_cache().unwrap();
    rt.end_request();
    assert_eq!(pair(&leaf), (long, "26.11.2026 17:00:00".into()));
    let before_abort = std::fs::read(&file).unwrap();
    let mut rt = runtime();
    stage(&mut rt, &leaf, "discard", "discard");
    rt.end_request();
    assert_eq!(std::fs::read(&file).unwrap(), before_abort);
    let mut slow = runtime();
    let mut fast = runtime();
    stage(&mut slow, &leaf, "slow", "17:01:00");
    stage(&mut fast, &leaf, "fast", "17:02:00");
    fast.commit_statistics_cache().unwrap();
    slow.commit_statistics_cache().unwrap();
    assert_eq!(pair(&leaf), ("fast".into(), "17:02:00".into()));
    let mut late = runtime();
    let mut early = runtime();
    stage(&mut early, &leaf, "early", "17:02:01");
    early.commit_statistics_cache().unwrap();
    // The calculation began before the other publication, but reached its
    // first registry write afterwards. It must not overwrite the new cache.
    stage(&mut late, &leaf, "late", "17:02:02");
    late.commit_statistics_cache().unwrap();
    assert_eq!(pair(&leaf), ("early".into(), "17:02:01".into()));
    let mut rt = runtime();
    stage(&mut rt, &leaf, "merged", "17:03:00");
    let mut other = Transaction::open(&file, root).unwrap();
    other
        .set(
            root,
            "ПроверкаДругогоПараметра",
            "REG_SZ",
            Some("preserved"),
        )
        .unwrap();
    other.commit().unwrap();
    rt.commit_statistics_cache().unwrap();
    assert_eq!(pair(&leaf), ("merged".into(), "17:03:00".into()));
    assert_eq!(
        Transaction::open(&file, root)
            .unwrap()
            .registry
            .query(root, "ПроверкаДругогоПараметра")
            .unwrap()
            .unwrap()
            .value,
        "preserved"
    );
    assert!(!cache_path(&engine.registry, "/99", &leaf).unwrap());
    assert!(!cache_path(
        &engine.registry,
        "/20a",
        &format!("{catalog}\\Настройки\\Файловое хранилище\\2026\\10")
    )
    .unwrap());
    assert!(cache_path(&engine.registry, "/20a", &format!("{leaf}\\..\\escape")).is_err());
    let mut rt = runtime();
    let id = rt.registry.as_ref().unwrap().open(&leaf).unwrap().unwrap();
    assert!(!rt
        .statistics_cache_call(
            id,
            "SetValue",
            &[
                Value::String("Other".into()),
                Value::Number(1.0),
                Value::String("no".into())
            ]
        )
        .unwrap());
    assert!(!rt
        .statistics_cache_call(
            root,
            "SetValue",
            &[
                Value::String("HTTPPort".into()),
                Value::Number(4.0),
                Value::Number(1234.0)
            ]
        )
        .unwrap());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..4)
        .map(|i| {
            let temp = temp.clone();
            let leaf = format!("{leaf}-{i}");
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let reg = Transaction::open(&temp.join("config.db4"), root)
                    .unwrap()
                    .registry;
                let mut rt = Runtime::new("/20a")
                    .with_data_root(&temp.to_string_lossy())
                    .with_registry(&reg);
                rt.statistics_cache_allowed = true;
                stage(&mut rt, &leaf, "parallel", &i.to_string());
                barrier.wait();
                rt.commit_statistics_cache().unwrap();
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    for i in 0..4 {
        assert_eq!(
            pair(&format!("{leaf}-{i}")),
            ("parallel".into(), i.to_string())
        );
    }
    println!("Verified isolated config: {}", file.display());
}
