use super::*;
use rustls::pki_types::PrivateKeyDer;
use std::{net::TcpListener, path::PathBuf};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src/ks/testdata/exchange-tls")
        .join(name)
}
fn server_config(
    expired: bool,
    version: &'static rustls::SupportedProtocolVersion,
) -> Arc<rustls::ServerConfig> {
    let name = if expired { "expired" } else { "server" };
    let cert = CertificateDer::from_pem_file(fixture(&format!("{name}.pem"))).unwrap();
    let key = PrivateKeyDer::from_pem_file(fixture(&format!("{name}.key"))).unwrap();
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[version])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap(),
    )
}
fn accept(
    listener: &TcpListener,
    config: &Arc<rustls::ServerConfig>,
) -> rustls::StreamOwned<rustls::ServerConnection, TcpStream> {
    let (socket, _) = listener.accept().unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    rustls::StreamOwned::new(
        rustls::ServerConnection::new(config.clone()).unwrap(),
        socket,
    )
}
fn endpoint(listener: &TcpListener) -> Endpoint {
    Endpoint::parse(
        &format!("127.0.0.1:{}", listener.local_addr().unwrap().port()),
        "ftp",
        21,
        990,
    )
    .unwrap()
}

#[test]
fn tls12_and_tls13_data_channels_resume_and_exchange_clean_eof() {
    for version in [&rustls::version::TLS12, &rustls::version::TLS13] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint(&listener);
        let config = server_config(false, version);
        let server = std::thread::spawn(move || {
            let mut control = accept(&listener, &config);
            let mut hello = [0; 6];
            control.read_exact(&mut hello).unwrap();
            assert_eq!(&hello, b"HELLO\n");
            control.write_all(b"200 ready\r\n").unwrap();
            control.flush().unwrap();
            let mut download = accept(&listener, &config);
            download.write_all(b"packet").unwrap();
            download.flush().unwrap();
            assert_eq!(
                download.conn.handshake_kind(),
                Some(rustls::HandshakeKind::Resumed)
            );
            download.conn.send_close_notify();
            download.flush().unwrap();
            assert_eq!(download.read(&mut [0; 1]).unwrap(), 0);
            let mut upload = accept(&listener, &config);
            let mut data = Vec::new();
            upload.read_to_end(&mut data).unwrap();
            assert_eq!(data, vec![123; 2 * 1024 * 1024]);
            assert_eq!(
                upload.conn.handshake_kind(),
                Some(rustls::HandshakeKind::Resumed)
            );
            upload.conn.send_close_notify();
            upload.flush().unwrap();
        });
        let mut control = Wire::connect(&endpoint).unwrap();
        control
            .tls_with_config("localhost", tls_config(Some(&fixture("ca.pem"))).unwrap())
            .unwrap();
        control.send(b"HELLO\n").unwrap();
        control.expect(&[200]).unwrap();
        let mut download = Wire::connect(&endpoint).unwrap();
        download.tls_from(&control, "localhost").unwrap();
        assert_eq!(download.bytes(100).unwrap(), b"packet");
        download.finish().unwrap();
        let mut upload = Wire::connect(&endpoint).unwrap();
        upload.tls_from(&control, "localhost").unwrap();
        upload.send(&vec![123; 2 * 1024 * 1024]).unwrap();
        upload.finish().unwrap();
        server.join().unwrap();
    }
}

#[test]
fn tls_certificate_errors_prevent_application_data() {
    for (trusted, host, expired, expected) in [
        (false, "localhost", false, "UnknownIssuer"),
        (true, "other.example.test", false, "not valid for name"),
        (true, "localhost", true, "Expired"),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = endpoint(&listener);
        let config = server_config(expired, &rustls::version::TLS13);
        let server = std::thread::spawn(move || {
            let mut stream = accept(&listener, &config);
            assert!(
                stream.read(&mut [0; 128]).is_err(),
                "credentials must not reach an invalid TLS peer"
            );
        });
        let mut wire = Wire::connect(&endpoint).unwrap();
        let ca = fixture("ca.pem");
        let config = tls_config(if trusted { Some(&ca) } else { None }).unwrap();
        wire.tls_with_config(host, config).unwrap();
        let error = wire.command("USER test-only").unwrap_err().to_string();
        assert!(
            error.to_lowercase().contains(&expected.to_lowercase()),
            "{error}"
        );
        drop(wire);
        server.join().unwrap();
    }
}

#[test]
fn tls_rejects_truncated_stream_without_close_notify() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = endpoint(&listener);
    let config = server_config(false, &rustls::version::TLS13);
    let server = std::thread::spawn(move || {
        let mut stream = accept(&listener, &config);
        stream.write_all(b"partial packet").unwrap();
        stream.flush().unwrap();
        // Deliberately omit close_notify: EOF cannot certify a complete file.
    });
    let mut wire = Wire::connect(&endpoint).unwrap();
    wire.tls_with_config("localhost", tls_config(Some(&fixture("ca.pem"))).unwrap())
        .unwrap();
    assert!(wire.bytes(100).is_err());
    server.join().unwrap();
}

#[test]
fn tls_handshake_remains_cancellable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = endpoint(&listener);
    let control = Arc::new(crate::execution::Control::default());
    let other = control.clone();
    let (release, wait) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (_socket, _) = listener.accept().unwrap();
        std::thread::sleep(Duration::from_millis(75));
        other.cancel();
        let _ = wait.recv_timeout(Duration::from_secs(5));
    });
    let _scope = crate::execution::Scope::enter(control);
    let mut wire = Wire::connect(&endpoint).unwrap();
    wire.tls_with_config("localhost", tls_config(None).unwrap())
        .unwrap();
    let start = Instant::now();
    let error = wire.command("HELLO").unwrap_err().to_string();
    assert!(error.contains("Запрос отменён"));
    assert!(start.elapsed() < Duration::from_secs(4));
    let _ = release.send(());
    server.join().unwrap();
}
