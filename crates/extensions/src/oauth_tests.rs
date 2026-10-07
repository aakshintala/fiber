use std::net::SocketAddr;
use std::sync::mpsc;

use super::*;

/// How long a test waits for a spawned program to report.
const WAIT: Duration = Duration::from_secs(15);

#[test]
fn the_rfc_7636_appendix_b_verifier_gives_its_published_challenge() {
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn a_generated_verifier_is_43_base64url_characters_and_differs_each_time() {
    let one = verifier().unwrap();
    let two = verifier().unwrap();
    assert_eq!(one.len(), 43);
    assert!(
        one.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    assert_ne!(one, two);
    assert_eq!(challenge(&one).len(), 43);
}

fn pairs(query: &str) -> Vec<(String, String)> {
    parse_query(query).unwrap()
}

fn pair(key: &str, value: &str) -> (String, String) {
    (key.to_owned(), value.to_owned())
}

#[test]
fn a_query_is_percent_decoded_and_a_plus_is_a_space() {
    assert_eq!(
        pairs("code=a%20b&state=s+t&x=%E2%9C%93"),
        [
            pair("code", "a b"),
            pair("state", "s t"),
            pair("x", "\u{2713}")
        ]
    );
}

#[test]
fn an_empty_query_and_empty_pairs_give_nothing() {
    assert!(pairs("").is_empty());
    assert!(pairs("&&").is_empty());
}

#[test]
fn a_key_without_a_value_has_an_empty_one_and_a_repeated_key_keeps_both_in_order() {
    assert_eq!(
        pairs("a&b=1&b=2"),
        [pair("a", ""), pair("b", "1"), pair("b", "2")]
    );
}

#[test]
fn an_invalid_percent_escape_is_an_error() {
    for bad in ["a=%", "a=%4", "a=%zz", "%g1=x", "a=%ff"] {
        assert!(parse_query(bad).is_err(), "{bad}");
    }
}

#[test]
fn a_request_line_gives_its_target_and_anything_else_gives_none() {
    fn target(head: &str) -> Option<&str> {
        request_target(head.as_bytes())
    }
    assert_eq!(
        target("GET /cb?code=1 HTTP/1.1\r\nHost: x\r\n\r\n"),
        Some("/cb?code=1")
    );
    assert_eq!(target("GET / HTTP/1.0\n\n"), Some("/"));
    assert_eq!(target("GET /\r\n\r\n"), None);
    assert_eq!(target("GET / FTP/1.1\r\n\r\n"), None);
    assert_eq!(target("GET / HTTP/1.1 extra\r\n\r\n"), None);
    assert_eq!(target("\r\n\r\n"), None);
}

#[test]
fn the_listener_binds_loopback_only() {
    let listener = bind(0).unwrap();
    assert_eq!(
        listener.local_addr().unwrap(),
        SocketAddr::from((Ipv4Addr::LOCALHOST, listener.local_addr().unwrap().port()))
    );
}

#[test]
fn a_taken_port_is_an_error() {
    let held = bind(0).unwrap();
    let port = held.local_addr().unwrap().port();
    assert!(bind(port).is_err());
}

#[test]
fn the_system_browser_starts_its_program_with_the_url() {
    let dir = fakes::TempDir::new("fiber-browser");
    let fifo = dir.path().join("seen");
    let made = Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(made.success());
    let body = format!("printf '%s' \"$1\" > '{}'\n", fifo.display());
    let script = fakes::script(dir.path(), "browser", &body);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut seen = String::new();
        let read = std::fs::File::open(&fifo).and_then(|mut f| f.read_to_string(&mut seen));
        tx.send(read.map(|_| seen))
    });
    SystemBrowser::with_program(&script).open("https://auth.example/authorize?x=1");
    let seen = rx.recv_timeout(WAIT).unwrap().unwrap();
    assert_eq!(seen, "https://auth.example/authorize?x=1");
}

#[test]
fn a_browser_that_cannot_start_is_not_an_error() {
    SystemBrowser::with_program("/nonexistent/fiber-browser").open("https://auth.example/");
}

#[test]
fn due_judges_a_stored_credential_by_the_clock() {
    let clock = fakes::clock::FakeClock::new();
    let wall = i64::try_from(clock.wall().duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();
    let dir = fakes::TempDir::new("fiber-due");
    let lock = CredentialFile::new(dir.path(), "p", "default")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap();
    let held = Held::new(lock, clock);
    let due = |expires: i64| held.due(&serde_json::json!({ "token": "t", "expires_at": expires }));
    assert!(!due(wall + 301));
    assert!(due(wall + 300));
    assert!(due(wall - 1));
    assert!(held.due(&serde_json::json!({ "token": "", "expires_at": wall + 9999 })));
    assert!(held.due(&serde_json::json!({ "token": "t", "expires_at": 1.5 })));
    assert!(held.due(&serde_json::json!("t")));
}

/// A connection that fails `pause` reads, as one that times out does,
/// before each byte of `head`.
struct Pausing {
    pause: u32,
    waited: u32,
    head: io::Cursor<Vec<u8>>,
}

impl Read for Pausing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.waited < self.pause {
            self.waited += 1;
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.waited = 0;
        // `read_head` reads one byte at a time.
        self.head.read(buf)
    }
}

fn head_pausing(pause: u32) -> Head {
    let (_keep, stop) = mpsc::channel();
    let mut stream = Pausing {
        pause,
        waited: 0,
        head: io::Cursor::new(b"GET /?code=ok HTTP/1.1\r\n\r\n".to_vec()),
    };
    read_head(&mut stream, &stop)
}

#[test]
fn a_connection_silent_for_the_bound_is_dropped_and_pauses_one_poll_shorter_never_are() {
    assert!(matches!(head_pausing(SILENT_POLLS), Head::Gone));
    // A pause before every byte: far more silent polls in all than the bound.
    assert!(matches!(head_pausing(SILENT_POLLS - 1), Head::Complete(_)));
}

/// A connection that never stops sending header bytes.
struct Dripping;

impl Read for Dripping {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        buf.fill(b'a');
        Ok(1)
    }
}

#[test]
fn a_client_that_keeps_sending_is_dropped_once_the_callback_is_cancelled() {
    let (keep, stop) = mpsc::channel::<()>();
    // Kept: still waiting, so the endless head runs to the bound.
    assert!(matches!(read_head(&mut Dripping, &stop), Head::TooLong));
    drop(keep);
    // Cancelled: dropped on the first pass, long before the bound.
    assert!(matches!(read_head(&mut Dripping, &stop), Head::Gone));
}

#[test]
fn a_malformed_parameter_is_named_only_when_recognised_and_its_value_never_shown() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "code=SECRET%zz",
            "the `code` parameter's value",
            "invalid % escape",
        ),
        (
            "code=SECRET%4",
            "the `code` parameter's value",
            "invalid % escape",
        ),
        (
            "code=SECRET%",
            "the `code` parameter's value",
            "invalid % escape",
        ),
        (
            "code=SECRET%ff",
            "the `code` parameter's value",
            "not UTF-8 once decoded",
        ),
        (
            "state=%zz",
            "the `state` parameter's value",
            "invalid % escape",
        ),
        ("SECRETCODE=%zz", "a parameter's value", "invalid % escape"),
        ("%0ASECRET=%zz", "a parameter's value", "invalid % escape"),
        ("SECRET%zz=x", "a parameter name", "invalid % escape"),
        ("SECRET%ff=x", "a parameter name", "not UTF-8 once decoded"),
    ];
    for (query, phrase, kind) in cases {
        let err = parse_query(query).unwrap_err();
        assert!(err.contains(phrase), "{query}: {err}");
        assert!(err.contains(kind), "{query}: {err}");
        assert!(!err.contains("SECRET"), "{query}: {err}");
        assert!(!err.contains('\n'), "{query}: {err}");
    }
}
