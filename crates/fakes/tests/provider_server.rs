//! The provider server through its public API, driven by a plain HTTP/1.1
//! client over a `TcpStream`.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "test code may unwrap and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;

use fakes::{ProviderServer, Request, Response};

/// What a client read back: the status code, the response headers with
/// lowercase names, and the body bytes.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn post(server: &ProviderServer, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Reply {
    let addr = server.url().trim_start_matches("http://").to_owned();
    let mut stream = TcpStream::connect(addr).unwrap();
    let mut head = format!("POST {path} HTTP/1.1\r\nHost: fake\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).unwrap();
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

#[test]
fn it_records_each_request_with_credentials_masked() {
    let server = ProviderServer::start([Response::stream("a"), Response::stream("b")]).unwrap();

    post(
        &server,
        "/v1/messages",
        &[
            ("Authorization", "Bearer sk-secret"),
            ("x-api-key", "sk-secret"),
            ("X-Goog-Api-Key", "sk-secret"),
            ("api-key", "sk-secret"),
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
    for name in ["authorization", "x-api-key", "x-goog-api-key", "api-key"] {
        assert_eq!(first.header(name), Some("<masked>"), "{name}");
    }
    assert_eq!(first.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(first.header("content-length"), Some("13"));

    let second = &requests[1];
    assert_eq!(
        second.path,
        "/v1beta/models/m:streamGenerateContent?alt=sse&key=<masked>"
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
    let addr = server.url().trim_start_matches("http://").to_owned();
    assert!(TcpStream::connect(&addr).is_ok());

    drop(server);

    assert!(TcpStream::connect(&addr).is_err());
}
