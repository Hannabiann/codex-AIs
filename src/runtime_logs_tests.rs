use super::*;

pub(crate) fn directory(label: &str) -> PathBuf {
    let base = std::env::var_os("CODEX_LOG_TEST_OUTPUT")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = base.join(format!(
        "logs-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&path).unwrap();
    path.canonicalize().unwrap()
}
#[test]
fn logs_default_directory_and_safe_overrides() {
    let data = directory("paths");
    assert_eq!(prepare(&data, None).unwrap(), data.join(DIRECTORY));
    assert!(prepare(&data, Some(&data)).is_err());
    assert!(prepare(&data, Some(data.parent().unwrap())).is_err());
    assert!(prepare(&data, Some(&data.join("old"))).is_err());
    let external = directory("external");
    assert_eq!(prepare(&data, Some(&external)).unwrap(), external);
    assert!(prepare(&data, Some(Path::new(""))).is_err());
}
#[test]
fn logs_append_concurrently_and_reopen_without_truncation() {
    let root = directory("append");
    let path = root.join("error.log");
    let journal = std::sync::Arc::new(Mutex::new(Journal {
        path: path.clone(),
        file: open_journal(&root, "error.log").unwrap(),
        generation: generation(),
    }));
    let threads = (0..8)
        .map(|worker| {
            let journal = journal.clone();
            std::thread::spawn(move || {
                for n in 0..100 {
                    append(&journal, &format!("{worker}:{n}:ошибка\n")).unwrap();
                }
            })
        })
        .collect::<Vec<_>>();
    for thread in threads {
        thread.join().unwrap();
    }
    drop(journal);
    let s = std::fs::read_to_string(&path).unwrap();
    assert_eq!(s.lines().count(), 800);
    assert_eq!(
        s.lines().collect::<std::collections::HashSet<_>>().len(),
        800
    );
    let journal = Mutex::new(Journal {
        path: path.clone(),
        file: open_journal(&root, "error.log").unwrap(),
        generation: generation(),
    });
    append(&journal, "restart\n").unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), s + "restart\n");
    assert_eq!(field("one\ntwo\r\t\""), "one\\ntwo\\r\\t\\\"");
}
#[test]
fn logs_admin_replaces_only_native_log_controls() {
    let html="<form><table><TR><TD ALIGN=CENTER COLSPAN=2><FONT SIZE=+1><B>Протокол:</B></FONT></TD></TR><tr><td><input name=log value='private/access.log'><input name=klog value='private/error.log'></td></tr><TR><TD ALIGN=CENTER COLSPAN=2><FONT SIZE=+1><B>Почта:</B></FONT></TD></TR><tr><td><input name=smtp value='mail'></td></tr></table></form>";
    let changed = admin_html(html.into(), "pref");
    assert!(changed.contains("/Log-codex-AIS/"));
    assert!(!changed.contains("name=log"));
    assert!(!changed.contains("private/"));
    assert!(changed.contains("name=smtp value='mail'"));
    assert_eq!(admin_html(html.into(), "another"), html);
}
