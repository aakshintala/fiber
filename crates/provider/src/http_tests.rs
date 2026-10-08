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
    let (mut socket, arrived) = fakes::within(
        "the socket to see the peer's byte",
        CALL_WITHIN,
        move || {
            let arrived = socket.await_input(wait());
            (socket, arrived)
        },
    );
    assert!(arrived.unwrap());
    assert!(socket.is_open(), "a read that got bytes leaves it open");

    drop(peer);
    let (mut socket, arrived) =
        fakes::within("the socket to see the peer close", CALL_WITHIN, move || {
            let arrived = socket.await_input(wait());
            (socket, arrived)
        });
    assert!(!arrived.unwrap());
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
    let signer = std::sync::Arc::new(Recorder(std::sync::Mutex::default()));
    let headers = [("x-client".to_owned(), "fiber".to_owned())];
    for _ in 0..2 {
        // An explicit direct connection, so the test holds without a
        // proxy whatever the developer's shell names.
        let (sent, _) = posted(
            url.clone(),
            &headers,
            b"{\"a\":1}",
            Some(std::sync::Arc::clone(&signer) as std::sync::Arc<dyn contract::signing::Signer>),
            &std::sync::Arc::default(),
            None,
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
    let (sent, _) = posted(
        format!("{}/v1", server.url()),
        &[],
        b"",
        Some(std::sync::Arc::new(Refuses) as std::sync::Arc<dyn contract::signing::Signer>),
        &std::sync::Arc::default(),
        None,
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
        std::sync::Arc::new(BadHeaderValue) as std::sync::Arc<dyn contract::signing::Signer>,
        std::sync::Arc::new(BadHeaderName) as std::sync::Arc<dyn contract::signing::Signer>,
    ] {
        let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
        let (sent, _) = posted(
            format!("{}/v1", server.url()),
            &[],
            b"",
            Some(signer),
            &std::sync::Arc::default(),
            None,
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
        let (Err(crate::Error::Status { retry_after, .. }), _) = posted(
            format!("{}/v1", server.url()),
            &[],
            b"{}",
            None,
            &std::sync::Arc::default(),
            None,
        ) else {
            panic!("a 429 with retry-after: {header} was not a status failure");
        };
        assert_eq!(retry_after, expected, "retry-after: {header}");
    }
}

/// How long the test waits for the server to see the call.
const REQUEST_WITHIN: std::time::Duration = std::time::Duration::from_secs(2);

/// How long a test waits for one call to return.
///
/// The worst test, `retry_after_is_kept_only_when_finite_and_non_negative`,
/// makes eight (8 x 5 s = 40 s <= 60 s, half of nextest's 120 s kill). A
/// passing run never waits on it; it only bounds a hang.
const CALL_WITHIN: std::time::Duration = std::time::Duration::from_secs(5);

/// One call's outcome, its body read to a `String` on success, with the
/// secrets it reported.
type Posted = (
    Result<(String, Option<bool>), crate::Error>,
    crate::redact::Secrets,
);

/// Runs one `post_with` with a fresh [`Secrets`] on its own thread and
/// returns its outcome with the secrets it reported. Calling code that
/// blocks is a wait too (`docs/testing.md`, "Waits and timeouts"): on
/// expiry the test fails naming the call. A signer the test inspects
/// afterwards is held as an `Arc` and passed as a clone.
fn posted(
    url: String,
    headers: &[(String, String)],
    body: &'static [u8],
    signer: Option<std::sync::Arc<dyn contract::signing::Signer>>,
    cancel: &std::sync::Arc<super::Cancel>,
    proxy: Option<ureq::Proxy>,
) -> Posted {
    let headers = headers.to_vec();
    let cancel = std::sync::Arc::clone(cancel);
    fakes::within(&format!("the call to {url}"), CALL_WITHIN, move || {
        let mut secrets = crate::redact::Secrets::default();
        let sent = super::post_with(
            &url,
            &headers,
            body,
            signer.as_deref(),
            &cancel,
            &mut secrets,
            proxy,
        );
        let outcome = match sent {
            Ok((mut stream, flag)) => {
                let mut text = String::new();
                match std::io::Read::read_to_string(&mut stream, &mut text) {
                    Ok(_) => Ok((text, flag)),
                    Err(err) => Err(crate::Error::Connection(err.to_string())),
                }
            }
            Err(err) => Err(err),
        };
        (outcome, secrets)
    })
}

#[test]
fn cancelling_a_call_mid_stream_closes_the_socket_without_a_proxy() {
    let server = fakes::ProviderServer::start([fakes::Response::stream("data: {}\n\n")]).unwrap();
    server.hold();
    let url = format!("{}/v1", server.url());
    let cancel: std::sync::Arc<super::Cancel> = std::sync::Arc::default();
    let (done, finished) = mpsc::channel();
    let worker = std::sync::Arc::clone(&cancel);
    thread::spawn(move || {
        // An explicit direct connection, so the test holds without a proxy
        // whatever the developer's shell names: `post` would read it.
        let result = super::post_with(
            &url,
            &[],
            b"{}",
            None,
            &worker,
            &mut crate::redact::Secrets::default(),
            None,
        )
        .map(|_| ());
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
    let (sent, _) = posted(
        url,
        &[],
        b"{}",
        None,
        &std::sync::Arc::default(),
        Some(proxy_through(&proxy)),
    );
    let (text, _) = sent.unwrap();
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
    let (sent, _) = posted(
        url,
        &[],
        b"{}",
        None,
        &std::sync::Arc::default(),
        Some(bypass),
    );
    let (text, _) = sent.unwrap();
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
            &mut crate::redact::Secrets::default(),
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
    fakes::within("the origin thread to return", HANDSHAKE_WITHIN, move || {
        origin.join()
    })
    .unwrap();
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
        let result = super::post_with(
            &url,
            &[],
            b"{}",
            None,
            &worker,
            &mut crate::redact::Secrets::default(),
            Some(through),
        )
        .map(|_| ());
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
    let (result, _) = posted(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        &cancel,
        Some(proxy_through(&proxy)),
    );
    let result = result.map(|_| ());
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
    let (result, _) = posted(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        &std::sync::Arc::default(),
        Some(denied),
    );
    let result = result.map(|_| ());
    let Err(crate::Error::Connection(why)) = result else {
        panic!("a refused CONNECT was not a connection failure: {result:?}");
    };
    assert!(why.contains("403"), "the refusal names its status: {why}");
    fakes::within(
        "the refusing proxy thread to return",
        CONNECT_WITHIN,
        move || refused.join(),
    )
    .unwrap();
}

/// Present in the re-executed child, absent in the parent.
const PROXY_CHILD: &str = "FIBER_TEST_PROXY_CHILD";

/// Present in the re-executed child when it should connect directly.
const PROXY_DIRECT_CHILD: &str = "FIBER_TEST_PROXY_DIRECT_CHILD";

/// The server URL, passed to the child on its environment.
const PROXY_CHILD_SERVER: &str = "FIBER_TEST_PROXY_SERVER";

/// The child's side of the proxy-environment tests: calls the server once,
/// directly or through the environment's proxy.
fn proxy_child_main() {
    let url = std::env::var(PROXY_CHILD_SERVER).unwrap();
    let direct = std::env::var_os(PROXY_DIRECT_CHILD).is_some();
    let (mut body, _) = super::post_signed(
        &format!("{url}/v1"),
        &[],
        b"{}",
        None,
        direct,
        &std::sync::Arc::default(),
        &mut crate::redact::Secrets::default(),
    )
    .unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut body, &mut text).unwrap();
    assert_eq!(text, "{}");
}

#[test]
fn the_proxy_environment_reaches_model_calls() {
    if std::env::var_os(PROXY_CHILD).is_some() {
        proxy_child_main();
        return;
    }
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let target = target_of(&server);
    let proxy_url = proxy.url();
    let server_url = server.url();
    let output = fakes::rerun(
        "http::tests::the_proxy_environment_reaches_model_calls",
        &[
            (PROXY_CHILD, "1"),
            ("HTTPS_PROXY", proxy_url.as_str()),
            (PROXY_CHILD_SERVER, server_url.as_str()),
        ],
    );
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

#[test]
fn a_direct_call_ignores_the_proxy_environment() {
    if std::env::var_os(PROXY_CHILD).is_some() {
        proxy_child_main();
        return;
    }
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let proxy_url = proxy.url();
    let server_url = server.url();
    let output = fakes::rerun(
        "http::tests::a_direct_call_ignores_the_proxy_environment",
        &[
            (PROXY_CHILD, "1"),
            (PROXY_DIRECT_CHILD, "1"),
            ("HTTPS_PROXY", proxy_url.as_str()),
            (PROXY_CHILD_SERVER, server_url.as_str()),
        ],
    );
    assert!(
        output.status.success(),
        "the direct child called past the proxy:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        proxy.connects().is_empty(),
        "nothing went through the proxy"
    );
    assert_eq!(server.requests().len(), 1);
}

/// Signs with an `authorization` scheme and a second header.
struct TwoHeaders;

impl contract::signing::Signer for TwoHeaders {
    fn sign(
        &self,
        _: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![
            ("authorization".to_owned(), "Bearer tok-1".to_owned()),
            ("x-sig".to_owned(), "s-1".to_owned()),
        ])
    }
}

#[test]
fn signed_header_values_join_the_secrets() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let (sent, secrets) = posted(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        Some(std::sync::Arc::new(TwoHeaders) as std::sync::Arc<dyn contract::signing::Signer>),
        &std::sync::Arc::default(),
        None,
    );
    assert!(sent.is_ok());
    assert_eq!(secrets.redact("Bearer tok-1"), "[redacted]");
    assert_eq!(secrets.redact("tok-1"), "[redacted]");
    assert_eq!(secrets.redact("s-1"), "[redacted]");
}

#[test]
fn a_signer_that_fails_adds_nothing() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let (sent, secrets) = posted(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        Some(std::sync::Arc::new(Refuses) as std::sync::Arc<dyn contract::signing::Signer>),
        &std::sync::Arc::default(),
        None,
    );
    assert!(sent.is_err());
    assert_eq!(secrets.redact("Bearer tok-1"), "Bearer tok-1");
}

/// Reports a credential no returned header carries: `sign()` replaced the
/// `authorization` header carrying it.
struct HiddenCredential;

impl contract::signing::Signer for HiddenCredential {
    fn sign(
        &self,
        _: &contract::signing::SignRequest<'_>,
    ) -> Result<Vec<(String, String)>, contract::signing::Error> {
        Ok(vec![("x-sig".to_owned(), "s-1".to_owned())])
    }

    fn credentials(&self) -> Vec<contract::Secret> {
        vec![contract::Secret::new("hidden-tok".to_owned())]
    }
}

#[test]
fn signer_credentials_join_the_secrets_even_when_no_header_carries_them() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let (sent, secrets) = posted(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        Some(std::sync::Arc::new(HiddenCredential) as std::sync::Arc<dyn contract::signing::Signer>),
        &std::sync::Arc::default(),
        None,
    );
    assert!(sent.is_ok());
    assert_eq!(secrets.redact("hidden-tok"), "[redacted]");
    assert_eq!(secrets.redact("s-1"), "[redacted]");
}

#[test]
fn a_signer_without_credentials_reports_none() {
    use contract::signing::Signer as _;
    assert!(
        Recorder(std::sync::Mutex::default())
            .credentials()
            .is_empty()
    );
}

// unprobed: shape from pi `parseErrorResponse` and codex-cli strings,
// `research/codex-responses-probe`, "The usage-limit error body".
const USAGE_LIMIT_BODY: &str = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1791396000}}"#;
const USAGE_DATE: &str = "Wed, 07 Oct 2026 16:00:00 GMT";

/// The status failure one `post_with` against `server` returns.
fn failed_status(server: &fakes::ProviderServer) -> crate::Error {
    let (sent, _) = posted(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        &std::sync::Arc::default(),
        None,
    );
    let Err(err) = sent.map(|_| ()) else {
        panic!("a 429 was not a status failure");
    };
    err
}

#[test]
fn a_usage_limit_body_waits_from_its_date_header_to_its_reset() {
    let server = fakes::ProviderServer::start([
        fakes::Response::status(429, USAGE_LIMIT_BODY).header("date", USAGE_DATE),
        fakes::Response::status(429, USAGE_LIMIT_BODY).header("date", USAGE_DATE),
    ])
    .unwrap();
    let crate::Error::Status { retry_after, .. } = failed_status(&server) else {
        panic!("not a status failure");
    };
    // `resets_at` 18:00 minus `Date` 16:00, in seconds.
    assert_eq!(retry_after, Some(7200.0));
    assert_eq!(
        failed_status(&server).code(),
        contract::ErrorCode::QuotaExceeded
    );
}

#[test]
fn a_retry_after_header_wins_over_the_usage_reset() {
    let server = fakes::ProviderServer::start([fakes::Response::status(429, USAGE_LIMIT_BODY)
        .header("date", USAGE_DATE)
        .header("retry-after", "7")])
    .unwrap();
    let crate::Error::Status { retry_after, .. } = failed_status(&server) else {
        panic!("not a status failure");
    };
    assert_eq!(retry_after, Some(7.0));
}

#[test]
fn a_usage_limit_without_a_usable_date_sets_no_wait() {
    let bodies = [
        ("no date", USAGE_LIMIT_BODY, None),
        ("bad date", USAGE_LIMIT_BODY, Some("not a date")),
        (
            "reset at the date",
            USAGE_LIMIT_BODY,
            Some("Wed, 07 Oct 2026 18:00:00 GMT"),
        ),
        (
            "reset before the date",
            USAGE_LIMIT_BODY,
            Some("Wed, 07 Oct 2026 19:00:00 GMT"),
        ),
    ];
    for (name, body, date) in bodies {
        let mut response = fakes::Response::status(429, body);
        if let Some(date) = date {
            response = response.header("date", date);
        }
        let server = fakes::ProviderServer::start([response]).unwrap();
        let crate::Error::Status { retry_after, .. } = failed_status(&server) else {
            panic!("not a status failure: {name}");
        };
        assert_eq!(retry_after, None, "{name}");
    }
    for body in [
        r#"{"error":{"type":"usage_limit_reached","resets_at":"soon"}}"#,
        r#"{"error":{"type":"usage_limit_reached","resets_at":1791396000.5}}"#,
        r#"{"error":{"type":"usage_limit_reached"}}"#,
        r#"{"error":{"message":"Slow down."}}"#,
    ] {
        let server = fakes::ProviderServer::start([
            fakes::Response::status(429, body).header("date", USAGE_DATE)
        ])
        .unwrap();
        let crate::Error::Status { retry_after, .. } = failed_status(&server) else {
            panic!("not a status failure: {body}");
        };
        assert_eq!(retry_after, None, "{body}");
    }
}
