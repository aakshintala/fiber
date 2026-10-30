use fakes::Deadline;
use fakes::MUST_SUCCEED_WITHIN;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

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
const REQUEST_WITHIN: std::time::Duration = fakes::MUST_SUCCEED_WITHIN;

/// One call's outcome, its body read to a `String` on success, with the
/// secrets it reported.
type Posted = (
    Result<(String, Option<bool>), crate::Error>,
    crate::redact::Secrets,
);

/// One `post_signed` over `route` on its own thread under [`MUST_SUCCEED_WITHIN`],
/// its body read to a `String` on success: without the wait a held call
/// outlives the test, which fails naming the call. `cancel` stays alive in
/// the caller while the test reads the server's side.
fn called(
    url: String,
    headers: &[(String, String)],
    body: &'static [u8],
    signer: Option<std::sync::Arc<dyn contract::signing::Signer>>,
    route: super::Route,
    cancel: &std::sync::Arc<super::Cancel>,
) -> Posted {
    let headers = headers.to_vec();
    let cancel = std::sync::Arc::clone(cancel);
    fakes::within(
        &format!("the call to {url}"),
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut secrets = crate::redact::Secrets::default();
            let sent = super::post_signed(
                &url,
                &headers,
                body,
                signer.as_deref(),
                &route,
                &cancel,
                &mut secrets,
            );
            let outcome = match sent {
                Ok((mut stream, flag, _)) => {
                    let mut text = String::new();
                    match std::io::Read::read_to_string(&mut stream, &mut text) {
                        Ok(_) => Ok((text, flag)),
                        Err(err) => Err(crate::Error::Connection(err.to_string())),
                    }
                }
                Err(err) => Err(err),
            };
            (outcome, secrets)
        },
    )
}

/// Runs one `post_signed` with a fresh [`Secrets`] on its own thread and
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
    let route = super::Route {
        via: match proxy {
            None => super::Via::Direct,
            Some(proxy) => super::Via::Through(proxy),
        },
        limits: net::LIMITS,
    };
    called(url, headers, body, signer, route, cancel)
}

#[test]
fn a_cancel_is_not_stopped_until_it_is_cancelled() {
    use net::Keep as _;
    let cancel: std::sync::Arc<super::Cancel> = std::sync::Arc::default();
    assert!(!cancel.is_stopped(), "a fresh call is not stopped");
    cancel.cancel();
    assert!(cancel.is_stopped(), "a cancelled call is stopped");
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
        let route = super::Route {
            via: super::Via::Direct,
            limits: net::LIMITS,
        };
        let result = super::post_signed(
            &url,
            &[],
            b"{}",
            None,
            &route,
            &worker,
            &mut crate::redact::Secrets::default(),
        )
        .map(|_| ());
        match done.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    assert!(
        server.await_requests(1, Deadline::after(REQUEST_WITHIN)),
        "the server saw the call before it was cancelled"
    );
    cancel.cancel();
    let result = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the cancelled call returns");
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a cancelled call fails to connect: {result:?}"
    );
}

/// How long a test waits for the proxy to record a CONNECT.
const CONNECT_WITHIN: std::time::Duration = fakes::MUST_SUCCEED_WITHIN;

/// How long a test waits for the proxy to see a tunnel close.
const CLOSE_WITHIN: std::time::Duration = fakes::MUST_SUCCEED_WITHIN;

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
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
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
const HANDSHAKE_WITHIN: std::time::Duration = fakes::MUST_SUCCEED_WITHIN;

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
        let route = super::Route {
            via: super::Via::Through(through),
            limits: net::LIMITS,
        };
        let result = super::post_signed(
            &format!("https://127.0.0.1:{port}/"),
            &[],
            b"{}",
            None,
            &route,
            &std::sync::Arc::default(),
            &mut crate::redact::Secrets::default(),
        )
        .map(|_| ());
        match done.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    assert_eq!(
        Deadline::after(HANDSHAKE_WITHIN)
            .recv(&first)
            .expect("the origin saw the handshake"),
        0x16,
        "a TLS ClientHello opens the tunnelled bytes"
    );
    assert!(
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
        "the proxy recorded CONNECT {target}"
    );
    let result = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the tunnelled call returns");
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a tunnel to nowhere fails to connect: {result:?}"
    );
    fakes::within(
        "the origin thread to return",
        Deadline::after(HANDSHAKE_WITHIN),
        move || origin.join(),
    )
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
        let route = super::Route {
            via: super::Via::Through(through),
            limits: net::LIMITS,
        };
        let result = super::post_signed(
            &url,
            &[],
            b"{}",
            None,
            &route,
            &worker,
            &mut crate::redact::Secrets::default(),
        )
        .map(|_| ());
        match done.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    assert!(
        server.await_requests(1, Deadline::after(REQUEST_WITHIN)),
        "the server saw the tunnelled call before it was cancelled"
    );
    assert!(
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
        "the proxy recorded CONNECT {target}"
    );
    cancel.cancel();
    let result = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .expect("the cancelled tunnelled call returns");
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a cancelled tunnelled call fails to connect: {result:?}"
    );
    assert!(
        proxy.await_closed(1, Deadline::after(CLOSE_WITHIN)),
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
    let proxy = fakes::RawHttpServer::start(
        b"HTTP/1.1 403 Forbidden\r\nconnection: close\r\ncontent-length: 0\r\n\r\n",
    )
    .unwrap();
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let denied = ureq::Proxy::builder(ureq::ProxyProtocol::Http)
        .host("127.0.0.1")
        .port(proxy.port())
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
    assert!(
        proxy.await_replies(1, Deadline::after(CONNECT_WITHIN)),
        "the refusing proxy answered"
    );
    let requests = proxy.requests();
    assert_eq!(requests.len(), 1, "the proxy saw the CONNECT");
    assert!(
        requests[0].method.starts_with("CONNECT"),
        "the client sent CONNECT: {:?}",
        requests[0].method
    );
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
    let route = super::Route {
        via: if direct {
            super::Via::Direct
        } else {
            super::Via::Environment
        },
        limits: net::LIMITS,
    };
    let (mut body, _, _) = super::post_signed(
        &format!("{url}/v1"),
        &[],
        b"{}",
        None,
        &route,
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
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
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

/// The status failure one `post_signed` against `server` returns.
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
fn a_present_but_unparseable_retry_after_is_left_unset() {
    // The header is present, so the body's wait must not replace it: an
    // unparseable `Retry-After` leaves `retry_after` unset, as before.
    for retry_after in ["abc", "Wed, 07 Oct 2026 16:00:00 GMT", "-1", "inf"] {
        let server = fakes::ProviderServer::start([fakes::Response::status(429, USAGE_LIMIT_BODY)
            .header("date", USAGE_DATE)
            .header("retry-after", retry_after)])
        .unwrap();
        let crate::Error::Status {
            retry_after: waited,
            ..
        } = failed_status(&server)
        else {
            panic!("not a status failure: retry-after: {retry_after}");
        };
        assert_eq!(waited, None, "retry-after: {retry_after}");
    }
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

/// The socket limits the stall tests run under: one second per address
/// to connect, 200 ms without a byte. A silent peer fails fast, while a
/// body that keeps arriving reads on.
fn stall_limits() -> net::Limits {
    net::Limits::new(
        std::time::Duration::from_secs(1),
        std::time::Duration::from_millis(200),
    )
    .unwrap()
}

/// One `post_signed` over a direct route with `limits`, sharing [`called`]:
/// the short limits keep a held call inside [`MUST_SUCCEED_WITHIN`].
fn posted_limited(
    url: String,
    limits: net::Limits,
    cancel: &std::sync::Arc<super::Cancel>,
) -> Result<(String, Option<bool>), crate::Error> {
    let route = super::Route {
        via: super::Via::Direct,
        limits,
    };
    called(url, &[], b"{}", None, route, cancel).0
}

#[test]
fn a_held_server_fails_the_call_as_a_dropped_connection() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    server.hold();
    let result = posted_limited(
        format!("{}/v1", server.url()),
        stall_limits(),
        &std::sync::Arc::default(),
    )
    .map(|_| ());
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a held server fails the call as a dropped connection: {result:?}"
    );
}

#[test]
fn a_stream_stalled_mid_body_fails_the_read_and_closes_the_socket() {
    let server =
        fakes::ProviderServer::start([fakes::Response::stall(200, "data: {}\n\n", 100_000)
            .header("content-type", "text/event-stream")])
        .unwrap();
    let cancel: std::sync::Arc<super::Cancel> = std::sync::Arc::default();
    let result =
        posted_limited(format!("{}/v1", server.url()), stall_limits(), &cancel).map(|_| ());
    assert!(
        matches!(result, Err(crate::Error::Connection(_))),
        "a stalled stream fails the read: {result:?}"
    );
    assert!(
        server.await_closed(1, Deadline::after(REQUEST_WITHIN)),
        "the server saw the socket close while the call's cancel is still alive"
    );
}

#[test]
fn a_stalled_error_body_is_a_dropped_connection_not_a_status() {
    let server = fakes::ProviderServer::start([fakes::Response::stall(500, "{", 1000)]).unwrap();
    let result = posted_limited(
        format!("{}/v1", server.url()),
        stall_limits(),
        &std::sync::Arc::default(),
    )
    .map(|_| ());
    let Err(crate::Error::Stalled { should_retry, .. }) = &result else {
        panic!("a stalled 500 is a dropped connection, not a status: {result:?}");
    };
    assert_eq!(*should_retry, None);
    assert_eq!(
        result.unwrap_err().code(),
        contract::ErrorCode::ConnectionFailed
    );
}

#[test]
fn a_stalled_error_body_keeps_a_no_retry_veto() {
    let server = fakes::ProviderServer::start([
        fakes::Response::stall(500, "{", 1000).header("x-should-retry", "false")
    ])
    .unwrap();
    let result = posted_limited(
        format!("{}/v1", server.url()),
        stall_limits(),
        &std::sync::Arc::default(),
    )
    .map(|_| ());
    let Err(err) = &result else {
        panic!("a stalled 500 with x-should-retry: false was not a stalled failure");
    };
    let crate::Error::Stalled { should_retry, .. } = err else {
        panic!("a stalled 500 with x-should-retry: false was not stalled: {err:?}");
    };
    assert_eq!(*should_retry, Some(false));
    assert_eq!(err.code(), contract::ErrorCode::ConnectionFailed);
    assert_eq!(err.should_retry(), Some(false));
    assert!(
        err.failure(
            "test",
            crate::LoginAdvice::Advised,
            &crate::redact::Secrets::default()
        )
        .provider
        .is_none()
    );
}

#[test]
fn an_overlong_error_body_is_still_a_status_with_no_body() {
    // Over ureq's 10 MB `read_to_string` limit: the read fails with a
    // non-timeout error, so the status keeps its code with an empty body.
    let server = fakes::ProviderServer::start([fakes::Response::status(
        500,
        vec![b'x'; 10 * 1024 * 1024 + 1],
    )])
    .unwrap();
    let Err(crate::Error::Status { status, body, .. }) = posted_limited(
        format!("{}/v1", server.url()),
        stall_limits(),
        &std::sync::Arc::default(),
    )
    .map(|_| ()) else {
        panic!("an overlong 500 was not a status failure");
    };
    assert_eq!(status, 500);
    assert_eq!(body, "", "a body ureq refuses to read stays empty");
}

#[test]
fn an_endpoint_runs_under_the_production_limits_by_default() {
    assert_eq!(
        crate::Endpoint::default().limits,
        net::LIMITS,
        "the default endpoint carries the production limits"
    );
}

#[test]
fn a_route_follows_its_endpoint_proxy_and_limits() {
    let direct = crate::Endpoint {
        direct: true,
        limits: stall_limits(),
        ..crate::Endpoint::default()
    };
    let route = super::Route::of(&direct);
    assert!(
        matches!(route.via, super::Via::Direct),
        "a direct endpoint connects directly"
    );
    assert_eq!(route.limits, stall_limits());
    let named = crate::Endpoint {
        direct: false,
        limits: stall_limits(),
        ..crate::Endpoint::default()
    };
    let route = super::Route::of(&named);
    assert!(
        matches!(route.via, super::Via::Environment),
        "an indirect endpoint reads the environment's proxy"
    );
    assert_eq!(route.limits, stall_limits());
}
use contract::clock::Clock;

/// A clock that ticks forward on every read: `now()` is the origin plus
/// one step per call, so milestone order is observable on one thread.
struct TickClock {
    origin: std::time::Instant,
    step: std::time::Duration,
    next: std::sync::atomic::AtomicU64,
}

impl TickClock {
    #[allow(
        clippy::disallowed_methods,
        reason = "the ticking clock's origin: every later read is a tick, never the process clock"
    )]
    fn new(step: std::time::Duration) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            origin: std::time::Instant::now(),
            step,
            next: std::sync::atomic::AtomicU64::new(0),
        })
    }
}

impl contract::clock::Clock for TickClock {
    fn now(&self) -> std::time::Instant {
        let n = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let ticks = u32::try_from(n).unwrap_or(u32::MAX);
        self.origin + self.step * ticks
    }

    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000)
    }

    fn sleep(&self, _: std::time::Duration) {}

    fn wait_until(
        &self,
        _: Option<std::time::Instant>,
        _: &mut dyn FnMut(Option<std::time::Duration>),
    ) {
    }

    fn subscribe(&self, _: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// One observed `post_signed` on its own thread under [`MUST_SUCCEED_WITHIN`],
/// returning the outcome with the secrets it reported.
fn observed(
    url: String,
    headers: &[(String, String)],
    body: &'static [u8],
    signer: Option<std::sync::Arc<dyn contract::signing::Signer>>,
    route: super::Route,
    cancel: &std::sync::Arc<super::Cancel>,
    clock: std::sync::Arc<dyn contract::clock::Clock>,
) -> (
    Result<super::ObservedSend, super::ObservedFailure>,
    crate::redact::Secrets,
) {
    observed_with(url, headers, body, signer, route, cancel, clock, None)
}

/// As [`observed`], timing the connector group through `timing` when
/// supplied: `None` keeps the plain connector with no net scope.
#[allow(
    clippy::too_many_arguments,
    reason = "one observed send with its timing input beside the call's own"
)]
fn observed_with(
    url: String,
    headers: &[(String, String)],
    body: &'static [u8],
    signer: Option<std::sync::Arc<dyn contract::signing::Signer>>,
    route: super::Route,
    cancel: &std::sync::Arc<super::Cancel>,
    clock: std::sync::Arc<dyn contract::clock::Clock>,
    timing: Option<contract::diag::Timing>,
) -> (
    Result<super::ObservedSend, super::ObservedFailure>,
    crate::redact::Secrets,
) {
    let headers = headers.to_vec();
    let cancel = std::sync::Arc::clone(cancel);
    fakes::within(
        &format!("the observed call to {url}"),
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut secrets = crate::redact::Secrets::default();
            let sent = super::post_signed_observed(
                &url,
                &headers,
                body,
                signer.as_deref(),
                &route,
                &cancel,
                &mut secrets,
                &clock,
                timing.as_ref(),
            );
            (sent, secrets)
        },
    )
}

fn direct_route() -> super::Route {
    super::Route {
        via: super::Via::Direct,
        limits: net::LIMITS,
    }
}

fn limited_route() -> super::Route {
    super::Route {
        via: super::Via::Direct,
        limits: stall_limits(),
    }
}

#[test]
fn signing_completes_before_the_send_stamp() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "hi")]).unwrap();
    let url = format!("{}/v1", server.url());
    let clock = TickClock::new(std::time::Duration::from_millis(5));
    let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    struct Order {
        clock: std::sync::Arc<TickClock>,
        order: std::sync::Arc<std::sync::Mutex<Vec<(&'static str, std::time::Instant)>>>,
    }
    impl contract::signing::Signer for Order {
        fn sign(
            &self,
            _: &contract::signing::SignRequest<'_>,
        ) -> Result<Vec<(String, String)>, contract::signing::Error> {
            self.order
                .lock()
                .unwrap()
                .push(("sign", contract::clock::Clock::now(&*self.clock)));
            Ok(vec![("x-signature".to_owned(), "sig".to_owned())])
        }
    }
    let signer = std::sync::Arc::new(Order {
        clock: std::sync::Arc::clone(&clock),
        order: std::sync::Arc::clone(&order),
    });
    let (sent, _) = observed(
        url,
        &[],
        b"{}",
        Some(signer),
        direct_route(),
        &std::sync::Arc::default(),
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
    );
    let sent = match sent {
        Ok(sent) => sent,
        Err(failed) => panic!("the observed send failed: {:?}", failed.error),
    };
    let order = order.lock().unwrap().clone();
    assert_eq!(order.len(), 1);
    assert_eq!(order[0].0, "sign");
    assert!(
        order[0].1 < sent.start,
        "signing completes before the send stamp"
    );
    assert!(
        sent.start < sent.headers_at,
        "the send stamp precedes the headers stamp"
    );
    assert_eq!(sent.status, 200);
}

#[test]
fn a_refused_peer_sends_one_record_with_response_fields_omitted() {
    let clock = fakes::clock::FakeClock::new();
    let (sent, _) = observed(
        fakes::refused::url(),
        &[],
        b"{}",
        None,
        direct_route(),
        &std::sync::Arc::default(),
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
    );
    let Err(failed) = sent else {
        panic!("a refused peer unexpectedly answered");
    };
    assert!(
        matches!(failed.error, crate::Error::Connection(_)),
        "a refused peer fails to connect: {:?}",
        failed.error
    );
    let Some(dispatched) = failed.sent else {
        panic!("a failed send after the stamp reports it");
    };
    assert_eq!(dispatched.headers_at, None);
    assert_eq!(dispatched.status, None);
    assert_eq!(dispatched.bytes_read, 0);
}

#[test]
fn a_pre_send_cancel_sign_failure_or_refused_header_sends_nothing() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let url = format!("{}/v1", server.url());
    let cancelled: std::sync::Arc<super::Cancel> = std::sync::Arc::default();
    cancelled.cancel();
    let as_clock = || {
        std::sync::Arc::clone(&fakes::clock::FakeClock::new())
            as std::sync::Arc<dyn contract::clock::Clock>
    };
    // Cancelled before it starts.
    let (sent, _) = observed(
        url.clone(),
        &[],
        b"{}",
        None,
        direct_route(),
        &cancelled,
        as_clock(),
    );
    let Err(failed) = sent else {
        panic!("a cancelled call sent anyway");
    };
    assert!(failed.sent.is_none(), "no send stamp before the send");
    // Signing failed.
    let (sent, _) = observed(
        url.clone(),
        &[],
        b"{}",
        Some(std::sync::Arc::new(Refuses)),
        direct_route(),
        &std::sync::Arc::default(),
        as_clock(),
    );
    let Err(failed) = sent else {
        panic!("an unsigned call sent anyway");
    };
    assert!(failed.sent.is_none(), "no send stamp on a signing failure");
    // A refused signed header.
    let (sent, _) = observed(
        url.clone(),
        &[],
        b"{}",
        Some(std::sync::Arc::new(BadHeaderValue)),
        direct_route(),
        &std::sync::Arc::default(),
        as_clock(),
    );
    let Err(failed) = sent else {
        panic!("a call with a refused signed header sent anyway");
    };
    assert!(failed.sent.is_none(), "no send stamp on a refused header");
    // A refused fixed header fails the same way, before the send.
    let (sent, _) = observed(
        url.clone(),
        &[("x-test".to_owned(), "bad\nvalue".to_owned())],
        b"{}",
        None,
        direct_route(),
        &std::sync::Arc::default(),
        as_clock(),
    );
    let Err(failed) = sent else {
        panic!("a call with a refused fixed header sent anyway");
    };
    let crate::Error::Sign(_) = &failed.error else {
        panic!(
            "a refused fixed header is a sign failure: {:?}",
            failed.error
        );
    };
    assert!(failed.sent.is_none(), "no send stamp on a refused header");
    assert!(server.requests().is_empty(), "nothing was ever sent");
}

#[test]
fn valid_fixed_headers_send_unchanged() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    let url = format!("{}/v1", server.url());
    let clock = fakes::clock::FakeClock::new();
    let (sent, _) = observed(
        url,
        &[
            ("x-test".to_owned(), "fine".to_owned()),
            ("x-cache-key".to_owned(), "s_1".to_owned()),
        ],
        b"{}",
        None,
        direct_route(),
        &std::sync::Arc::default(),
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
    );
    let sent = match sent {
        Ok(sent) => sent,
        Err(failed) => panic!("valid fixed headers failed to send: {:?}", failed.error),
    };
    assert_eq!(sent.status, 200);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("x-test"), Some("fine"));
    assert_eq!(requests[0].header("x-cache-key"), Some("s_1"));
}

/// One failure through both sends, asserting the observed failure matches
/// the ordinary one field for field.
fn assert_same_error(ordinary: &crate::Error, observed: &crate::Error) {
    match (ordinary, observed) {
        (
            crate::Error::Status {
                status: a_status,
                body: a_body,
                retry_after: a_wait,
                should_retry: a_retry,
                error_type: a_type,
                ..
            },
            crate::Error::Status {
                status: b_status,
                body: b_body,
                retry_after: b_wait,
                should_retry: b_retry,
                error_type: b_type,
                ..
            },
        ) => {
            assert_eq!(a_status, b_status);
            assert_eq!(a_body, b_body, "error bodies match byte for byte");
            assert_eq!(a_wait, b_wait);
            assert_eq!(a_retry, b_retry);
            assert_eq!(a_type, b_type);
        }
        (
            crate::Error::Stalled {
                should_retry: a_retry,
                ..
            },
            crate::Error::Stalled {
                should_retry: b_retry,
                ..
            },
        ) => {
            assert_eq!(a_retry, b_retry);
        }
        _ => panic!("different failures: {ordinary:?} vs {observed:?}"),
    }
}

/// One response through both sends: the ordinary failure with the observed
/// one, and the observed byte count.
fn both_sends(
    make: impl Fn() -> fakes::Response,
    expected_bytes: u64,
) -> (crate::Error, crate::Error) {
    let clock = fakes::clock::FakeClock::new();
    let as_clock = || std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>;
    // The ordinary send reads the first answer, the observed send the
    // second, identical one.
    let server = fakes::ProviderServer::start([make(), make()]).unwrap();
    let url = format!("{}/v1", server.url());
    let cancel: std::sync::Arc<super::Cancel> = std::sync::Arc::default();
    let Err(ordinary) = called(url.clone(), &[], b"{}", None, direct_route(), &cancel)
        .0
        .map(|_| ())
    else {
        panic!("the ordinary send unexpectedly succeeded");
    };
    let Err(failed) = observed(url, &[], b"{}", None, direct_route(), &cancel, as_clock())
        .0
        .map(|_| ())
    else {
        panic!("the observed send unexpectedly succeeded");
    };
    let Some(dispatched) = failed.sent else {
        panic!("the observed failure reports no send");
    };
    assert_eq!(dispatched.status, Some(500));
    assert!(
        dispatched.headers_at.is_some(),
        "the error keeps its header time"
    );
    assert_eq!(
        dispatched.bytes_read, expected_bytes,
        "the count holds the bytes actually read"
    );
    (ordinary, failed.error)
}

#[test]
fn error_bodies_match_the_ordinary_path_byte_for_byte() {
    let (ordinary, observed) = both_sends(
        || fakes::Response::status(500, r#"{"error":"boom"}"#),
        r#"{"error":"boom"}"#.len() as u64,
    );
    assert_same_error(&ordinary, &observed);
    let (ordinary, observed) = both_sends(|| fakes::Response::status(500, ""), 0);
    assert_same_error(&ordinary, &observed);
    // Invalid UTF-8 under a JSON content type: both refuse the body the
    // same way.
    let (ordinary, observed) = both_sends(|| fakes::Response::status(500, vec![0xff, 0xfe]), 2);
    assert_same_error(&ordinary, &observed);
    // Invalid UTF-8 under a text content type: both apply the same lossy
    // read.
    let (ordinary, observed) = both_sends(
        || {
            fakes::Response::status(500, vec![b'o', 0xff, b'k'])
                .header("content-type", "text/plain")
        },
        3,
    );
    assert_same_error(&ordinary, &observed);
    // Over the 10 MB body limit: both keep the status with an empty body,
    // while the count holds what was actually read.
    let (ordinary, observed) = both_sends(
        || fakes::Response::status(500, vec![b'x'; 10 * 1024 * 1024 + 1]),
        10 * 1024 * 1024,
    );
    assert_same_error(&ordinary, &observed);
    let crate::Error::Status { body, .. } = &observed else {
        panic!("an overlong 500 is a status failure: {observed:?}");
    };
    assert_eq!(body, "");
}

#[test]
fn both_sends_keep_the_aws_exception_name() {
    let (ordinary, observed) = both_sends(
        || {
            fakes::Response::status(500, r#"{"message":"boom"}"#).header(
                "x-amzn-ErrorType",
                "InternalServerException:http://internal.amazon.com/coral/com.amazon.bedrock/",
            )
        },
        r#"{"message":"boom"}"#.len() as u64,
    );
    assert_same_error(&ordinary, &observed);
    let crate::Error::Status { error_type, .. } = &observed else {
        panic!("a 500 is a status failure: {observed:?}");
    };
    assert_eq!(error_type.as_deref(), Some("InternalServerException"));
}

#[test]
fn the_aws_exception_name_is_the_header_before_its_first_colon() {
    for (header, name) in [
        (
            Some("ThrottlingException:http://internal.amazon.com/coral/com.amazon.bedrock/"),
            Some("ThrottlingException"),
        ),
        (Some(" ThrottlingException "), Some("ThrottlingException")),
        (Some(":http://internal.amazon.com/"), None),
        (Some(""), None),
        (None, None),
    ] {
        let mut response = fakes::Response::status(429, r#"{"message":"slow down"}"#);
        if let Some(header) = header {
            response = response.header("x-amzn-errortype", header);
        }
        let server = fakes::ProviderServer::start([response]).unwrap();
        let crate::Error::Status { error_type, .. } = failed_status(&server) else {
            panic!("not a status failure");
        };
        assert_eq!(error_type.as_deref(), name, "{header:?}");
    }
}

#[test]
fn a_stalled_error_body_keeps_status_and_bytes() {
    let server = fakes::ProviderServer::start([fakes::Response::stall(500, "{", 1000)]).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let (sent, _) = observed(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        limited_route(),
        &std::sync::Arc::default(),
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
    );
    let Err(failed) = sent else {
        panic!("a stalled 500 unexpectedly succeeded");
    };
    let crate::Error::Stalled { .. } = &failed.error else {
        panic!("a stalled 500 is a dropped connection: {:?}", failed.error);
    };
    let Some(dispatched) = failed.sent else {
        panic!("the stalled failure reports no send");
    };
    assert_eq!(dispatched.status, Some(500), "the status is retained");
    assert!(
        dispatched.headers_at.is_some(),
        "the header time is retained"
    );
    assert_eq!(dispatched.bytes_read, 1, "the partial body is counted");
}

#[test]
fn a_stalled_text_error_body_counts_the_bytes_already_read() {
    // Shorter than ureq's 8-byte lossy prefetch: the lossy reader pulls
    // the prefix into its buffer before the stall, so a counter above it
    // reports zero. Counting beneath the buffering keeps the byte read.
    let server = fakes::ProviderServer::start([
        fakes::Response::stall(500, "{", 1000).header("content-type", "text/plain")
    ])
    .unwrap();
    let clock = fakes::clock::FakeClock::new();
    let (sent, _) = observed(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        limited_route(),
        &std::sync::Arc::default(),
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
    );
    let Err(failed) = sent else {
        panic!("a stalled text 500 unexpectedly succeeded");
    };
    let crate::Error::Stalled { .. } = &failed.error else {
        panic!(
            "a stalled text 500 is a dropped connection: {:?}",
            failed.error
        );
    };
    let Some(dispatched) = failed.sent else {
        panic!("the stalled failure reports no send");
    };
    assert_eq!(dispatched.status, Some(500), "the status is retained");
    assert!(
        dispatched.headers_at.is_some(),
        "the header time is retained"
    );
    assert_eq!(
        dispatched.bytes_read, 1,
        "the byte read before the stall is counted"
    );
}

#[test]
fn an_observed_success_counts_the_body() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "hi")]).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let (sent, _) = observed(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        direct_route(),
        &std::sync::Arc::default(),
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
    );
    let sent = match sent {
        Ok(sent) => sent,
        Err(failed) => panic!("the observed send failed: {:?}", failed.error),
    };
    let mut stream = sent.stream;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut stream, &mut text).unwrap();
    assert_eq!(text, "hi");
    assert_eq!(stream.count(), 2);
}

#[test]
fn recorded_targets_hold_no_query_fragment_userinfo_or_secret() {
    let mut secrets = crate::redact::Secrets::default();
    secrets.add(contract::Secret::new("SECRET".to_owned()));
    secrets.add(contract::Secret::new("sig-value".to_owned()));
    // (url, host, path): a `None` path is omitted entirely.
    let encoded_secret = "SECRET"
        .bytes()
        .map(|byte| format!("%{byte:02X}"))
        .collect::<String>();
    let cases: Vec<(String, Option<&str>, Option<&str>)> = vec![
        (
            "http://127.0.0.1:50731/v1/responses".into(),
            Some("127.0.0.1:50731"),
            Some("/v1/responses"),
        ),
        (
            "https://user:pass@example.com:8443/a".into(),
            Some("example.com:8443"),
            Some("/a"),
        ),
        ("https://example.com".into(), Some("example.com"), Some("/")),
        (
            "example.com/a%20b".into(),
            Some("example.com"),
            Some("/a b"),
        ),
        (
            "https://example.com/models/x:streamGenerateContent?alt=sse&key=SECRET#frag".into(),
            Some("example.com"),
            Some("/models/x:streamGenerateContent"),
        ),
        (
            "https://example.com/a%2Fb%2fc".into(),
            Some("example.com"),
            Some("/a/b/c"),
        ),
        ("https://example.com/%FF".into(), Some("example.com"), None),
        (
            format!("https://example.com/%FF{encoded_secret}"),
            Some("example.com"),
            None,
        ),
        (
            "https://example.com/v1/SECRET-key".into(),
            Some("example.com"),
            Some("/v1/[redacted]-key"),
        ),
        (
            format!("https://example.com/v1/{encoded_secret}"),
            Some("example.com"),
            Some("/v1/[redacted]"),
        ),
        (
            "https://example.com/?token=sig-value".into(),
            Some("example.com"),
            Some("/"),
        ),
        (
            "https://SECRET@example.com/".into(),
            Some("example.com"),
            Some("/"),
        ),
        (
            "https://api-SECRET.example.com:8443/x".into(),
            Some("api-[redacted].example.com:8443"),
            Some("/x"),
        ),
        (
            "https://example.com/100%sure".into(),
            Some("example.com"),
            Some("/100%sure"),
        ),
        (
            "https://example.com/a%".into(),
            Some("example.com"),
            Some("/a%"),
        ),
    ];
    for (url, host, path) in cases {
        let (found_host, found_path) = super::sanitized_target(&url, &secrets);
        assert_eq!(found_host.as_deref(), host, "host of {url}");
        assert_eq!(found_path.as_deref(), path, "path of {url}");
        for value in [&found_host, &found_path].into_iter().flatten() {
            assert!(!value.contains("SECRET"), "no secret in {url}: {value}");
            assert!(
                !value.contains("sig-value"),
                "no header value in {url}: {value}"
            );
            assert!(!value.contains('?'), "no query in {url}: {value}");
            assert!(!value.contains('#'), "no fragment in {url}: {value}");
        }
        if let Some(path) = &found_path {
            assert!(!path.is_empty(), "a recorded path is never empty");
        }
    }
}

#[test]
fn the_server_receives_the_original_query_while_the_record_drops_it() {
    struct Capture {
        lines: std::sync::Mutex<Vec<contract::diag::HttpRequest>>,
    }
    impl contract::diag::DebugLog for Capture {
        fn debugging(&self) -> bool {
            true
        }
        fn http_request(&self, request: &contract::diag::HttpRequest) {
            self.lines.lock().unwrap().push(request.clone());
        }
    }
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    let secret = "SECRET-VALUE";
    let url = format!(
        "{}/models/x:streamGenerateContent?alt=sse&key={secret}",
        server.url()
    );
    let log = std::sync::Arc::new(Capture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let observation = crate::RequestObservation::new(
        std::sync::Arc::clone(&log) as std::sync::Arc<dyn contract::diag::DebugLog>,
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
        std::num::NonZeroU32::MIN,
    )
    .with_model("gemini-2.0-flash".to_owned());
    let mut secrets = crate::redact::Secrets::default();
    secrets.add(contract::Secret::new(secret.to_owned()));
    let mut finish =
        crate::call::ObservationFinish::new(&observation, "gemini", &url, 7, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(secrets);
    finish.set_bytes(2);
    finish.finish();
    let lines = log.lines.lock().unwrap().clone();
    assert_eq!(lines.len(), 1);
    let line = serde_json::to_vec(&lines[0]).unwrap();
    let text = String::from_utf8(line).unwrap();
    assert!(
        !text.contains(secret),
        "the key is nowhere in the line: {text}"
    );
    assert!(
        !text.contains('?'),
        "the query is nowhere in the line: {text}"
    );
    assert!(text.contains("/models/x:streamGenerateContent"), "{text}");
    let requests = server.requests();
    assert!(
        requests.is_empty(),
        "this unit path sends nothing by itself"
    );
}

#[test]
fn an_observed_out_of_range_retry_after_stays_unset() {
    // The usage-limit body with a date would yield a 7,200 s wait on its
    // own: a present `retry-after` vetoes that fallback, and an infinite
    // or negative one leaves `retry_after` unset on the observed path too.
    for retry_after in ["Infinity", "-5"] {
        let server = fakes::ProviderServer::start([fakes::Response::status(429, USAGE_LIMIT_BODY)
            .header("date", USAGE_DATE)
            .header("retry-after", retry_after)])
        .unwrap();
        let clock = fakes::clock::FakeClock::new();
        let (sent, _) = observed(
            format!("{}/v1", server.url()),
            &[],
            b"{}",
            None,
            direct_route(),
            &std::sync::Arc::default(),
            std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
        );
        let Err(failed) = sent else {
            panic!("a 429 with retry-after: {retry_after} was not a status failure");
        };
        let crate::Error::Status {
            retry_after: waited,
            ..
        } = &failed.error
        else {
            panic!(
                "a 429 with retry-after: {retry_after} was not a status failure: {:?}",
                failed.error
            );
        };
        assert_eq!(*waited, None, "retry-after: {retry_after}");
        let Some(dispatched) = failed.sent else {
            panic!("the observed failure reports no send");
        };
        assert_eq!(dispatched.status, Some(429));
    }
}

/// Captures the lines one record writes, for the redaction tests below.
struct RedactCapture {
    lines: std::sync::Mutex<Vec<contract::diag::HttpRequest>>,
}

impl contract::diag::DebugLog for RedactCapture {
    fn debugging(&self) -> bool {
        true
    }
    fn http_request(&self, request: &contract::diag::HttpRequest) {
        self.lines.lock().unwrap().push(request.clone());
    }
}

impl RedactCapture {
    fn serialized(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap()
            .iter()
            .map(|line| String::from_utf8(serde_json::to_vec(line).unwrap()).unwrap())
            .collect()
    }
}

/// Percent-encodes `secret`, byte by byte, in upper hex.
fn encoded(secret: &str) -> String {
    secret.bytes().map(|byte| format!("%{byte:02X}")).collect()
}

#[test]
fn a_secret_in_the_path_raw_and_encoded_reaches_no_line() {
    // The query is stripped before redaction matters, so the secret hides
    // in the path twice: raw and percent-encoded. Without the redaction
    // set neither form may reach the serialized line.
    let secret = "sk-live-path-secret";
    let url = format!("http://127.0.0.1:9/v1/{secret}/files/{}", encoded(secret));
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let observation = crate::RequestObservation::new(
        std::sync::Arc::clone(&log) as std::sync::Arc<dyn contract::diag::DebugLog>,
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
        std::num::NonZeroU32::MIN,
    )
    .with_model("m".to_owned());
    let mut secrets = crate::redact::Secrets::default();
    secrets.add(contract::Secret::new(secret.to_owned()));
    let mut finish =
        crate::call::ObservationFinish::new(&observation, "fake", &url, 7, clock.now());
    finish.sent(clock.now(), 200);
    finish.set_redact(secrets);
    finish.set_bytes(2);
    finish.finish();
    let lines = log.serialized();
    assert_eq!(lines.len(), 1);
    assert!(
        !lines[0].contains(secret),
        "neither the raw nor the encoded secret is in the line: {}",
        lines[0]
    );
    assert!(lines[0].contains("[redacted]"), "{}", lines[0]);
}

/// Reads the body and answers done: a decoder with no protocol framing, so
/// the test pins the call's redaction, not a protocol's decoding.
#[allow(
    clippy::result_large_err,
    reason = "the decode carries its partial alongside the error for the call's usage"
)]
fn plain_decode(
    stream: &mut dyn std::io::BufRead,
    _: contract::events::CacheLifetime,
    _: &mut dyn FnMut(contract::provider::Delta),
) -> Result<contract::provider::Reply, (crate::Error, Option<contract::provider::CallUsage>)> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut *stream, &mut text).unwrap();
    Ok(contract::provider::Reply {
        actions: Vec::new(),
        finish: contract::provider::Finish::Completed,
        generation_id: None,
        tokens: contract::shapes::Tokens {
            input: 0,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 0,
        },
        web_searches: None,
        cost: None,
        input_size: contract::provider::InputSize {
            bytes: 0,
            media: false,
        },
    })
}

#[test]
fn a_fixed_header_only_secret_is_redacted_from_the_recorded_path() {
    // The secret lives only in a fixed header value: no key, no signer, so
    // the send's own secrets are empty and only the call's fixed-header
    // enrichment can redact the secret the path carries. Without it the
    // recorded path leaks the value.
    use contract::provider::ModelCall as _;
    let secret = "fixed-header-secret-value";
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    let endpoint = crate::Endpoint {
        provider: "fake".to_owned(),
        model: "m".to_owned(),
        base_url: server.url(),
        key: None,
        headers: vec![("x-api-key".to_owned(), secret.to_owned())],
        direct: true,
        ..crate::Endpoint::default()
    };
    let request = contract::provider::ModelRequest {
        system_prompt: "You are terse.".to_owned(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: contract::events::CacheLifetime::OneHour,
        cache_key: "session_1".to_owned(),
        conversation: (vec![contract::provider::Input::User {
            text: "hi".to_owned(),
            images: Vec::new(),
        }])
        .into(),
        sent_tools: None,
        previous_end: None,
        max_output_tokens: None,
        media: Default::default(),
    };
    let url = format!("{}/v1/{secret}/chat", server.url());
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let observation = crate::RequestObservation::new(
        std::sync::Arc::clone(&log) as std::sync::Arc<dyn contract::diag::DebugLog>,
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
        std::num::NonZeroU32::MIN,
    )
    .with_model("m".to_owned());
    let call = crate::call::Call::new(
        &endpoint,
        &request,
        url,
        vec![("x-api-key".to_owned(), secret.to_owned())],
        b"{}".to_vec(),
        plain_decode,
    )
    .observed(observation);
    let reply = fakes::within(
        "the call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = call.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    assert!(reply.0.is_ok(), "the call reads: {:?}", reply.0);
    let lines = log.serialized();
    assert_eq!(lines.len(), 1);
    assert!(
        !lines[0].contains(secret),
        "the fixed-header secret is nowhere in the line: {}",
        lines[0]
    );
}

#[test]
fn a_fixed_bearer_token_is_redacted_from_the_recorded_path() {
    // The bare token lives only inside a fixed `Authorization` value: no
    // key, no signer, so only the call's fixed-header enrichment can
    // redact the token the path carries raw and percent-encoded. The
    // complete value alone never matches the bare token, so without the
    // bare-credential enrichment the recorded path leaks it.
    use contract::provider::ModelCall as _;
    let secret = "fixed-bearer-token-value";
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    let endpoint = crate::Endpoint {
        provider: "fake".to_owned(),
        model: "m".to_owned(),
        base_url: server.url(),
        key: None,
        headers: vec![("authorization".to_owned(), format!("Bearer {secret}"))],
        direct: true,
        ..crate::Endpoint::default()
    };
    let request = contract::provider::ModelRequest {
        system_prompt: "You are terse.".to_owned(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: contract::events::CacheLifetime::OneHour,
        cache_key: "session_1".to_owned(),
        conversation: (vec![contract::provider::Input::User {
            text: "hi".to_owned(),
            images: Vec::new(),
        }])
        .into(),
        sent_tools: None,
        previous_end: None,
        max_output_tokens: None,
        media: Default::default(),
    };
    let encoded_secret: String = secret.bytes().map(|byte| format!("%{byte:02X}")).collect();
    let url = format!("{}/v1/{secret}/files/{encoded_secret}", server.url());
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let observation = crate::RequestObservation::new(
        std::sync::Arc::clone(&log) as std::sync::Arc<dyn contract::diag::DebugLog>,
        std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
        std::num::NonZeroU32::MIN,
    )
    .with_model("m".to_owned());
    let call = crate::call::Call::new(
        &endpoint,
        &request,
        url,
        vec![("authorization".to_owned(), format!("Bearer {secret}"))],
        b"{}".to_vec(),
        plain_decode,
    )
    .observed(observation);
    let reply = fakes::within(
        "the call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = call.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    assert!(reply.0.is_ok(), "the call reads: {:?}", reply.0);
    let lines = log.serialized();
    assert_eq!(lines.len(), 1);
    assert!(
        !lines[0].contains(secret),
        "the bare bearer token is nowhere in the line: {}",
        lines[0]
    );
    assert!(lines[0].contains("[redacted]"), "{}", lines[0]);
    // The request itself is unchanged: the server still saw the secret.
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].path.contains(secret),
        "the request keeps its path: {}",
        requests[0].path
    );
}

#[test]
fn percent_decode_decodes_exact_bytes() {
    let cases: Vec<(&[u8], &[u8])> = vec![
        (b"", b""),
        (b"plain", b"plain"),
        (b"a%2Fb", b"a/b"),
        (b"a%2fb", b"a/b"),
        (b"%41%42", b"AB"),
        (b"%%41", b"%A"),
        (b"%4%41", b"%4A"),
        (b"%%%41", b"%%A"),
        (b"%zz", b"%zz"),
        (b"%2", b"%2"),
        (b"%", b"%"),
        (b"100%sure", b"100%sure"),
        (b"%FF", &[0xFF]),
        (b"a%20b", b"a b"),
    ];
    for (raw, expected) in cases {
        assert_eq!(
            super::percent_decode(raw),
            expected.to_vec(),
            "decoding {raw:?}"
        );
    }
}

/// One header map with this `content-type`, or none when absent.
fn content_headers(content_type: Option<&str>) -> ureq::http::HeaderMap {
    let mut headers = ureq::http::HeaderMap::new();
    if let Some(value) = content_type {
        headers.insert("content-type", value.parse().unwrap());
    }
    headers
}

#[test]
fn is_text_body_matches_only_the_text_mime() {
    // (content-type, expected): the prefix before `;` decides, so only
    // `text/*` decodes lossy while anything else stays strict.
    let cases: Vec<(Option<&str>, bool)> = vec![
        (Some("text/plain"), true),
        (Some("text/html; charset=utf-8"), true),
        (None, false),
        (Some("application/json"), false),
        (Some("application/octet-stream"), false),
    ];
    for (content_type, expected) in cases {
        assert_eq!(
            super::is_text_body(&content_headers(content_type)),
            expected,
            "content-type: {content_type:?}"
        );
    }
}

#[test]
fn lossy_question_replaces_each_invalid_byte_with_one_question() {
    // (raw, expected): pure-invalid, mixed, cut-short, empty and valid
    // passthrough. Every nonempty-invalid row returns a nonempty string
    // that is neither empty nor a constant, and needs the loop to run.
    let cases: Vec<(&[u8], &str)> = vec![
        (&[0xFF, 0xFE], "??"),
        (b"a\xFFb", "a?b"),
        (b"a\xE2", "a?"),
        (b"", ""),
        ("h\u{e9}llo".as_bytes(), "h\u{e9}llo"),
    ];
    for (raw, expected) in cases {
        assert_eq!(super::lossy_question(raw), expected, "raw: {raw:?}");
    }
}

#[test]
fn decode_error_body_splits_text_lossy_from_strict() {
    // Text decodes exactly as the lossy reader; anything else must be
    // valid UTF-8, else the body stays empty as the ordinary path does.
    let text_cases: Vec<(&[u8], &str)> = vec![
        (&[0xFF, 0xFE], "??"),
        (b"a\xFFb", "a?b"),
        (b"a\xE2", "a?"),
        (b"", ""),
        ("h\u{e9}llo".as_bytes(), "h\u{e9}llo"),
    ];
    for (raw, expected) in text_cases {
        assert_eq!(
            super::decode_error_body(raw, true),
            expected,
            "text raw: {raw:?}"
        );
    }
    assert_eq!(super::decode_error_body(&[0xFF, 0xFE], false), "");
    assert_eq!(super::decode_error_body(b"", false), "");
    assert_eq!(
        super::decode_error_body("h\u{e9}llo".as_bytes(), false),
        "h\u{e9}llo"
    );
}

/// One scripted non-2xx response carrying exactly one `content-type`
/// header: `Response::status` defaults to JSON, so a second chained
/// header would send two content types and leave which one ureq reports
/// ambiguous.
fn error_response(status: u16, content_type: &str, body: Vec<u8>) -> fakes::Response {
    fakes::Response {
        status,
        headers: vec![("content-type".to_owned(), content_type.to_owned())],
        body,
        drop_connection: false,
        stall: false,
    }
}

#[test]
fn observed_error_bodies_pin_the_text_strict_split() {
    let clock = fakes::clock::FakeClock::new();
    let as_clock = || std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>;
    // A text error body decodes lossy: the recorded body is the exact
    // `?`-replaced string and the count is the raw byte count.
    let raw = vec![b'o', 0xFF, b'k'];
    let server =
        fakes::ProviderServer::start([error_response(500, "text/plain", raw.clone())]).unwrap();
    let (sent, _) = observed(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        direct_route(),
        &std::sync::Arc::default(),
        as_clock(),
    );
    let Err(failed) = sent else {
        panic!("a text 500 unexpectedly succeeded");
    };
    let crate::Error::Status { body, .. } = &failed.error else {
        panic!("a text 500 is a status failure: {:?}", failed.error);
    };
    assert_eq!(body, "o?k");
    let Some(dispatched) = failed.sent else {
        panic!("the text failure reports no send");
    };
    assert_eq!(dispatched.bytes_read, raw.len() as u64);
    // The same invalid bytes under JSON stay strict: an empty recorded
    // body while the count still holds the bytes actually read.
    let raw = vec![0xFF, 0xFE];
    let server =
        fakes::ProviderServer::start([error_response(500, "application/json", raw.clone())])
            .unwrap();
    let (sent, _) = observed(
        format!("{}/v1", server.url()),
        &[],
        b"{}",
        None,
        direct_route(),
        &std::sync::Arc::default(),
        as_clock(),
    );
    let Err(failed) = sent else {
        panic!("a JSON 500 unexpectedly succeeded");
    };
    let crate::Error::Status { body, .. } = &failed.error else {
        panic!("a JSON 500 is a status failure: {:?}", failed.error);
    };
    assert_eq!(body, "");
    let Some(dispatched) = failed.sent else {
        panic!("the JSON failure reports no send");
    };
    assert_eq!(dispatched.bytes_read, raw.len() as u64);
}

/// A capture Timing for observed calls: records each invocation's
/// arguments and each owned guard's drop, pinning one `net.connect`
/// scope per connector group.
struct TimingCapture {
    calls: std::sync::Mutex<Vec<(contract::diag::Op, Option<String>, bool)>>,
    drops: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl TimingCapture {
    fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            calls: std::sync::Mutex::new(Vec::new()),
            drops: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        })
    }

    fn timing(self: &std::sync::Arc<Self>) -> contract::diag::Timing {
        let me = std::sync::Arc::clone(self);
        std::sync::Arc::new(move |op, tool, started| {
            me.calls
                .lock()
                .unwrap()
                .push((op, tool.map(str::to_owned), started.is_some()));
            let guard = TimingGuard {
                drops: std::sync::Arc::clone(&me.drops),
            };
            Some(Box::new(guard) as Box<dyn Send>)
        })
    }

    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }

    fn arguments(&self) -> Vec<(contract::diag::Op, Option<String>, bool)> {
        self.calls.lock().unwrap().clone()
    }

    fn drops(&self) -> u64 {
        self.drops.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The capture's owned guard: dropping it counts the scope's end.
struct TimingGuard {
    drops: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Drop for TimingGuard {
    fn drop(&mut self) {
        self.drops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// A direct endpoint for `server` holding no key and no signer.
fn test_endpoint(server: &fakes::ProviderServer) -> crate::Endpoint {
    crate::Endpoint {
        provider: "fake".to_owned(),
        model: "m".to_owned(),
        base_url: server.url(),
        direct: true,
        ..crate::Endpoint::default()
    }
}

/// One minimal request: a user turn with no images, tools or rewound state.
fn test_request() -> contract::provider::ModelRequest {
    contract::provider::ModelRequest {
        system_prompt: "You are terse.".to_owned(),
        tools: Vec::new(),
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: contract::events::CacheLifetime::OneHour,
        cache_key: "session_1".to_owned(),
        conversation: (vec![contract::provider::Input::User {
            text: "hi".to_owned(),
            images: Vec::new(),
        }])
        .into(),
        sent_tools: None,
        previous_end: None,
        max_output_tokens: None,
        media: Default::default(),
    }
}

/// One observed call over `endpoint` timing its connector group through
/// `net`: the observation binds the model, the clock and the attempt.
#[allow(
    clippy::too_many_arguments,
    reason = "one observed call with its log, clock and timing beside the call's own"
)]
fn timed_call(
    endpoint: &crate::Endpoint,
    request: &contract::provider::ModelRequest,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    log: &std::sync::Arc<RedactCapture>,
    clock: &std::sync::Arc<fakes::clock::FakeClock>,
    net: &std::sync::Arc<TimingCapture>,
) -> crate::call::Call {
    crate::call::Call::new(endpoint, request, url, headers, body, plain_decode).observed(
        crate::RequestObservation::new(
            std::sync::Arc::clone(log) as std::sync::Arc<dyn contract::diag::DebugLog>,
            std::sync::Arc::clone(clock) as std::sync::Arc<dyn contract::clock::Clock>,
            std::num::NonZeroU32::MIN,
        )
        .with_model("m".to_owned())
        .with_timing(net.timing()),
    )
}

/// The exact invocation every connector scope makes: its own operation,
/// no tool and no explicit stamp.
fn assert_connector_scope(net: &TimingCapture) {
    assert_eq!(
        net.arguments(),
        vec![(contract::diag::Op::NetConnect, None, false)],
        "one group starts one connector scope"
    );
}

#[test]
fn an_observed_open_with_timing_times_one_connector_scope() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    let endpoint = test_endpoint(&server);
    let request = test_request();
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let net = TimingCapture::new();
    let call = timed_call(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        &log,
        &clock,
        &net,
    );
    let worker = std::sync::Arc::clone(&net);
    let text = fakes::within(
        "the observed open to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut body = call.open().expect("the open sends");
            assert_eq!(
                worker.calls(),
                1,
                "the open counts its scope before any read"
            );
            let mut text = String::new();
            std::io::Read::read_to_string(&mut body, &mut text).unwrap();
            text
        },
    );
    assert_eq!(text, "ok", "the observed body reads unchanged");
    // No code under test waits on the clock here; the observed call has returned, so this move only proves later moves add no scope (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(std::time::Duration::from_millis(50));
    assert_eq!(
        net.calls(),
        1,
        "reads, drops and later advances add no scope"
    );
    assert_eq!(net.drops(), 1, "the scope drops once");
    assert_eq!(
        log.serialized().len(),
        1,
        "the http_request line still exists"
    );
}

#[test]
fn an_observed_run_with_timing_times_one_scope_and_reads_the_reply() {
    use contract::provider::ModelCall as _;
    let server = fakes::ProviderServer::start([
        fakes::Response::status(200, "ok"),
        fakes::Response::status(200, "ok"),
    ])
    .unwrap();
    let endpoint = test_endpoint(&server);
    let request = test_request();
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let net = TimingCapture::new();
    let call = timed_call(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        &log,
        &clock,
        &net,
    );
    let reply = fakes::within(
        "the observed call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = call.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    assert!(reply.0.is_ok(), "the observed run reads: {:?}", reply.0);
    assert_connector_scope(&net);
    let plain = crate::call::Call::new(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        plain_decode,
    );
    let plain = fakes::within(
        "the plain call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = plain.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    assert_eq!(
        format!("{:?}", reply.0),
        format!("{:?}", plain.0),
        "the timing changes no reply"
    );
    // No code under test waits on the clock here; the observed call has returned, so this move only proves later moves add no scope (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(std::time::Duration::from_millis(50));
    assert_eq!(net.calls(), 1, "decoding and later advances add no scope");
    assert_eq!(net.drops(), 1, "the scope drops once");
    assert_eq!(
        log.serialized().len(),
        1,
        "the http_request line still exists"
    );
}

#[test]
fn an_observed_run_without_timing_writes_its_line_with_no_net_scope() {
    use contract::provider::ModelCall as _;
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    let endpoint = test_endpoint(&server);
    let request = test_request();
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let call = crate::call::Call::new(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        plain_decode,
    )
    .observed(
        crate::RequestObservation::new(
            std::sync::Arc::clone(&log) as std::sync::Arc<dyn contract::diag::DebugLog>,
            std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
            std::num::NonZeroU32::MIN,
        )
        .with_model("m".to_owned()),
    );
    let reply = fakes::within(
        "the untimed call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = call.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    assert!(reply.0.is_ok(), "the untimed run reads: {:?}", reply.0);
    assert_eq!(
        log.serialized().len(),
        1,
        "the http_request line still exists with no timing"
    );
}

/// A writer at `info`: records whatever lines arrive, so the test pins
/// that none do.
struct InfoCapture {
    lines: std::sync::Mutex<Vec<contract::diag::HttpRequest>>,
}

impl contract::diag::DebugLog for InfoCapture {
    fn debugging(&self) -> bool {
        false
    }

    fn http_request(&self, request: &contract::diag::HttpRequest) {
        self.lines.lock().unwrap().push(request.clone());
    }
}

#[test]
fn an_info_observation_never_invokes_its_timing() {
    use contract::provider::ModelCall as _;
    let server = fakes::ProviderServer::start([
        fakes::Response::status(200, "ok"),
        fakes::Response::status(200, "ok"),
    ])
    .unwrap();
    let endpoint = test_endpoint(&server);
    let request = test_request();
    let log = std::sync::Arc::new(InfoCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let panic_timing: contract::diag::Timing =
        std::sync::Arc::new(|_, _, _| panic!("an info call starts no scope"));
    let observed = crate::call::Call::new(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        plain_decode,
    )
    .observed(
        crate::RequestObservation::new(
            std::sync::Arc::clone(&log) as std::sync::Arc<dyn contract::diag::DebugLog>,
            std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>,
            std::num::NonZeroU32::MIN,
        )
        .with_model("m".to_owned())
        .with_timing(panic_timing),
    );
    let info = fakes::within(
        "the info call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = observed.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    let plain = crate::call::Call::new(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        plain_decode,
    );
    let plain = fakes::within(
        "the plain call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = plain.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    assert_eq!(
        format!("{:?}", info.0),
        format!("{:?}", plain.0),
        "an info call runs as an ordinary one"
    );
    assert!(
        log.lines.lock().unwrap().is_empty(),
        "an info call writes no line"
    );
}

#[test]
fn a_precancelled_call_starts_no_scope() {
    use contract::provider::ModelCall as _;
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    let endpoint = test_endpoint(&server);
    let request = test_request();
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let net = TimingCapture::new();
    let url = format!("{}/v1", server.url());
    let timed = timed_call(
        &endpoint,
        &request,
        url.clone(),
        Vec::new(),
        b"{}".to_vec(),
        &log,
        &clock,
        &net,
    );
    timed.cancel();
    let failed = fakes::within(
        "the cancelled call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = timed.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    let plain = crate::call::Call::new(
        &endpoint,
        &request,
        url,
        Vec::new(),
        b"{}".to_vec(),
        plain_decode,
    );
    plain.cancel();
    let plain = fakes::within(
        "the plain cancelled call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = plain.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    assert_eq!(
        format!("{:?}", failed.0),
        format!("{:?}", plain.0),
        "a cancel before the send keeps its ordinary error"
    );
    assert!(
        server.requests().is_empty(),
        "a cancelled call sends nothing"
    );
    assert_eq!(net.calls(), 0, "a call never sent starts no scope");
    assert!(
        log.serialized().is_empty(),
        "a call never sent writes no line"
    );
}

/// Runs `build`'s call and returns its reply's shape: every pre-send
/// failure below must match its plain twin, send nothing and time nothing.
fn pre_send_outcome(build: impl FnOnce() -> crate::call::Call + Send + 'static) -> String {
    use contract::provider::ModelCall as _;
    fakes::within(
        "the pre-send call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let call = build();
            let mut deltas = Vec::new();
            let reply = call.run(&mut |delta| deltas.push(delta));
            format!("{reply:?}")
        },
    )
}

#[test]
fn signing_and_header_failures_start_no_scope() {
    let refusing = std::sync::Arc::new(Refuses) as std::sync::Arc<dyn contract::signing::Signer>;
    let bad_value =
        std::sync::Arc::new(BadHeaderValue) as std::sync::Arc<dyn contract::signing::Signer>;
    let cases = vec![
        (
            "a signing failure",
            Some(std::sync::Arc::clone(&refusing)),
            Vec::new(),
        ),
        (
            "a refused fixed header",
            None,
            vec![("x-sig\nature".to_owned(), "sig".to_owned())],
        ),
        (
            "a refused signed header",
            Some(std::sync::Arc::clone(&bad_value)),
            Vec::new(),
        ),
    ];
    for (what, signer, headers) in cases {
        let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
        let mut endpoint = test_endpoint(&server);
        endpoint.signer = signer.clone();
        let request = test_request();
        let url = format!("{}/v1", server.url());
        let log = std::sync::Arc::new(RedactCapture {
            lines: std::sync::Mutex::new(Vec::new()),
        });
        let clock = fakes::clock::FakeClock::new();
        let net = TimingCapture::new();
        let timed = timed_call(
            &endpoint,
            &request,
            url.clone(),
            headers.clone(),
            b"{}".to_vec(),
            &log,
            &clock,
            &net,
        );
        let failed = pre_send_outcome(move || timed);
        let plain = crate::call::Call::new(
            &endpoint,
            &request,
            url,
            headers,
            b"{}".to_vec(),
            plain_decode,
        );
        let ordinary = pre_send_outcome(move || plain);
        assert_eq!(failed, ordinary, "{what} keeps its ordinary error");
        assert!(server.requests().is_empty(), "{what} sends nothing");
        assert_eq!(net.calls(), 0, "{what} starts no scope");
        assert_eq!(net.drops(), 0, "{what} drops no scope");
        assert!(log.serialized().is_empty(), "{what} writes no line");
    }
}

#[test]
fn a_refused_observed_run_times_one_scope_without_milestones() {
    use contract::provider::ModelCall as _;
    let endpoint = crate::Endpoint {
        base_url: fakes::refused::url(),
        direct: true,
        ..crate::Endpoint::default()
    };
    let request = test_request();
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let net = TimingCapture::new();
    let url = format!("{}/v1", fakes::refused::url());
    let timed = timed_call(
        &endpoint,
        &request,
        url.clone(),
        Vec::new(),
        b"{}".to_vec(),
        &log,
        &clock,
        &net,
    );
    let failed = fakes::within(
        "the refused call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = timed.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    let plain = crate::call::Call::new(
        &endpoint,
        &request,
        url,
        Vec::new(),
        b"{}".to_vec(),
        plain_decode,
    );
    let ordinary = fakes::within(
        "the plain refused call to return",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut deltas = Vec::new();
            let reply = plain.run(&mut |delta| deltas.push(delta));
            (reply, deltas)
        },
    );
    assert_eq!(
        format!("{:?}", failed.0),
        format!("{:?}", ordinary.0),
        "a refused send keeps its ordinary error"
    );
    assert_connector_scope(&net);
    assert_eq!(net.drops(), 1, "the refused group drops once");
    let lines = log.lines.lock().unwrap().clone();
    assert_eq!(lines.len(), 1, "a sent-but-failed call still records");
    assert_eq!(lines[0].status, None, "no response means no status");
    assert_eq!(lines[0].headers_ms, None, "no headers means no header time");
}

#[test]
fn an_observed_proxy_send_times_its_one_socket() {
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let proxy = fakes::ConnectProxy::start().unwrap();
    let target = target_of(&server);
    let url = format!("{}/v1", server.url());
    let clock = fakes::clock::FakeClock::new();
    let as_clock = || std::sync::Arc::clone(&clock) as std::sync::Arc<dyn contract::clock::Clock>;
    let net = TimingCapture::new();
    let route = super::Route {
        via: super::Via::Through(proxy_through(&proxy)),
        limits: net::LIMITS,
    };
    let clock = as_clock();
    let timing = net.timing();
    let cancel = std::sync::Arc::default();
    let (sent, body, _) = fakes::within(
        "the observed proxy send and response body to be read",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
            let mut secrets = crate::redact::Secrets::default();
            let sent = super::post_signed_observed(
                &url,
                &[],
                b"{}",
                None,
                &route,
                &cancel,
                &mut secrets,
                &clock,
                Some(&timing),
            );
            match sent {
                Ok(mut sent) => {
                    let mut text = String::new();
                    let body =
                        std::io::Read::read_to_string(&mut sent.stream, &mut text).map(|_| text);
                    (Ok(()), body, secrets)
                }
                Err(failed) => (Err(failed), Ok(String::new()), secrets),
            }
        },
    );
    match sent {
        Ok(()) => {}
        Err(failed) => panic!("the observed proxy send failed: {:?}", failed.error),
    }
    let text = body.unwrap();
    assert_eq!(text, "{}", "the proxy send reached the origin");
    assert!(
        proxy.await_connects(1, Deadline::after(CONNECT_WITHIN)),
        "the proxy recorded CONNECT {target}"
    );
    assert_eq!(proxy.connects(), [target]);
    assert_connector_scope(&net);
    assert_eq!(net.drops(), 1, "the proxy socket drops once");
}

#[test]
fn the_net_scope_ends_while_the_response_is_held() {
    use contract::provider::ModelCall as _;
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "ok")]).unwrap();
    server.hold();
    let endpoint = test_endpoint(&server);
    let request = test_request();
    let log = std::sync::Arc::new(RedactCapture {
        lines: std::sync::Mutex::new(Vec::new()),
    });
    let clock = fakes::clock::FakeClock::new();
    let net = TimingCapture::new();
    let call = timed_call(
        &endpoint,
        &request,
        format!("{}/v1", server.url()),
        Vec::new(),
        b"{}".to_vec(),
        &log,
        &clock,
        &net,
    );
    let (done_tx, done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut deltas = Vec::new();
        let reply = call.run(&mut |delta| deltas.push(delta));
        match done_tx.send(format!("{reply:?}")) {
            Ok(()) | Err(_) => {}
        }
    });
    assert!(
        server.await_requests(1, Deadline::after(REQUEST_WITHIN)),
        "the server saw the call while holding its response"
    );
    assert_connector_scope(&net);
    assert_eq!(
        net.drops(),
        1,
        "the net scope is already complete while headers stay blocked"
    );
    // No code under test waits on the clock here; the call is blocked in the held socket read, and this move is the elapsed time the line records (ruling 6179406086).
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(std::time::Duration::from_millis(50));
    server.release();
    let reply =
        Deadline::after(MUST_SUCCEED_WITHIN).recv_or_fail(&done_rx, "the held call to return");
    assert!(
        reply.contains("Ok("),
        "the held call reads after release: {reply}"
    );
    assert_eq!(net.calls(), 1, "the later work adds no net event");
    let lines = log.lines.lock().unwrap().clone();
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].total_ms >= 50,
        "the provider duration still covers the held work: {}",
        lines[0].total_ms
    );
}
