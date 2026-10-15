use super::{Address, NAME_RULE, check_name};
use crate::Error;
use contract::ErrorCode;

#[test]
fn websocket_addresses_keep_the_endpoint() {
    for (text, secure, host, port, target) in [
        ("ws://box", false, "box", 80, "/"),
        (
            "wss://box.tailnet.ts.net",
            true,
            "box.tailnet.ts.net",
            443,
            "/",
        ),
        ("ws://127.0.0.1:7777/hub", false, "127.0.0.1", 7777, "/hub"),
        ("wss://[::1]:8443", true, "::1", 8443, "/"),
        ("ws://box:1", false, "box", 1, "/"),
        ("ws://box:65535", false, "box", 65535, "/"),
        ("ws://box/hub?x=y", false, "box", 80, "/hub?x=y"),
    ] {
        let Address::Web(endpoint) = Address::parse(text).unwrap() else {
            panic!("expected a websocket: {text}");
        };
        assert_eq!(endpoint.secure, secure, "{text}");
        assert_eq!(endpoint.host, host, "{text}");
        assert_eq!(endpoint.port, port, "{text}");
        assert_eq!(endpoint.target, target, "{text}");
    }
}

#[test]
fn unix_address_is_an_absolute_socket_path() {
    let Address::Unix(path) = Address::parse("unix:/tmp/fiber-hub.sock").unwrap() else {
        panic!("expected a socket");
    };
    assert_eq!(path, std::path::Path::new("/tmp/fiber-hub.sock"));
}

#[test]
fn refused_addresses_name_the_grammar_without_echoing_secrets() {
    for text in [
        "",
        "box",
        "http://box",
        "unix:relative",
        "unix:",
        "ws://",
        "ws:///hub",
        "ws://user:secret@box",
        "ws://box/#fragment",
        "ws://box/a b",
        "ws://box/\n",
        "ws://box:0",
        "ws://box:65536",
        "ws://box:-1",
        "ws://box:abc",
        "ws://box:",
        "ws://::1",
        "ws://[::1",
        "ws://[bad]",
        "ws://[::1]garbage",
        "ws://box?x=y",
    ] {
        let error = Address::parse(text).unwrap_err();
        assert!(matches!(error, Error::BadAddress { .. }), "{text}: {error}");
        assert_eq!(error.code(), ErrorCode::Usage);
        assert!(error.to_string().contains("ws://"));
        assert!(error.to_string().contains("unix:"));
        assert!(!error.to_string().contains("secret"));
    }
}

#[test]
fn hub_names_obey_the_credential_file_rule() {
    for name in [
        "a".into(),
        "a".repeat(63),
        "a".repeat(64),
        "Box-1_x.y".into(),
        "box.locks".into(),
        "box.tmps".into(),
    ] {
        check_name(&name).unwrap();
    }
    for name in [
        "".into(),
        "a".repeat(65),
        ".box".into(),
        "local".into(),
        "a/b".into(),
        "box.lock".into(),
        "box.tmp".into(),
        "é".into(),
        "a b".into(),
    ] {
        let error = check_name(&name).unwrap_err();
        assert!(matches!(error, Error::BadName { .. }));
        assert_eq!(error.code(), ErrorCode::Usage);
        assert_eq!(error.to_string(), NAME_RULE);
    }
}
