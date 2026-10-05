use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

use ureq::Timeout;
use ureq::unversioned::transport::time::Duration;
use ureq::unversioned::transport::{LazyBuffers, NextTimeout, Transport};

use super::Socket;

fn wait() -> NextTimeout {
    NextTimeout {
        after: Duration::NotHappening,
        reason: Timeout::Global,
    }
}

#[test]
fn a_socket_is_open_until_a_read_finds_the_peer_closed() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    let mut socket = Socket {
        stream,
        buffers: LazyBuffers::new(1024, 1024),
        open: true,
    };
    assert!(socket.is_open());

    peer.write_all(b"x").unwrap();
    assert!(socket.await_input(wait()).unwrap());
    assert!(socket.is_open(), "a read that got bytes leaves it open");

    drop(peer);
    assert!(!socket.await_input(wait()).unwrap());
    assert!(!socket.is_open(), "a read that found the peer closed");
}

/// Records what it was asked to sign and adds one header.
struct Recorder(std::sync::Mutex<Vec<(String, String, Vec<u8>)>>);

impl contract::signing::Signer for Recorder {
    fn sign(
        &self,
        request: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        self.0.lock().unwrap().push((
            request.method.to_owned(),
            request.url.to_owned(),
            request.body.to_vec(),
        ));
        Ok(vec![("x-signature".to_owned(), "sig".to_owned())])
    }
}

/// Returns a header value HTTP rejects.
struct BadHeaderValue;

impl contract::signing::Signer for BadHeaderValue {
    fn sign(
        &self,
        _: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![("x-signature".to_owned(), "sig\n".to_owned())])
    }
}

/// Returns a header name HTTP rejects.
struct BadHeaderName;

impl contract::signing::Signer for BadHeaderName {
    fn sign(
        &self,
        _: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![("x-sig\nature".to_owned(), "sig".to_owned())])
    }
}

/// Refuses to sign.
struct Refuses;

impl contract::signing::Signer for Refuses {
    fn sign(
        &self,
        _: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Err(contract::signing::Error::Failed("no key".to_owned()))
    }
}

#[test]
fn a_signer_is_asked_on_every_send_and_its_headers_are_sent() {
    let server = fakes::ProviderServer::start([
        fakes::Response::status(200, "{}"),
        fakes::Response::status(200, "{}"),
    ])
    .unwrap();
    let url = format!("{}/v1/responses", server.url());
    let signer = Recorder(std::sync::Mutex::default());
    let headers = [("x-client".to_owned(), "fiber".to_owned())];
    for _ in 0..2 {
        let sent = super::post_signed(
            &url,
            &headers,
            b"{\"a\":1}",
            Some(&signer),
            &std::sync::Arc::default(),
        );
        assert!(sent.is_ok());
    }
    let asked = signer.0.lock().unwrap().clone();
    assert_eq!(asked.len(), 2, "a retry signs again");
    assert_eq!(asked[0], ("POST".to_owned(), url, b"{\"a\":1}".to_vec()));
    for request in server.requests() {
        assert_eq!(request.header("x-signature"), Some("sig"));
        assert_eq!(request.header("x-client"), Some("fiber"));
        assert_eq!(request.body, b"{\"a\":1}");
    }
}

#[test]
fn a_request_that_cannot_be_signed_is_never_sent() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let sent = super::post_signed(
        &format!("{}/v1", server.url()),
        &[],
        b"",
        Some(&Refuses),
        &std::sync::Arc::default(),
    );
    let Err(err) = sent.map(|_| ()) else {
        panic!("signed anyway");
    };
    let crate::Error::Sign(why) = &err else {
        panic!("not a sign failure: {err:?}");
    };
    assert!(why.to_string().contains("no key"), "{why}");
    assert_eq!(err.code(), contract::ErrorCode::CredentialFailed);
    assert_eq!(
        err.should_retry(),
        Some(false),
        "a sign failure is not retried"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn unusable_signed_headers_are_never_sent() {
    for signer in [
        &BadHeaderValue as &dyn contract::signing::Signer,
        &BadHeaderName,
    ] {
        let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
        let sent = super::post_signed(
            &format!("{}/v1", server.url()),
            &[],
            b"",
            Some(signer),
            &std::sync::Arc::default(),
        );
        let Err(err) = sent.map(|_| ()) else {
            panic!("sent unusable signed headers");
        };
        let crate::Error::Sign(why) = &err else {
            panic!("not a sign failure: {err:?}");
        };
        let message = why.to_string();
        assert!(
            matches!(why, contract::signing::Error::NotHeaders(_)),
            "{message}"
        );
        assert!(
            !message.contains('\n'),
            "must not echo header values: {message}"
        );
        assert!(
            message.contains("x-signature") || message.contains(r"x-sig\nature"),
            "names the bad header: {message}"
        );
        assert_eq!(err.code(), contract::ErrorCode::CredentialFailed);
        assert_eq!(
            err.should_retry(),
            Some(false),
            "unusable signed headers are not retried"
        );
        assert!(server.requests().is_empty());
    }
}

#[test]
fn retry_after_is_kept_only_when_finite_and_non_negative() {
    for (header, expected) in [
        ("7", Some(7.0)),
        ("1.5", Some(1.5)),
        ("0", Some(0.0)),
        ("abc", None),
        ("NaN", None),
        ("inf", None),
        ("-1", None),
        ("-0.5", None),
    ] {
        let server = fakes::ProviderServer::start([
            fakes::Response::status(429, "{}").header("retry-after", header)
        ])
        .unwrap();
        let Err(crate::Error::Status { retry_after, .. }) = super::post(
            &format!("{}/v1", server.url()),
            &[],
            b"{}",
            &std::sync::Arc::default(),
        ) else {
            panic!("a 429 with retry-after: {header} was not a status failure");
        };
        assert_eq!(retry_after, expected, "retry-after: {header}");
    }
}

/// How long the test waits for the server to see the call.
const REQUEST_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// How long the test waits for a cancelled call's thread to return.
const CALL_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

#[test]
fn cancelling_a_call_mid_stream_closes_the_socket_without_a_proxy() {
    let server = fakes::ProviderServer::start([fakes::Response::stream("data: {}\n\n")]).unwrap();
    server.hold();
    let url = format!("{}/v1", server.url());
    let cancel: std::sync::Arc<super::Cancel> = std::sync::Arc::default();
    let (done, finished) = mpsc::channel();
    let worker = std::sync::Arc::clone(&cancel);
    thread::spawn(move || {
        let result = super::post(&url, &[], b"{}", &worker).map(|_| ());
        match done.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    assert!(
        server.await_requests(1, REQUEST_WITHIN),
        "the server saw the call before it was cancelled"
    );
    cancel.cancel();
    let result = finished
        .recv_timeout(CALL_WITHIN)
        .expect("the cancelled call returns");
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a cancelled call fails to connect: {result:?}"
    );
}

/// How long a test waits for the proxy to record a CONNECT.
const CONNECT_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a test waits for the proxy to see a tunnel close.
const CLOSE_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// A proxy value pointing at `proxy`, bypassing nothing.
fn proxy_through(proxy: &fakes::ConnectProxy) -> ureq::Proxy {
    ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(proxy.port())
        .build()
        .unwrap()
}

/// The `host:port` the proxy records for a call to `server`.
fn target_of(server: &fakes::ProviderServer) -> String {
    let url = server.url();
    let port = url.rsplit(':').next().unwrap();
    format!("127.0.0.1:{port}")
}

#[test]
fn a_call_with_a_proxy_value_tunnels_through_the_proxy() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let target = target_of(&server);
    let url = format!("{}/v1", server.url());
    let (mut body, _) = super::post_with(
        &url,
        &[],
        b"{}",
        None,
        &std::sync::Arc::default(),
        Some(proxy_through(&proxy)),
    )
    .unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut body, &mut text).unwrap();
    assert_eq!(text, "{}");
    assert!(
        proxy.await_connects(1, CONNECT_WITHIN),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_call_past_no_proxy_bypasses_the_proxy() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let bypass = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(proxy.port())
        .no_proxy("127.0.0.1")
        .build()
        .unwrap();
    let url = format!("{}/v1", server.url());
    let (mut body, _) = super::post_with(
        &url,
        &[],
        b"{}",
        None,
        &std::sync::Arc::default(),
        Some(bypass),
    )
    .unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut body, &mut text).unwrap();
    assert_eq!(text, "{}");
    assert!(
        proxy.connects().is_empty(),
        "nothing went through the proxy"
    );
    assert_eq!(server.requests().len(), 1);
}

/// How long the test waits for the origin to see the handshake bytes.
const HANDSHAKE_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

#[test]
fn tls_runs_end_to_end_inside_the_tunnel() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (heard, first) = mpsc::channel();
    let origin = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(HANDSHAKE_WITHIN)).unwrap();
        let mut byte = [0; 1];
        std::io::Read::read_exact(&mut stream, &mut byte).unwrap();
        heard.send(byte[0]).unwrap();
    });
    let proxy = fakes::ConnectProxy::start().unwrap();
    let target = format!("127.0.0.1:{port}");
    let (done, finished) = mpsc::channel();
    let through = proxy_through(&proxy);
    thread::spawn(move || {
        let result = super::post_with(
            &format!("https://127.0.0.1:{port}/"),
            &[],
            b"{}",
            None,
            &std::sync::Arc::default(),
            Some(through),
        )
        .map(|_| ());
        match done.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    assert_eq!(
        first
            .recv_timeout(HANDSHAKE_WITHIN)
            .expect("the origin saw the handshake"),
        0x16,
        "a TLS ClientHello opens the tunnelled bytes"
    );
    assert!(
        proxy.await_connects(1, CONNECT_WITHIN),
        "the proxy recorded CONNECT {target}"
    );
    let result = finished
        .recv_timeout(CALL_WITHIN)
        .expect("the tunnelled call returns");
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a tunnel to nowhere fails to connect: {result:?}"
    );
    origin.join().unwrap();
}

#[test]
fn cancelling_a_call_mid_stream_through_the_proxy_closes_the_tunnel() {
    let server = fakes::ProviderServer::start([fakes::Response::stream("data: {}\n\n")]).unwrap();
    server.hold();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let target = target_of(&server);
    let url = format!("{}/v1", server.url());
    let cancel: std::sync::Arc<super::Cancel> = std::sync::Arc::default();
    let (done, finished) = mpsc::channel();
    let worker = std::sync::Arc::clone(&cancel);
    let through = proxy_through(&proxy);
    thread::spawn(move || {
        let result = super::post_with(&url, &[], b"{}", None, &worker, Some(through)).map(|_| ());
        match done.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    assert!(
        server.await_requests(1, REQUEST_WITHIN),
        "the server saw the tunnelled call before it was cancelled"
    );
    assert!(
        proxy.await_connects(1, CONNECT_WITHIN),
        "the proxy recorded CONNECT {target}"
    );
    cancel.cancel();
    let result = finished
        .recv_timeout(CALL_WITHIN)
        .expect("the cancelled tunnelled call returns");
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a cancelled tunnelled call fails to connect: {result:?}"
    );
    assert!(
        proxy.await_closed(1, CLOSE_WITHIN),
        "the proxy saw the tunnel close"
    );
}

#[test]
fn a_call_cancelled_before_it_starts_never_connects_to_the_proxy() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let cancel: std::sync::Arc<super::Cancel> = std::sync::Arc::default();
    cancel.cancel();
    let result = super::post_with(
        &format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        &cancel,
        Some(proxy_through(&proxy)),
    )
    .map(|_| ());
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a pre-cancelled call fails before connecting: {result:?}"
    );
    assert!(
        proxy.connects().is_empty(),
        "nothing went through the proxy"
    );
    assert!(server.requests().is_empty());
}

#[test]
fn a_proxy_that_refuses_connect_fails_the_call() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let refused = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = String::new();
        let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
        loop {
            let mut line = String::new();
            std::io::BufRead::read_line(&mut reader, &mut line).unwrap();
            head.push_str(&line);
            if line.trim().is_empty() {
                break;
            }
        }
        assert!(
            head.starts_with("CONNECT"),
            "the client sent CONNECT: {head:?}"
        );
        stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nconnection: close\r\ncontent-length: 0\r\n\r\n")
            .unwrap();
    });
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let denied = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(port)
        .build()
        .unwrap();
    let result = super::post_with(
        &format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        &std::sync::Arc::default(),
        Some(denied),
    )
    .map(|_| ());
    let Err(crate::Error::Connection(why)) = result else {
        panic!("a refused CONNECT was not a connection failure: {result:?}");
    };
    assert!(why.contains("403"), "the refusal names its status: {why}");
    refused.join().unwrap();
}

/// Present in the re-executed child, absent in the parent.
const PROXY_CHILD: &str = "FIBER_TEST_PROXY_CHILD";

/// The server URL, passed to the child on its environment.
const PROXY_CHILD_SERVER: &str = "FIBER_TEST_PROXY_SERVER";

/// How long the parent waits for the re-executed child to exit.
const CHILD_WITHIN: std::time::Duration = std::time::Duration::from_secs(10);

#[test]
fn the_proxy_environment_reaches_model_calls() {
    if std::env::var_os(PROXY_CHILD).is_some() {
        let url = std::env::var(PROXY_CHILD_SERVER).unwrap();
        let (mut body, _) = super::post_signed(
            &format!("{url}/v1"),
            &[],
            b"{}",
            None,
            &std::sync::Arc::default(),
        )
        .unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut body, &mut text).unwrap();
        assert_eq!(text, "{}");
        return;
    }
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let target = target_of(&server);
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "http::tests::the_proxy_environment_reaches_model_calls",
            "--nocapture",
        ])
        .env(PROXY_CHILD, "1")
        .env("HTTPS_PROXY", proxy.url())
        .env(PROXY_CHILD_SERVER, server.url())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let output = child.wait_with_output().unwrap();
        match done.send(output) {
            Ok(()) | Err(_) => {}
        }
    });
    let output = match finished.recv_timeout(CHILD_WITHIN) {
        Ok(output) => output,
        Err(_) => panic!("waited {CHILD_WITHIN:?} for the proxy-env child"),
    };
    assert!(
        output.status.success(),
        "the proxy-env child called through the proxy:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        proxy.await_connects(1, CONNECT_WITHIN),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);
}
