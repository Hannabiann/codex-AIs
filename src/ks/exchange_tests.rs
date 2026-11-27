use super::*;

#[test]
fn automatic_exchange_requires_the_scheduler_target_and_does_not_leak() {
    for target in ["/20a/~sendDocsAuto", "/20a/~sendDocsAutoGet"] {
        assert!(scheduled_target(true, "/20a/", target));
        assert!(!scheduled_target(false, "/20a", target));
        assert!(!scheduled_target(true, "/99", target));
        assert!(!scheduled_target(true, "/20a", &format!("{target}?other")));
    }
    assert!(!scheduled_target(true, "/20a", "/20a/~mp_at"));
    let mut runtime = Runtime::new("/20a");
    runtime.exchange_access.scheduled = true;
    runtime.exchange_begin_write().unwrap();
    runtime.exchange_cleanup().unwrap();
    // Each configured recipient may initialize a new exchanger in the same job.
    runtime.exchange_begin_write().unwrap();
    runtime.end_request();
    assert!(!runtime.exchange_access.scheduled);
    assert!(runtime.exchange_begin_write().is_err());
}

#[test]
fn native_court_filename_join_has_no_implicit_comma() {
    let mut runtime = Runtime::new("/test");
    runtime.render(&super::super::super::parser::parse("if(['1','73'].join()!='173') throw 'default join'; if(['1','73'].join(',')!='1,73') throw 'explicit separator';").unwrap()).unwrap();
}

fn temporary(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "codex-exchange-{name}-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&p).unwrap();
    p.canonicalize().unwrap()
}

#[test]
fn failed_exchange_cleans_owned_workspace_but_keeps_incoming_packet() {
    let root = temporary("abort");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(root.join("incoming.7z"), b"incoming").unwrap();
    std::fs::write(work.join("partial.xml"), b"partial").unwrap();
    let mut runtime = Runtime::new("/20a");
    runtime.exchange_access.work = Some(work.clone());
    runtime.exchange_access.roots = vec![root.clone(), work.clone()];
    runtime.end_request();
    assert!(!work.exists());
    assert_eq!(std::fs::read(root.join("incoming.7z")).unwrap(), b"incoming");
    std::fs::remove_file(root.join("incoming.7z")).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn automatic_exchange_continues_other_courts_and_reports_partial_failure() {
    let mut runtime = Runtime::new("/20a");
    runtime.exchange_access.scheduled = true;
    runtime.render(&super::super::super::parser::parse(r#"
        var completed = 0;
        function autosend_court(oid, settings, filter) {
            if (oid == 1) unsupported_transport();
            if (oid == 2) throw 'recipient failed';
            completed++;
        }
        __codex_ExchangeAutoCourt(1,null,null);
        __codex_ExchangeAutoCourt(2,null,null);
        __codex_ExchangeAutoCourt(3,null,null);
        if (completed != 1) throw 'remaining court was skipped';
    "#).unwrap()).unwrap();
    let error = runtime.exchange_auto_finish().unwrap_err().to_string();
    assert!(error.contains("court=1") && error.contains("court=2"));
    runtime.end_request();
    assert!(runtime.exchange_auto_court(&[Value::Null, Value::Null, Value::Null]).is_err());
}

#[test]
fn exchange_files_are_scoped_and_require_write_authorization() {
    let root = temporary("scope");
    let work = root.join("work");
    std::fs::create_dir(&work).unwrap();
    let mut runtime = Runtime::new("/20a").with_data_root(root.to_str().unwrap());
    assert!(runtime.exchange_begin_write().is_err());
    runtime.exchange_access.request = true;
    runtime.exchange_begin_write().unwrap();
    assert!(runtime.document_write_allowed);
    runtime.exchange_access.work = Some(work.clone());
    runtime.exchange_access.roots = vec![work.clone()];
    assert_eq!(
        runtime
            .exchange_path(&work.join("a_0.xml").to_string_lossy(), true)
            .unwrap(),
        Some(work.join("a_0.xml"))
    );
    assert!(runtime
        .exchange_path(&root.join("config.db4").to_string_lossy(), true)
        .unwrap()
        .is_none());
    assert!(runtime
        .exchange_path(&work.join("x.db4").to_string_lossy(), true)
        .is_err());
    assert!(runtime
        .exchange_path(&format!("{}/../a_0.xml", work.display()), true)
        .is_err());
    runtime.exchange_cleanup().unwrap();
    assert!(!work.exists());
    assert!(root.exists());
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn exchange_delivery_publishes_complete_files_without_overwriting() {
    let root = temporary("delivery");
    let from = root.join("input.7z");
    let to = root.join("output.7z");
    let bytes = vec![0xA5; 170_003];
    std::fs::write(&from, &bytes).unwrap();
    archive::deliver(&from, &to).unwrap();
    assert_eq!(std::fs::read(&to).unwrap(), bytes);
    std::fs::write(&from, b"other").unwrap();
    assert!(archive::deliver(&from, &to).is_err());
    assert_eq!(std::fs::read(&to).unwrap(), bytes);
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    std::fs::remove_file(from).unwrap();
    std::fs::remove_file(to).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn automatic_settings_roundtrip_xml_characters() {
    let mut runtime = Runtime::new("/20a");
    let source="<settings xmlversion=\"1.2\"><court oid=\"42\"><nm_type value=\"Суд &amp; &quot;имя&quot;\"/></court></settings>";
    let encoded = encoding_rs::WINDOWS_1251.encode(source).0.into_owned();
    let values = runtime
        .exchange_auto_parse(&[InputStream::value(&encoded)])
        .unwrap();
    let xml = runtime.exchange_auto_xml(&[values]).unwrap();
    let text = runtime.member(xml, "xml".into()).unwrap().text();
    assert_eq!(
        xml_sax::events(source).unwrap(),
        xml_sax::events(&text).unwrap()
    );
    assert!(runtime
        .exchange_auto_parse(&[InputStream::value(b"<!DOCTYPE s><settings/>")])
        .is_err());
}

#[test]
fn pkg_reads_native_compression_and_rejects_corruption() {
    // Synthetic XML packed by the legacy Packer, not a database extract.
    let native = vec![
        54, 54, 54, 32, 75, 111, 100, 101, 107, 115, 32, 97, 114, 99, 104, 105, 118, 101, 32, 26,
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 217, 0, 0, 0, 1, 1, 184, 137, 184, 137, 3, 0,
        117, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 32, 0, 0, 0, 123, 93, 117,
        16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 8, 65, 95, 49, 46, 88, 77, 76, 0, 117, 16,
        85, 0, 210, 181, 255, 199, 3, 33, 8, 37, 27, 72, 52, 170, 194, 90, 115, 253, 201, 19, 8,
        96, 65, 84, 79, 43, 173, 245, 166, 55, 195, 12, 103, 130, 9, 6, 79, 7, 21, 149, 67, 101,
        16, 84, 71, 64, 253, 79, 16, 81, 21, 17, 138, 8, 35, 194, 136, 24, 49, 136, 90, 162, 22,
        168, 5, 106, 129, 26, 80, 5, 85, 80, 5, 85, 168, 66, 21, 170, 80, 207, 49, 9, 51, 211, 88,
        4, 0, 253, 127, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];
    let expected = [
        b"<test>".as_slice(),
        &b"hello test 123456789\n".repeat(200),
        b"</test>",
    ]
    .concat();
    assert_eq!(
        pkg::decode(&native).unwrap(),
        vec![("a_1.xml".into(), expected)]
    );
    let mut corrupt = native.clone();
    corrupt[110] ^= 1;
    assert!(pkg::decode(&corrupt).is_err());
    assert!(pkg::decode(&native[..native.len() - 1]).is_err());
}
#[test]
fn pkg_writes_multiple_blocks_and_validates_names() {
    let files = vec![
        ("a_0.xml".into(), vec![65; 90000]),
        ("a_1.xml".into(), b"<document/>".to_vec()),
    ];
    let bytes = pkg::encode(&files).unwrap();
    assert_eq!(pkg::decode(&bytes).unwrap(), files);
    assert!(pkg::encode(&[("../a_0.xml".into(), vec![65])]).is_err());
    let mut bad = bytes;
    bad[89] = b'/';
    assert!(pkg::decode(&bad).is_err());
}
#[test]
fn exchange_rejects_protocol_and_path_injection() {
    assert_eq!(
        wire::Endpoint::parse("smtp://localhost:2525", "smtp", 25, 465)
            .unwrap()
            .port,
        2525
    );
    assert!(
        wire::Endpoint::parse("smtp+starttls://localhost", "smtp", 25, 465)
            .unwrap()
            .starttls
    );
    assert!(
        wire::Endpoint::parse("smtps://example.org", "smtp", 25, 465)
            .unwrap()
            .tls
    );
    for name in [
        "../a.7z",
        "a.7z\r\nDELE 1",
        "-a.pkg",
        "a.7z:stream",
        "a.7z.000",
    ] {
        assert!(!packet_name(name));
    }
    assert!(packet_name("a_1.7z.001"));
    assert!(wire::clean("user\r\nPASS secret").is_err());
    assert!(ftp::path("/safe/../other").is_err());
}
#[test]
fn exchange_mail_parses_marker_and_binary_attachment() {
    let raw=b"Subject: packet>1>1\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=boundary\r\n\r\n--boundary\r\nContent-Type: text/plain; charset=windows-1251\r\n\r\n *** ID = SENDDOCSIDENTIFICATIONLABEL ***\r\n--boundary\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"attach_0_1.7z\"\r\nContent-Transfer-Encoding: base64\r\n\r\nAAECA/8=\r\n--boundary--\r\n";
    let message = mail::parse(raw).unwrap().unwrap();
    assert_eq!(message.files[0].1, vec![0, 1, 2, 3, 255]);
    assert_eq!((message.index, message.count), (1, 1));
    assert!(mail::parse(b"Subject: unrelated\r\n\r\nordinary mail")
        .unwrap()
        .is_none());
    let malformed = String::from_utf8_lossy(raw).replace("attach_0_1.7z", "../wrong.7z");
    assert!(mail::parse(malformed.as_bytes()).is_err());
}

#[test]
fn smtp_auth_and_data_acknowledgement_use_real_protocol_replies() {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut stream = BufReader::new(socket);
        fn reply(stream: &mut BufReader<std::net::TcpStream>, text: &str) {
            stream.get_mut().write_all(text.as_bytes()).unwrap();
        }
        fn line(stream: &mut BufReader<std::net::TcpStream>) -> String {
            let mut s = String::new();
            stream.read_line(&mut s).unwrap();
            s
        }
        reply(&mut stream, "220 local test\r\n");
        assert_eq!(line(&mut stream), "EHLO codex-ais\r\n");
        reply(&mut stream, "250-localhost\r\n250 AUTH LOGIN\r\n");
        assert_eq!(line(&mut stream), "AUTH LOGIN\r\n");
        reply(&mut stream, "334 VXNlcg==\r\n");
        assert_eq!(line(&mut stream), "dGVzdA==\r\n");
        reply(&mut stream, "334 UGFzcw==\r\n");
        assert_eq!(line(&mut stream), "bG9jYWwtb25seQ==\r\n");
        reply(&mut stream, "235 OK\r\n");
        assert_eq!(line(&mut stream), "MAIL FROM:<from@example.test>\r\n");
        reply(&mut stream, "250 OK\r\n");
        assert_eq!(line(&mut stream), "RCPT TO:<to@example.test>\r\n");
        reply(&mut stream, "250 OK\r\n");
        assert_eq!(line(&mut stream), "DATA\r\n");
        reply(&mut stream, "354 Data\r\n");
        let mut body = String::new();
        loop {
            let s = line(&mut stream);
            if s == ".\r\n" {
                break;
            }
            assert!(!s.is_empty());
            body.push_str(&s);
        }
        let message = mail::parse(body.as_bytes()).unwrap().unwrap();
        assert_eq!(message.files[0].1, b"packet bytes");
        reply(&mut stream, "250 Accepted\r\n");
        // Deliberately close instead of replying to QUIT; DATA was accepted.
        assert_eq!(line(&mut stream), "QUIT\r\n");
    });
    mail::send(
        &mail::Settings {
            smtp: format!("127.0.0.1:{port}"),
            pop: String::new(),
            from: "from@example.test".into(),
            user: "test".into(),
            password: "local-only".into(),
        },
        "to@example.test",
        "unique-packet>1>1",
        "a.7z",
        b"packet bytes",
    )
    .unwrap();
    server.join().unwrap();
}

#[test]
fn stalled_exchange_connection_observes_request_cancellation() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let control = std::sync::Arc::new(crate::execution::Control::default());
    let other = control.clone();
    let (finished, release) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(75));
        other.cancel();
        let _ = release.recv_timeout(std::time::Duration::from_secs(5));
    });
    let _scope = crate::execution::Scope::enter(control);
    let mut wire = wire::Wire::connect(
        &wire::Endpoint::parse(&format!("127.0.0.1:{port}"), "smtp", 25, 465).unwrap(),
    )
    .unwrap();
    let start = std::time::Instant::now();
    let error = wire.line().unwrap_err();
    assert!(format!("{error:?}").contains("Запрос отменён"));
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
    let _ = finished.send(());
    server.join().unwrap();
}
