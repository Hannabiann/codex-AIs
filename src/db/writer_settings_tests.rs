use super::*;
use std::path::PathBuf;

#[test]
fn information_revision_setting_is_restricted_to_its_native_format() {
    assert!(valid_value("_infSystemSetup", "$Revision: 44579 $"));
    assert!(!valid_value("InfSystem", "$Revision: 44579 $"));
    assert!(!valid_value("_infSystemSetup", "$Revision: bad $"));
    assert!(!valid_value("_infSystemSetup", "$Revision:  $"));
    assert!(valid_value("InfSystem", "0;1;2;3;4;5;6;7;8"));
}

fn fixture() -> (PathBuf, u32, u32, crate::db::registry::Registry) {
    let base = std::env::var("CODEX_TEST_DATA_ROOT").expect("CODEX_TEST_DATA_ROOT");
    let engine = crate::db::Db4Engine::init_from_config_root(&base).unwrap();
    let root = engine.registry.root;
    let parent = engine
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
    let dir = std::env::temp_dir().join(format!(
        "codex-duplicates-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.db4");
    std::fs::copy(Path::new(&base).join("config.db4"), &path).unwrap();
    (path, root, parent, engine.registry.clone())
}

#[test]
#[ignore = "Requires CODEX_TEST_DATA_ROOT with a duplicate-setting configuration"]
fn settings_duplicates_read_native_precedence_and_preserve_file() {
    let (path, _, parent, registry) = fixture();
    let before = std::fs::read(&path).unwrap();
    let mut names = crate::vm::distribution::setting_names();
    names.push("AccessByUserForMakeVacation");
    let got = read(&path, parent, &names).unwrap();
    let mut duplicates = 0;
    let mut index = MainKey::open(&path).unwrap();
    for name in names {
        let records = find_all(&mut index, parent, name).unwrap();
        if records.len() > 1 {
            duplicates += 1;
            println!(
                "Duplicate parameter: {name}, active records: {}",
                records.len()
            );
        }
        assert_eq!(
            got.iter().find(|e| e.name.eq_ignore_ascii_case(name)),
            registry.query(parent, name).unwrap()
        );
    }
    assert!(
        duplicates > 0,
        "This regression fixture must contain duplicate directory settings"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    println!(
        "Read {} parameters; {duplicates} duplicate names; source bytes unchanged",
        got.len()
    );
}

#[test]
#[ignore = "Requires CODEX_TEST_DATA_ROOT; changes only an isolated config.db4 copy"]
fn settings_duplicates_save_delete_restart_and_stale_form() {
    let (path, root, parent, _) = fixture();
    let mut index = MainKey::open(&path).unwrap();
    let name = crate::vm::distribution::setting_names()
        .into_iter()
        .find(|name| find_all(&mut index, parent, name).unwrap().len() > 1)
        .expect("duplicate fixture");
    let before = find_all(&mut index, parent, name).unwrap();
    let old = crate::db::registry::string_value(&before[0]).unwrap();
    let kind = crate::db::decode(before[0].string(5).unwrap());
    let next = if old == "1" { "0" } else { "1" };
    let snapshot = std::fs::read(&path).unwrap();
    assert!(save(
        &path,
        parent,
        &[Update {
            name: name.into(),
            expected: Some("stale".into()),
            value: Some(next.into()),
            kind: kind.clone()
        }]
    )
    .unwrap_err()
    .contains("Настройки изменились"));
    assert_eq!(std::fs::read(&path).unwrap(), snapshot);
    save(
        &path,
        parent,
        &[Update {
            name: name.into(),
            expected: Some(old),
            value: Some(next.into()),
            kind: kind.clone(),
        }],
    )
    .unwrap();
    let mut reopened = MainKey::open(&path).unwrap();
    let after = find_all(&mut reopened, parent, name).unwrap();
    assert_eq!(after.len(), before.len());
    for (old, new) in before.iter().zip(&after) {
        assert_eq!(old.integer(1024), new.integer(1024));
        assert_eq!(old.integer(3), new.integer(3));
        assert_eq!(old.string(1), new.string(1));
        assert_eq!(crate::db::registry::string_value(new).unwrap(), next);
        assert_eq!(new.string(5), Some(kind.as_bytes()));
    }
    // A fresh Registry and the indexed settings reader must agree after reopen.
    let reg = super::super::registry::Transaction::open(&path, root)
        .unwrap()
        .registry;
    assert_eq!(
        read(&path, parent, &[name]).unwrap().first(),
        reg.query(parent, name).unwrap()
    );
    let stable = std::fs::read(&path).unwrap();
    save(
        &path,
        parent,
        &[Update {
            name: name.into(),
            expected: Some(next.into()),
            value: Some(next.into()),
            kind: kind.clone(),
        }],
    )
    .unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), stable);
    save(
        &path,
        parent,
        &[Update {
            name: name.into(),
            expected: Some(next.into()),
            value: None,
            kind,
        }],
    )
    .unwrap();
    assert!(read(&path, parent, &[name]).unwrap().is_empty());
    let mut reopened = MainKey::open(&path).unwrap();
    for record in before {
        assert!(reopened
            .lookup(record.integer(1024).unwrap())
            .unwrap()
            .is_none());
    }
    println!(
        "Duplicate save, no-op, stale form and delete verified on {}",
        path.display()
    );
}

#[test]
#[ignore = "Builds a duplicate-setting config fixture; CODEX_DUPLICATE_FIXTURE must name a new file"]
fn settings_duplicates_fixture_for_http() {
    let output =
        PathBuf::from(std::env::var("CODEX_DUPLICATE_FIXTURE").expect("CODEX_DUPLICATE_FIXTURE"));
    assert!(!output.exists());
    let (path, _, parent, _) = fixture();
    let old = Arc::new(std::fs::read(&path).unwrap());
    let mut index = MainKey::from_image(old.clone()).unwrap();
    let name = "ReestrJudeRaspred";
    let current = find_all(&mut index, parent, name).unwrap();
    assert_eq!(current.len(), 1, "Use a clean single-value fixture");
    let mut copy = current[0].clone();
    let next = index.max_oid().unwrap().unwrap() + 1;
    Arc::make_mut(&mut copy.fields)
        .iter_mut()
        .find(|f| f.id == 1024)
        .unwrap()
        .value = Value::Integer(next);
    let image = if u16at(&old, 5) == 0x612 {
        legacy(&mut index, &old, &[copy], &[], &BTreeSet::new()).unwrap()
    } else {
        index_delta::candidate_from_image(&path, &[copy], &[], old.clone()).unwrap()
    };
    let mut verify = MainKey::from_image(Arc::new(image.clone())).unwrap();
    assert_eq!(find_all(&mut verify, parent, name).unwrap().len(), 2);
    std::fs::write(&output, image).unwrap();
    println!("HTTP regression config: {}", output.display());
}
