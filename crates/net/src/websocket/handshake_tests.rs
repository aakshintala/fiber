use super::*;
use std::io::Cursor;

const KEY: &str = "dGhlIHNhbXBsZSBub25jZQ==";
const ACCEPT: &str = "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=";
fn switching() -> String {
    format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {ACCEPT}\r\n\r\n"
    )
}
fn endpoint(secure: bool, host: &str, port: u16) -> Endpoint {
    Endpoint {
        secure,
        host: host.into(),
        port,
        target: "/hub".into(),
    }
}

#[test]
fn rfc_accept_and_hub_reply_preserve_the_first_frame() {
    assert_eq!(accept(KEY), ACCEPT);
    let mut bytes = switching().into_bytes();
    bytes.extend_from_slice(b"\x81\x09hub_hello");
    let mut cursor = Cursor::new(bytes);
    let mut tail = reply(&mut cursor, KEY).unwrap();
    cursor.read_to_end(&mut tail).unwrap();
    assert_eq!(
        super::super::frame::Reader::default().next(&mut Cursor::new(tail)),
        super::super::frame::Incoming::Text("hub_hello".into())
    );
}

#[test]
fn request_matches_the_wire_and_formats_authorities() {
    assert_eq!(
        request(&endpoint(true, "box.tailnet.ts.net", 443), KEY),
        format!(
            "GET /hub HTTP/1.1\r\nHost: box.tailnet.ts.net\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {KEY}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
    );
    for (ep, host) in [
        (endpoint(false, "box", 80), "box"),
        (endpoint(true, "box", 443), "box"),
        (endpoint(false, "box", 81), "box:81"),
        (endpoint(true, "box", 444), "box:444"),
        (endpoint(false, "::1", 80), "[::1]"),
        (endpoint(true, "::1", 8443), "[::1]:8443"),
    ] {
        assert!(request(&ep, KEY).contains(&format!("Host: {host}\r\n")));
    }
    let a = key().unwrap();
    let b = key().unwrap();
    assert_eq!(STANDARD.decode(&a).unwrap().len(), 16);
    assert_ne!(a, b);
}

#[test]
fn refuses_status_and_invalid_upgrade_headers() {
    let error = reply(&mut Cursor::new(b"HTTP/1.1 403 Forbidden\r\n\r\n"), KEY).unwrap_err();
    assert!(matches!(&error, Error::Upgrade{status_line} if status_line=="HTTP/1.1 403 Forbidden"));
    for bad in [
        switching().replace(ACCEPT, "wrong"),
        switching().replace("Upgrade: websocket\r\n", ""),
        switching().replace("Connection: Upgrade\r\n", ""),
        switching().replace("Upgrade: websocket", "Upgrade: other"),
        switching().replace("Connection: Upgrade", "Connection: notupgrade"),
        switching().replace(&format!("Sec-WebSocket-Accept: {ACCEPT}\r\n"), ""),
    ] {
        assert!(matches!(
            reply(&mut Cursor::new(bad), KEY),
            Err(Error::Accept)
        ));
    }
    for status in ["HTTP/1.1 1010", "HTTP/1.0 101"] {
        let wire = switching().replace("HTTP/1.1 101", status);
        assert!(matches!(
            reply(&mut Cursor::new(wire), KEY),
            Err(Error::Upgrade { .. })
        ));
    }
    let mut accepted = switching()
        .replace("Connection: Upgrade", "connection: keep-alive, uPgRaDe")
        .replace("Upgrade: websocket", "upgrade: WebSocket");
    assert!(reply(&mut Cursor::new(&mut accepted), KEY).is_ok());
}

#[test]
fn reply_cap_includes_the_blank_line() {
    for len in [8191, 8192, 8193] {
        let base = switching();
        let padding = "x".repeat(len - base.len() - 5);
        let wire = base.replace("\r\n\r\n", &format!("\r\nX: {padding}\r\n\r\n"));
        assert_eq!(wire.len(), len);
        let result = reply(&mut Cursor::new(wire), KEY);
        if len <= 8192 {
            assert!(result.is_ok(), "{len}");
        } else {
            assert!(matches!(result, Err(Error::ReplyTooLong)));
        }
    }
    for len in [8191, 8192] {
        assert!(
            matches!(reply(&mut Cursor::new(vec![b'x'; len]), KEY), Err(Error::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof)
        );
    }
    assert!(matches!(
        reply(&mut Cursor::new(vec![b'x'; 8193]), KEY),
        Err(Error::ReplyTooLong)
    ));
    assert!(
        matches!(reply(&mut Cursor::new(b"HTTP/1.1 101\r\n"),KEY),Err(Error::Io(e)) if e.kind()==io::ErrorKind::UnexpectedEof)
    );
}

#[test]
fn every_websocket_error_has_connection_failed_code() {
    let addr = "127.0.0.1:1".parse().unwrap();
    for error in [
        Error::Resolve {
            host: "box".into(),
            reason: "missing".into(),
        },
        Error::Connect {
            addr,
            source: io::Error::other("refused"),
        },
        Error::ProxyScheme {
            scheme: "socks5".into(),
        },
        Error::ProxyRefused {
            status_line: "407".into(),
        },
        Error::Tls {
            reason: "certificate".into(),
        },
        Error::Upgrade {
            status_line: "403".into(),
        },
        Error::Accept,
        Error::ReplyTooLong,
        Error::Io(io::Error::other("reset")),
    ] {
        assert_eq!(error.code(), contract::ErrorCode::ConnectionFailed);
    }
}
