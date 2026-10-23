use super::*;
use fakes::Deadline;
use fakes::MUST_SUCCEED_WITHIN;
use std::io::{BufRead, BufReader};
use std::net::TcpListener;
use std::sync::{
    OnceLock,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

pub(crate) fn trusted() -> Arc<ClientConfig> {
    configured(true, test_roots())
}

fn test_roots() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    let cert =
        rustls::pki_types::CertificateDer::from_pem_slice(include_bytes!("../../fixtures/ca.pem"))
            .unwrap();
    roots.add(cert).unwrap();
    roots
}
pub(crate) fn server_config() -> Arc<rustls::ServerConfig> {
    Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![rustls::pki_types::CertificateDer::from(
                    include_bytes!("../../fixtures/leaf.der").as_slice(),
                )],
                rustls::pki_types::PrivateKeyDer::try_from(
                    include_bytes!("../../fixtures/leaf.key.der").as_slice(),
                )
                .unwrap(),
            )
            .unwrap(),
    )
}
use rustls::pki_types::pem::PemObject;

#[test]
fn platform_verifier_advertises_the_crypto_providers_signature_schemes() {
    assert_eq!(
        Platform.supported_verify_schemes(),
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    );
}

#[test]
fn an_abrupt_tls_eof_is_reported_without_another_tcp_read() {
    fakes::within(
        "abrupt TLS EOF",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (close, close_wait) = std::sync::mpsc::channel::<()>();
            let (done, finished) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let (mut tcp, _) = listener.accept().unwrap();
                tcp.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
                tcp.set_write_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
                let mut tls = rustls::ServerConnection::new(server_config()).unwrap();
                while tls.is_handshaking() {
                    tls.complete_io(&mut tcp).unwrap();
                }
                match close_wait.recv() {
                    Ok(()) | Err(_) => {}
                }
                tcp.shutdown(std::net::Shutdown::Both).unwrap();
                done.send(()).unwrap();
            });
            let via = super::super::Via {
                tls: trusted(),
                proxy: None,
                ..super::super::Via::from_env()
            };
            let endpoint = super::super::Endpoint {
                secure: true,
                host: "127.0.0.1".into(),
                port: addr.port(),
                target: "/".into(),
            };
            let mut socket = Socket::new(
                TcpStream::connect(addr).unwrap(),
                Vec::new(),
                &endpoint,
                &via,
            )
            .unwrap();
            close.send(()).unwrap();
            assert_eq!(
                socket.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::UnexpectedEof
            );
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&finished, "TLS peer closed without close_notify");
        },
    );
}

#[test]
fn tls_socket_echoes_text_with_the_test_ca() {
    fakes::within(
        "TLS websocket echo",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let (done, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let (tcp, _) = listener.accept().unwrap();
                tcp.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
                tcp.set_write_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server_config()).unwrap(),
                    tcp,
                );
                let (head, _) = super::super::handshake::headers(&mut stream).unwrap();
                let key = head
                    .lines()
                    .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
                    .unwrap();
                let accept = super::super::handshake::accept(key);
                let mut reply=format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").into_bytes();
                reply.extend_from_slice(b"\x81\x04echo");
                stream.write_all(&reply).unwrap();
                let mut frame = [0; 10];
                stream.read_exact(&mut frame).unwrap();
                assert_eq!(frame[0], 0x81);
                assert_eq!(frame[1], 0x84);
                let bytes: Vec<_> = frame[6..]
                    .iter()
                    .zip(frame[2..6].iter().cycle())
                    .map(|(b, m)| b ^ m)
                    .collect();
                assert_eq!(bytes, b"back");
                done.send(()).unwrap();
            });
            let mut via = super::super::Via::from_env();
            via.tls = trusted();
            via.proxy = None;
            let ep = super::super::Endpoint {
                secure: true,
                host: "127.0.0.1".into(),
                port: addr.port(),
                target: "/".into(),
            };
            let mut stream = super::super::connect_with(&ep, &via).unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line, "echo\n");
            stream.write_all(b"back\n").unwrap();
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&rx, "TLS peer read the client frame");
        },
    );
}

#[test]
fn tls_records_larger_than_the_deframer_buffer_keep_every_byte() {
    fakes::within(
        "large TLS websocket message",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (done, completed) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let (tcp, _) = listener.accept().unwrap();
                tcp.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
                tcp.set_write_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server_config()).unwrap(),
                    tcp,
                );
                let (head, _) = super::super::handshake::headers(&mut stream).unwrap();
                let key = head
                    .lines()
                    .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
                    .unwrap();
                let accept = super::super::handshake::accept(key);
                let mut reply = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n").into_bytes();
                reply.extend_from_slice(&[0x81, 127]);
                reply.extend_from_slice(&262144u64.to_be_bytes());
                reply.extend(std::iter::repeat_n(b'x', 262144));
                stream.write_all(&reply).unwrap();
                done.send(()).unwrap();
            });
            let via = super::super::Via {
                tls: trusted(),
                proxy: None,
                ..super::super::Via::from_env()
            };
            let endpoint = super::super::Endpoint {
                secure: true,
                host: "127.0.0.1".into(),
                port,
                target: "/".into(),
            };
            let mut stream = super::super::connect_with(&endpoint, &via).unwrap();
            let mut line = Vec::new();
            stream.read_to_end(&mut line).unwrap();
            assert_eq!(line.len(), 262145);
            assert_eq!(line.last(), Some(&b'\n'));
            assert!(line[..262144].iter().all(|byte| *byte == b'x'));
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&completed, "TLS server sent all records");
        },
    );
}

/// How long the server reads after the client rejects its certificate; no
/// application data is expected.
const NO_DATA_AFTER_REJECTION: Duration = Duration::from_secs(2);

#[test]
fn platform_verifier_refuses_the_private_test_ca() {
    fakes::within(
        "untrusted TLS certificate",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            std::thread::spawn(move || {
                let (tcp, _) = listener.accept().unwrap();
                tcp.set_read_timeout(Some(NO_DATA_AFTER_REJECTION)).unwrap();
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(server_config()).unwrap(),
                    tcp,
                );
                let mut b = [0];
                match stream.read(&mut b) {
                    Ok(_) | Err(_) => {}
                }
            });
            let via = super::super::Via {
                tls: configured(false, test_roots()),
                proxy: None,
                ..super::super::Via::from_env()
            };
            let ep = super::super::Endpoint {
                secure: true,
                host: "127.0.0.1".into(),
                port: addr.port(),
                target: "/".into(),
            };
            assert!(matches!(
                super::super::connect_with(&ep, &via),
                Err(crate::Error::Tls { .. })
            ));
        },
    );
}

#[test]
fn silent_tls_peer_expires_at_connect_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (release, hold) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (_tcp, _) = listener.accept().unwrap();
        match hold.recv() {
            Ok(()) | Err(_) => {}
        }
    });
    let result = fakes::within(
        "TLS connect bound",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let via = super::super::Via {
                proxy: None,
                limits: crate::Limits::new(Duration::from_millis(100), Duration::from_secs(300))
                    .unwrap(),
                ..super::super::Via::from_env()
            };
            super::super::connect_with(
                &super::super::Endpoint {
                    secure: true,
                    host: "127.0.0.1".into(),
                    port: addr.port(),
                    target: "/".into(),
                },
                &via,
            )
        },
    );
    assert!(matches!(result,Err(crate::Error::Stalled{idle}) if idle==Duration::from_millis(100)));
    drop(release);
}

#[test]
fn empty_store_decision_is_shared_and_loaded_once() {
    for count in [0, 1, 2] {
        let cache = OnceLock::new();
        let loads = AtomicUsize::new(0);
        for _ in 0..2 {
            assert_eq!(
                crate::empty_store_with(&cache, || {
                    loads.fetch_add(1, Ordering::SeqCst);
                    count
                }),
                count == 0
            );
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }
}
