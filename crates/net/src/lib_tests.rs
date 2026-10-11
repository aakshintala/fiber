use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use ureq::tls::{RootCerts, TlsConfig};
use ureq::unversioned::resolver::DefaultResolver;

use super::{Error, agent, config, tls_config, tls_config_with};
use fakes::Deadline;

// The test CA and the leaf it signed, generated once with:
//
// ca.cnf:
// [req]
// distinguished_name=dn
// prompt=no
// x509_extensions=v3_ca
// [dn]
// CN=Fiber net test CA
// [v3_ca]
// basicConstraints=critical,CA:TRUE
// keyUsage=critical,keyCertSign,cRLSign
// subjectKeyIdentifier=hash
//
// leaf.cnf:
// [ext]
// basicConstraints=critical,CA:FALSE
// keyUsage=critical,digitalSignature
// extendedKeyUsage=serverAuth
// subjectAltName=IP:127.0.0.1,DNS:localhost
// authorityKeyIdentifier=keyid
//
// openssl ecparam -name prime256v1 -genkey -noout -out ca.key
// openssl req -x509 -new -key ca.key -sha256 -days 36500 -config ca.cnf -out ca.pem
// openssl ecparam -name prime256v1 -genkey -noout -out leaf.key
// openssl req -new -key leaf.key -subj /CN=localhost -out leaf.csr
// openssl x509 -req -in leaf.csr -CA ca.pem -CAkey ca.key -CAcreateserial -sha256 -days 36500 -extfile leaf.cnf -extensions ext -out leaf.pem
// openssl x509 -in leaf.pem -outform DER -out leaf.der
// openssl pkcs8 -topk8 -nocrypt -in leaf.key -outform DER -out leaf.key.der
//
// The keys are test-only and trusted nowhere outside these tests.
const CA_PEM: &[u8] = include_bytes!("../fixtures/ca.pem");
const LEAF_DER: &[u8] = include_bytes!("../fixtures/leaf.der");
const LEAF_KEY_DER: &[u8] = include_bytes!("../fixtures/leaf.key.der");

/// One TLS call's deadline: the client's reply and the server's outcome.
const DEADLINE: Duration = Duration::from_secs(5);

/// One child test's bound, applied twice by `fakes::rerun_within`: the
/// child's exit, then its reaping after a kill.
const CHILD_WITHIN: Duration = Duration::from_secs(10);

/// Present in the re-executed child, carrying the expected selection.
const NET_CHILD: &str = "FIBER_NET_CHILD";

/// The test CA as fallback roots, standing in for Mozilla's list: the
/// connect test trusts only this CA, so a platform-verifier selection fails
/// it.
fn test_ca_roots() -> RootCerts {
    match ureq::tls::Certificate::from_pem(CA_PEM) {
        Ok(cert) => RootCerts::from([cert]),
        Err(err) => panic!("the test CA parses: {err}"),
    }
}

/// The selection as the child checks it: the expectation travels on the
/// environment because the child cannot return a value.
fn roots_name(roots: &RootCerts) -> &'static str {
    match roots {
        RootCerts::WebPki => "webpki",
        RootCerts::PlatformVerifier => "platform",
        RootCerts::Specific(_) | _ => "other",
    }
}

/// A local TLS server answering one connection with 200 `ok`. It binds
/// 127.0.0.1:0 before the client starts; the bound listener is the readiness
/// signal, so no sleep waits for it.
struct Server {
    port: u16,
    outcome: mpsc::Receiver<Result<(), String>>,
}

impl Server {
    fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let (done, outcome) = mpsc::channel();
        match thread::Builder::new()
            .name("net-test-tls-server".to_owned())
            .spawn(move || {
                let result = serve_once(&listener);
                match done.send(result) {
                    Ok(()) | Err(_) => {}
                }
            }) {
            Ok(_) | Err(_) => {}
        }
        Ok(Self { port, outcome })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // A plain connection wakes a still-pending accept, whose handshake
        // then fails at once under the read deadline, so the worker sends
        // its outcome and exits. A listener already gone fails this, which
        // is fine.
        match TcpStream::connect(std::net::SocketAddr::from(([127, 0, 0, 1], self.port))) {
            Ok(_) | Err(_) => {}
        }
    }
}

/// Serves one connection: completes the handshake, reads the request head,
/// answers 200 `ok`. The socket's deadlines bound the handshake too, so a
/// wake-up connection fails at once instead of blocking.
fn serve_once(listener: &TcpListener) -> Result<(), String> {
    let (mut stream, _) = listener
        .accept()
        .map_err(|err| format!("accepting the test connection: {err}"))?;
    stream
        .set_read_timeout(Some(DEADLINE))
        .map_err(|err| format!("arming the read deadline: {err}"))?;
    stream
        .set_write_timeout(Some(DEADLINE))
        .map_err(|err| format!("arming the write deadline: {err}"))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server_config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|err| format!("agreeing TLS versions: {err}"))?
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(LEAF_DER)],
            rustls::pki_types::PrivateKeyDer::try_from(LEAF_KEY_DER)
                .map_err(|_| "the test leaf key parses".to_owned())?,
        )
        .map_err(|err| format!("loading the test leaf: {err}"))?;
    let mut connection = rustls::ServerConnection::new(Arc::new(server_config))
        .map_err(|err| format!("starting the server handshake: {err}"))?;
    let mut tls = rustls::Stream::new(&mut connection, &mut stream);
    let mut head = Vec::new();
    loop {
        let mut chunk = [0_u8; 512];
        let ended = head.ends_with(b"\r\n\r\n");
        if ended {
            break;
        }
        let count = match tls.read(&mut chunk) {
            Ok(0) => return Err("the client closed before sending a head".to_owned()),
            Ok(count) => count,
            Err(err) => return Err(format!("reading the head: {err}")),
        };
        head.extend_from_slice(chunk.get(..count).unwrap_or(&[]));
        if head.len() > 16_384 {
            return Err("the head ran past 16 KiB".to_owned());
        }
    }
    tls.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok")
        .map_err(|err| format!("answering 200: {err}"))?;
    Ok(())
}

/// GETs `https://127.0.0.1:<port>/` with `tls` on a thread and returns the
/// channel the reply arrives on: calling code that blocks is a wait too, so
/// the test receives the result with a deadline. No proxy: the call must
/// reach loopback directly even when the environment names one
/// (`docs/dependencies.md`, "Proxies").
fn get(tls: TlsConfig, port: u16) -> mpsc::Receiver<Result<String, ureq::Error>> {
    let (done, reply) = mpsc::channel();
    match thread::Builder::new()
        .name("net-test-client".to_owned())
        .spawn(move || {
            let built = ureq::config::Config::builder()
                .tls_config(tls)
                .proxy(None)
                .build();
            let caller = agent(
                built,
                Arc::new(()),
                DefaultResolver::default(),
                crate::LIMITS,
            );
            let outcome = match caller.get(format!("https://127.0.0.1:{port}/")).call() {
                Ok(mut response) => response.body_mut().read_to_string(),
                Err(err) => Err(err),
            };
            match done.send(outcome) {
                Ok(()) | Err(_) => {}
            }
        }) {
        Ok(_) | Err(_) => {}
    }
    reply
}

/// Runs `test` in a child whose certificate store is `store_pem` in its own
/// directory: a wrong selection sends the body through the platform
/// verifier, which must then read this store and no machine store. The
/// child inherits no proxy variables, only the store and the expectation.
fn in_child(test: &str, store_pem: &[u8], expected: &str) {
    let dir = fakes::TempDir::new("fiber-net-child");
    let file = dir.path().join("store.pem");
    assert!(
        std::fs::write(&file, store_pem).is_ok(),
        "writing the child store file"
    );
    let store_dir = dir.path().join("store.d");
    assert!(
        std::fs::create_dir(&store_dir).is_ok(),
        "making the child store directory"
    );
    let file_text = file.to_string_lossy().into_owned();
    let dir_text = store_dir.to_string_lossy().into_owned();
    let output = fakes::rerun_within(
        &format!("tests::{test}"),
        &[
            ("SSL_CERT_FILE", file_text.as_str()),
            ("SSL_CERT_DIR", dir_text.as_str()),
            (NET_CHILD, expected),
        ],
        CHILD_WITHIN,
    );
    assert!(
        output.status.success(),
        "child {test} exited {}:\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn an_empty_store_connects_through_the_fallback_roots() {
    if std::env::var_os(NET_CHILD).is_none() {
        in_child(
            "an_empty_store_connects_through_the_fallback_roots",
            &[],
            "fallback",
        );
        return;
    }
    let server = match Server::start() {
        Ok(server) => server,
        Err(err) => panic!("binding the test TLS server: {err}"),
    };
    let reply = get(
        tls_config_with(&OnceLock::new(), || 0, test_ca_roots()),
        server.port,
    );
    match Deadline::after(DEADLINE).recv(&reply) {
        Ok(Ok(body)) => assert_eq!(body, "ok", "the fallback roots reach the test server"),
        Ok(Err(err)) => panic!("requesting through the fallback roots: {err}"),
        Err(_) => panic!("the client answers within {DEADLINE:?}"),
    }
    match Deadline::after(DEADLINE).recv(&server.outcome) {
        Ok(Ok(())) => {}
        Ok(Err(err)) => panic!("serving the test connection: {err}"),
        Err(_) => panic!("the server answers within {DEADLINE:?}"),
    }
}

#[test]
fn a_full_store_keeps_the_platform_verifier() {
    assert!(
        matches!(
            tls_config_with(&OnceLock::new(), || 1, test_ca_roots()).root_certs(),
            RootCerts::PlatformVerifier
        ),
        "a store with one certificate keeps the platform verifier"
    );
}

#[test]
fn the_store_is_loaded_once_and_its_answer_kept() {
    let cache = OnceLock::new();
    let loads = std::cell::Cell::new(0);
    let first = tls_config_with(
        &cache,
        || {
            loads.set(loads.get() + 1);
            0
        },
        test_ca_roots(),
    );
    assert!(
        matches!(first.root_certs(), RootCerts::Specific(_)),
        "an empty store selects the fallback roots"
    );
    let second = tls_config_with(
        &cache,
        || {
            loads.set(loads.get() + 1);
            1
        },
        test_ca_roots(),
    );
    assert!(
        matches!(second.root_certs(), RootCerts::Specific(_)),
        "the cached answer survives a store that later reads full"
    );
    assert_eq!(loads.get(), 1, "the store loads once per cache");
}

#[test]
fn an_empty_store_selects_the_fallback() {
    if std::env::var_os(NET_CHILD).is_none() {
        // `cfg!` picks the expectation at compile time, so the test is
        // compiled on every platform and skipped on none.
        let expected = if cfg!(target_os = "linux") {
            "webpki"
        } else {
            "platform"
        };
        in_child("an_empty_store_selects_the_fallback", &[], expected);
        return;
    }
    let expected = std::env::var(NET_CHILD).unwrap_or_default();
    assert_eq!(
        roots_name(config().build().tls_config().root_certs()),
        expected.as_str(),
        "an empty store selects the expected roots in the agent config"
    );
    assert_eq!(
        roots_name(tls_config().root_certs()),
        expected.as_str(),
        "an empty store selects the expected roots"
    );
    let store = std::env::var("SSL_CERT_FILE").unwrap_or_default();
    assert!(!store.is_empty(), "the child inherits its store file");
    assert!(
        std::fs::write(&store, CA_PEM).is_ok(),
        "refilling the child store file"
    );
    assert_eq!(
        roots_name(tls_config().root_certs()),
        expected.as_str(),
        "the first answer is kept after the store refills"
    );
}

#[test]
fn a_store_with_one_certificate_keeps_the_platform_verifier() {
    if std::env::var_os(NET_CHILD).is_none() {
        in_child(
            "a_store_with_one_certificate_keeps_the_platform_verifier",
            CA_PEM,
            "platform",
        );
        return;
    }
    let expected = std::env::var(NET_CHILD).unwrap_or_default();
    assert_eq!(
        roots_name(config().build().tls_config().root_certs()),
        expected.as_str(),
        "a full store keeps the platform verifier in the agent config"
    );
    assert_eq!(
        roots_name(tls_config().root_certs()),
        expected.as_str(),
        "a full store keeps the platform verifier"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn tls_config_uses_the_platform_verifier() {
    assert!(
        matches!(tls_config().root_certs(), RootCerts::PlatformVerifier),
        "the shared TLS config verifies against the platform store"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn agent_config_carries_the_platform_verifier() {
    assert!(
        matches!(
            config().build().tls_config().root_certs(),
            RootCerts::PlatformVerifier
        ),
        "the shared agent config verifies against the platform store"
    );
}

#[test]
fn a_stopped_call_reports_connection_failed() {
    assert_eq!(Error::Stopped.code(), contract::ErrorCode::ConnectionFailed);
}

#[test]
fn production_limits_are_fifteen_seconds_and_five_minutes() {
    assert_eq!(
        super::LIMITS.connect(),
        Duration::from_secs(15),
        "the per-address connect limit is 15 s"
    );
    assert_eq!(
        super::LIMITS.idle(),
        Duration::from_secs(300),
        "the idle limit is 300 s"
    );
    assert_eq!(
        super::Limits::default(),
        super::LIMITS,
        "the default is the production limits"
    );
}

/// Each single-zero case kills one side of the `||`: with `&&` the
/// one-zero limit would be `Some`.
#[test]
fn limits_new_refuses_a_zero_bound() {
    assert!(
        super::Limits::new(Duration::ZERO, Duration::from_secs(1)).is_none(),
        "a zero connect is refused"
    );
    assert!(
        super::Limits::new(Duration::from_secs(1), Duration::ZERO).is_none(),
        "a zero idle is refused"
    );
    let limits = super::Limits::new(Duration::from_secs(1), Duration::from_millis(200))
        .expect("non-zero bounds build a limit");
    assert_eq!(
        limits.connect(),
        Duration::from_secs(1),
        "the connect getter"
    );
    assert_eq!(limits.idle(), Duration::from_millis(200), "the idle getter");
}

#[test]
fn timed_out_is_true_for_timeouts_and_false_for_other_failures() {
    assert!(
        super::timed_out(&ureq::Error::Timeout(ureq::Timeout::Global)),
        "ureq's own deadline is a timeout"
    );
    assert!(
        super::timed_out(&ureq::Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "waited out"
        ))),
        "a timed-out socket is a timeout"
    );
    assert!(
        !super::timed_out(&ureq::Error::HostNotFound),
        "a missing host is not a timeout"
    );
}

#[test]
fn timed_out_is_false_for_a_refused_peer() {
    // Negate-check: flipping the kind guard to true would report this
    // refusal as a timeout.
    let refused = ureq::Error::Io(std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "refused",
    ));
    assert!(
        !super::timed_out(&refused),
        "a refused peer is not a timeout"
    );
}

#[test]
fn stall_and_connect_timeout_report_connection_failed() {
    assert_eq!(
        Error::Stalled {
            idle: Duration::from_millis(200)
        }
        .code(),
        contract::ErrorCode::ConnectionFailed
    );
    assert_eq!(
        Error::ConnectTimedOut {
            addr: std::net::SocketAddr::from(([127, 0, 0, 1], 9)),
            limit: Duration::from_secs(15),
        }
        .code(),
        contract::ErrorCode::ConnectionFailed
    );
}
