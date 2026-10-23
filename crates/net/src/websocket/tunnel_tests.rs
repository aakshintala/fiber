use super::*;
use fakes::Deadline;
use fakes::MUST_SUCCEED_WITHIN;
use std::io::Read;
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};

#[derive(Debug)]
struct Recording {
    addr: SocketAddr,
    calls: Mutex<Vec<Duration>>,
}
impl Resolver for Recording {
    fn resolve(
        &self,
        _uri: &ureq::http::Uri,
        _config: &ureq::config::Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        self.calls.lock().unwrap().push(*timeout.after);
        let mut addresses = self.empty();
        addresses.push(self.addr);
        Ok(addresses)
    }
}
fn endpoint(port: u16) -> Endpoint {
    Endpoint {
        secure: false,
        host: "127.0.0.1".into(),
        port,
        target: "/".into(),
    }
}
fn via() -> Via {
    Via {
        proxy: None,
        ..Via::from_env()
    }
}

#[test]
fn dial_distinguishes_a_timeout_from_other_connect_errors() {
    let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
    for kind in [
        io::ErrorKind::TimedOut,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::ConnectionRefused,
    ] {
        let mut via = via();
        via.resolver = Arc::new(Recording {
            addr,
            calls: Mutex::new(Vec::new()),
        });
        let mut calls = Vec::new();
        let error = dial("box", 1234, &via, &mut |address, bound| {
            calls.push((*address, bound));
            Err(io::Error::from(kind))
        })
        .unwrap_err();
        assert_eq!(calls, vec![(addr, via.limits.connect())]);
        match (kind, error) {
            (
                io::ErrorKind::TimedOut,
                Error::ConnectTimedOut {
                    addr: actual,
                    limit,
                },
            ) => {
                assert_eq!(actual, addr);
                assert_eq!(limit, via.limits.connect());
            }
            (
                io::ErrorKind::WouldBlock | io::ErrorKind::ConnectionRefused,
                Error::Connect {
                    addr: actual,
                    source,
                },
            ) => {
                assert_eq!(actual, addr);
                assert_eq!(source.kind(), kind);
            }
            (_, error) => panic!("wrong connect error for {kind:?}: {error:?}"),
        }
    }
}

#[test]
fn resolver_gets_connect_limit_and_its_answer_is_dialled() {
    fakes::within(
        "resolved connection",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let recorder = Arc::new(Recording {
                addr: listener.local_addr().unwrap(),
                calls: Mutex::new(Vec::new()),
            });
            let mut via = via();
            via.limits =
                crate::Limits::new(Duration::from_millis(123), Duration::from_secs(300)).unwrap();
            via.resolver = recorder.clone();
            let (stream, tail) = connect(&endpoint(1), &via).unwrap();
            assert!(tail.is_empty());
            assert_eq!(stream.peer_addr().unwrap(), recorder.addr);
            assert_eq!(
                *recorder.calls.lock().unwrap(),
                vec![Duration::from_millis(123)]
            );
            assert_eq!(Via::from_env().limits, crate::LIMITS);
        },
    );
}

#[test]
fn proxy_tunnels_and_no_proxy_dials_direct() {
    fakes::within(
        "proxy and bypass",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let proxy = fakes::ConnectProxy::start().unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let mut via = via();
            via.proxy = Some(ureq::Proxy::new(&proxy.url()).unwrap());
            let (mut stream, _) = connect(&endpoint(port), &via).unwrap();
            stream.write_all(b"through").unwrap();
            let (mut peer, _) = listener.accept().unwrap();
            peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
            let mut bytes = [0; 7];
            peer.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"through");
            assert_eq!(proxy.connects(), vec![format!("127.0.0.1:{port}")]);
            stream.shutdown(std::net::Shutdown::Both).unwrap();
            drop(peer);
            let direct = fakes::ConnectProxy::start().unwrap();
            via.proxy = Some(
                ureq::Proxy::builder(ureq::ProxyProtocol::Http)
                    .host("127.0.0.1")
                    .port(direct.port())
                    .no_proxy("127.0.0.1")
                    .build()
                    .unwrap(),
            );
            let (stream, _) = connect(&endpoint(port), &via).unwrap();
            assert_eq!(stream.peer_addr().unwrap().port(), port);
            assert!(direct.connects().is_empty());
        },
    );
}

#[test]
fn proxy_basic_credentials_and_buffered_bytes_survive() {
    fakes::within(
        "proxy authorization",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let (mut peer, _) = listener.accept().unwrap();
                peer.set_read_timeout(Some(MUST_SUCCEED_WITHIN)).unwrap();
                let (request, _) = handshake::headers(&mut peer).unwrap();
                peer.write_all(b"HTTP/1.1 200 Connection established\r\n\r\nfirst")
                    .unwrap();
                tx.send(request).unwrap();
            });
            let mut via = via();
            via.proxy =
                Some(ureq::Proxy::new(&format!("http://user:pw@127.0.0.1:{port}")).unwrap());
            let (mut stream, mut tail) = connect(&endpoint(999), &via).unwrap();
            stream.read_to_end(&mut tail).unwrap();
            assert_eq!(tail, b"first");
            let request =
                fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "CONNECT request");
            assert_eq!(
                request,
                "CONNECT 127.0.0.1:999 HTTP/1.1\r\nHost: 127.0.0.1:999\r\nProxy-Authorization: Basic dXNlcjpwdw==\r\n\r\n"
            );
        },
    );
}

#[test]
fn refused_and_unsupported_proxies_are_named_without_passwords() {
    fakes::within(
        "proxy failures",
        Deadline::after(MUST_SUCCEED_WITHIN),
        || {
            let server =
                fakes::RawHttpServer::start(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                    .unwrap();
            let mut via = via();
            via.proxy = Some(ureq::Proxy::new(&server.url()).unwrap());
            assert!(
                matches!(connect(&endpoint(1),&via),Err(Error::ProxyRefused{status_line}) if status_line.contains("407"))
            );
            via.proxy = Some(ureq::Proxy::new("socks5://user:pw@127.0.0.1:1").unwrap());
            let error = connect(&endpoint(1), &via).unwrap_err();
            assert!(matches!(&error,Error::ProxyScheme{scheme} if scheme=="socks5"));
            assert!(!error.to_string().contains("pw"));
        },
    );
}

#[test]
fn silent_proxy_uses_connect_not_idle_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (done, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (_peer, _) = listener.accept().unwrap();
        match held.recv() {
            Ok(()) | Err(_) => {}
        }
    });
    std::thread::spawn(move || {
        let mut via = via();
        via.proxy = Some(ureq::Proxy::new(&format!("http://127.0.0.1:{port}")).unwrap());
        via.limits =
            crate::Limits::new(Duration::from_millis(100), Duration::from_secs(300)).unwrap();
        done.send(connect(&endpoint(1), &via)).unwrap();
    });
    let result =
        fakes::Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&rx, "silent proxy connect bound");
    assert!(matches!(result,Err(Error::Stalled{idle}) if idle==Duration::from_millis(100)));
    drop(release);
}
