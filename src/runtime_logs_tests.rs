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

#[test]
fn multiline_ks_dumps_are_atomic_between_workers() {
    let root = directory("ks-diagnostic");
    let path = root.join("error.log");
    let journal = std::sync::Arc::new(Mutex::new(Journal {
        path: path.clone(), file: open_journal(&root,"error.log").unwrap(), generation: generation(),
    }));
    let threads = (0..6).map(|worker| {
        let journal = journal.clone();
        std::thread::spawn(move || {
            for n in 0..8 {
                let mut runtime = crate::ks::runtime::Runtime::new("/20a");
                let source = format!("const CODE=695;\nfunction mp_xCreate(code, description) {{\n    return {{Code:code, Description:description}};\n}}\nfunction fail(attrId) {{\n    var local=attrId+1;\n    throw mp_xCreate(CODE, 'Test attribute: '+attrId);\n}}\nfail({});", worker*100+n);
                let ast = runtime.parse_cached_named(&source,"diagnostic-fixture.ks").unwrap();
                assert!(runtime.render(&ast).is_err());
                let detail = runtime.take_error_diagnostic();
                let (_, record) = diagnostic_record("request KS", &format!("url=/20a?fixture&nd={} error=test",worker*100+n), &detail);
                append(&journal,&record).unwrap();
            }
        })
    }).collect::<Vec<_>>();
    for thread in threads { thread.join().unwrap(); }
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.matches("----- BEGIN KS DIAGNOSTIC -----").count(),48);
    assert_eq!(text.matches("----- END KS DIAGNOSTIC -----").count(),48);
    for block in text.split("----- END KS DIAGNOSTIC -----").take(48) {
        assert_eq!(block.matches("----- BEGIN KS DIAGNOSTIC -----").count(),1);
        assert!(block.contains("<< throw >> mp_xCreate"));
        assert!(block.contains("Code=695"));
        assert!(block.contains("Global variables and constants"));
        assert!(block.contains("Arguments (scalar values at call"));
    }
}

#[test]
fn diagnostic_url_keeps_reproduction_parameters_and_hides_secrets() {
    let url=diagnostic_url("/20a?ispravlenieForm&nd=123&prevdoc=456&attr=9&text=%D2%E5%F1%F2&%70assword=SECRET&access_token=TOKEN&sid=SESSION");
    assert!(url.contains("prevdoc=456&attr=9&text=%D2%E5%F1%F2"));
    for secret in ["SECRET","TOKEN","SESSION"] { assert!(!url.contains(secret)); }
    assert_eq!(url.matches("[hidden]").count(),3);
}
