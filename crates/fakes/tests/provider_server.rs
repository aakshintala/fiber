//! The provider server through its public API, driven by a plain HTTP/1.1
//! client over a `TcpStream`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Request, Response, fingerprint};

/// The deadline on each wait: one exchange (connecting, writing the whole
/// request, reading the whole reply) is one wait, and a stall fails naming the
/// step. A test makes at most 3 exchanges and an `await_requests` test adds one
/// wait: 40 s, at most half of nextest's 120 s kill (`docs/testing.md`,
/// "Waits and timeouts").
const DEADLINE: Duration = fakes::MUST_SUCCEED_WITHIN;

fn addr(server: &ProviderServer) -> SocketAddr {
    server.url().trim_start_matches("http://").parse().unwrap()
}

/// What a client read back: the status code, the response headers with
/// lowercase names, and the body bytes.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn post(server: &ProviderServer, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let mut request = format!("POST {path} HTTP/1.1\r\nHost: fake\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    let mut request = request.into_bytes();
    request.extend_from_slice(body);
    exchange(server, &request)
}

/// Sends `request` as written, ends the write side, and reads the reply to
/// the end of the connection. A client thread does the I/O and reports each
/// step, so each wait has one overall deadline and a stall names its step.
fn exchange(server: &ProviderServer, request: &[u8]) -> Reply {
    let (addr, request) = (addr(server), request.to_vec());
    let (tx, rx) = mpsc::channel();
    let step = Arc::new(Mutex::new("connecting to the fake provider"));
    let client_step = Arc::clone(&step);
    thread::spawn(move || {
        let enter = |name: &'static str| *client_step.lock().unwrap() = name;
        let steps = || -> io::Result<Vec<u8>> {
            let mut stream = TcpStream::connect(addr)?;
            enter("writing the request to the fake provider");
            stream.write_all(&request)?;
            stream.shutdown(Shutdown::Write)?;
            enter("reading the fake provider's whole reply");
            let mut raw = Vec::new();
            stream.read_to_end(&mut raw)?;
            Ok(raw)
        };
        tx.send(steps()).unwrap();
    });
    // One deadline for the whole exchange; the step names where it stalled.
    let raw = match rx.recv_timeout(DEADLINE) {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => panic!("{}: {e}", step.lock().unwrap()),
        Err(_) => panic!("{}: not done within {DEADLINE:?}", step.lock().unwrap()),
    };
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split(' ')
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|l| {
            let (n, v) = l.split_once(':').unwrap();
            (n.to_ascii_lowercase(), v.trim().to_owned())
        })
        .collect();
    Reply {
        status,
        headers,
        body: raw[split + 4..].to_vec(),
    }
}

fn header<'a>(reply: &'a Reply, name: &str) -> Option<&'a str> {
    reply
        .headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// The response bytes of the first exchange in a probe's recording.
fn recorded(file: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../research")
        .join(file);
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    json[0]["raw_sse"].as_str().unwrap().as_bytes().to_vec()
}

#[test]
fn it_serves_a_recorded_stream_byte_for_byte() {
    let bytes = recorded("anthropic-messages-probe/raw/stream.json");
    assert!(bytes.starts_with(b"event: message_start\n"));
    let server = ProviderServer::start([Response::stream(bytes.clone())]).unwrap();

    let reply = post(&server, "/v1/messages", &[], b"{}");

    assert_eq!(reply.status, 200);
    assert_eq!(header(&reply, "content-type"), Some("text/event-stream"));
    assert_eq!(reply.body, bytes);
}

#[test]
fn it_serves_a_scripted_sequence_one_response_per_request() {
    let tool_call = "event: message_start\ndata: {\"type\":\"tool_use\"}\n\n";
    let text = "event: content_block_delta\ndata: {\"text\":\"hi\"}\n\n";
    let server = ProviderServer::start([
        Response::stream(tool_call),
        Response::status(429, r#"{"error":"rate_limited"}"#).header("retry-after", "7"),
        Response::stream(text),
    ])
    .unwrap();

    let first = post(&server, "/v1/messages", &[], b"1");
    let second = post(&server, "/v1/messages", &[], b"2");
    let third = post(&server, "/v1/messages", &[], b"3");

    assert_eq!(
        (first.status, first.body),
        (200, tool_call.as_bytes().to_vec())
    );
    assert_eq!(second.status, 429);
    assert_eq!(header(&second, "retry-after"), Some("7"));
    assert_eq!(header(&second, "content-type"), Some("application/json"));
    assert_eq!(second.body, br#"{"error":"rate_limited"}"#);
    assert_eq!((third.status, third.body), (200, text.as_bytes().to_vec()));
}

#[test]
fn a_request_past_the_end_of_the_script_gets_a_500_naming_why() {
    let server = ProviderServer::start([Response::stream("data: x\n\n")]).unwrap();
    post(&server, "/", &[], b"");

    let reply = post(&server, "/", &[], b"");

    assert_eq!(reply.status, 500);
    assert!(
        String::from_utf8(reply.body)
            .unwrap()
            .contains("no scripted response left")
    );
    assert_eq!(server.requests().len(), 2);
}

/// `printf 'sk-secret' | shasum -a 256` begins `746b4ad1`. Pinning that
/// literal here means [`fingerprint`] cannot drift from SHA-256.
#[test]
fn fingerprint_of_sk_secret_is_the_prefix_computed_outside_rust() {
    assert_eq!(fingerprint("sk-secret"), "sha256:746b4ad1");
}

#[test]
fn it_records_each_request_with_credential_fingerprints() {
    let server = ProviderServer::start([Response::stream("a"), Response::stream("b")]).unwrap();

    post(
        &server,
        "/v1/messages",
        &[
            ("Authorization", "Bearer sk-secret"),
            ("x-api-key", "sk-secret"),
            ("X-Goog-Api-Key", "sk-secret"),
            ("api-key", "sk-secret"),
            ("cookie", "sk-secret"),
            ("Proxy-Authorization", "Bearer sk-secret"),
            ("anthropic-version", "2023-06-01"),
        ],
        br#"{"model":"m"}"#,
    );
    post(
        &server,
        "/v1beta/models/m:streamGenerateContent?alt=sse&key=sk-secret",
        &[],
        b"second",
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let first: &Request = &requests[0];
    assert_eq!(first.method, "POST");
    assert_eq!(first.path, "/v1/messages");
    assert_eq!(first.body, br#"{"model":"m"}"#);
    let secret = fingerprint("sk-secret");
    let bearer = fingerprint("Bearer sk-secret");
    assert_eq!(first.header("authorization"), Some(bearer.as_str()));
    assert_eq!(first.header("proxy-authorization"), Some(bearer.as_str()));
    for name in ["x-api-key", "x-goog-api-key", "api-key", "cookie"] {
        assert_eq!(first.header(name), Some(secret.as_str()), "{name}");
    }
    assert_eq!(first.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(first.header("content-length"), Some("13"));

    let second = &requests[1];
    assert_eq!(
        second.path,
        format!("/v1beta/models/m:streamGenerateContent?alt=sse&key={secret}")
    );
    assert_eq!(second.body, b"second");
    let everything = format!("{requests:?}");
    assert!(!everything.contains("sk-secret"), "{everything}");
}

#[test]
fn two_servers_run_side_by_side_on_their_own_ports() {
    let a = ProviderServer::start([Response::stream("a")]).unwrap();
    let b = ProviderServer::start([Response::stream("b")]).unwrap();
    assert_ne!(a.url(), b.url());

    assert_eq!(post(&b, "/", &[], b"").body, b"b");
    assert_eq!(post(&a, "/", &[], b"").body, b"a");
    assert_eq!((a.requests().len(), b.requests().len()), (1, 1));
}

#[test]
fn dropping_the_server_closes_its_port() {
    let server = ProviderServer::start([]).unwrap();
    let addr = addr(&server);
    assert!(TcpStream::connect_timeout(&addr, DEADLINE).is_ok());

    drop(server);

    assert!(TcpStream::connect_timeout(&addr, DEADLINE).is_err());
}

#[test]
fn it_records_a_chunked_body_decoded() {
    let server = ProviderServer::start([Response::stream("ok")]).unwrap();

    let reply = exchange(
        &server,
        b"POST /v1/messages HTTP/1.1\r\nHost: fake\r\nTransfer-Encoding: chunked\r\n\r\n\
          8\r\n{\"model\"\r\nb;ext=1\r\n:\"m\",\"n\":1}\r\n0\r\nx-trailer: t\r\n\r\n",
    );

    assert_eq!(reply.body, b"ok");
    let body = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert_eq!(body, r#"{"model":"m","n":1}"#);
}

#[test]
fn a_malformed_chunked_body_gets_a_400_naming_why_and_is_recorded() {
    let server = ProviderServer::start([Response::stream("ok")]).unwrap();

    let reply = exchange(
        &server,
        b"POST /v1/messages HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcX\r\n0\r\n\r\n",
    );
    let truncated = exchange(
        &server,
        b"POST /v1/messages HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nab",
    );
    let next = post(&server, "/v1/messages", &[], b"{}");

    assert_eq!(reply.status, 400);
    let why = String::from_utf8(reply.body).unwrap();
    assert!(why.contains("not followed by CRLF"), "{why}");
    assert_eq!(truncated.status, 400);
    let why = String::from_utf8(truncated.body).unwrap();
    assert!(why.contains("ends before its framing does"), "{why}");
    // The script kept its response for the next well-formed request.
    assert_eq!((next.status, next.body), (200, b"ok".to_vec()));
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].body, b"abc");
    assert_eq!(requests[1].body, b"ab");
}

/// `await_requests` on a helper, so a mutant that waits out its 30 s `within`
/// fails this wait instead of hanging the test.
fn await_requests_within(server: &Arc<ProviderServer>, count: usize) -> bool {
    let (tx, rx) = mpsc::channel();
    let server = Arc::clone(server);
    thread::spawn(
        move || match tx.send(server.await_requests(count, Duration::from_secs(30))) {
            Ok(()) | Err(mpsc::SendError(_)) => {}
        },
    );
    rx.recv_timeout(DEADLINE)
        .expect("waited for await_requests")
}

#[test]
fn await_requests_is_false_before_any_request() {
    let server = ProviderServer::start([Response::status(200, "ok")]).unwrap();
    assert!(!server.await_requests(1, Duration::ZERO));
}

#[test]
fn await_requests_is_true_when_the_recorded_count_equals_the_wait() {
    let server = Arc::new(ProviderServer::start([Response::status(200, "ok")]).unwrap());
    post(&server, "/one", &[], b"");
    assert_eq!(server.requests().len(), 1);
    assert!(await_requests_within(&server, 1));
}

#[test]
fn await_requests_is_true_when_more_than_the_count_are_recorded() {
    let server = Arc::new(
        ProviderServer::start([Response::status(200, "ok"), Response::status(200, "ok")]).unwrap(),
    );
    post(&server, "/one", &[], b"");
    post(&server, "/two", &[], b"");
    assert!(server.requests().len() > 1);
    assert!(await_requests_within(&server, 1));
}

#[test]
fn only_the_newest_bodies_are_kept_and_older_requests_keep_their_size() {
    let server = ProviderServer::start_with_fallback([], Response::stream("ok"))
        .unwrap()
        .keep_last_bodies(1);

    post(&server, "/v1/messages", &[], b"body-0");
    post(&server, "/v1/messages", &[], b"body-1");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].body.is_empty());
    assert_eq!(requests[1].body, b"body-1");
    assert!(requests.iter().all(|r| r.body_len == 6), "{requests:?}");
    assert_eq!(requests[0].path, "/v1/messages");
}

#[test]
fn keep_all_bodies_overrides_a_smaller_limit() {
    let server = ProviderServer::start_with_fallback([], Response::stream("ok"))
        .unwrap()
        .keep_last_bodies(0)
        .keep_all_bodies();

    post(&server, "/v1/messages", &[], b"body-0");
    post(&server, "/v1/messages", &[], b"body-1");

    let requests = server.requests();
    assert_eq!(requests[0].body, b"body-0");
    assert_eq!(requests[1].body, b"body-1");
}
