use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc;

use super::*;

/// How long a test waits for a spawned program to report. A freshly written
/// script can take several seconds to run the first time on macOS.
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
    let script = dir.path().join("browser.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", fifo.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
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

/// A connection that fails `silent` reads, as one that times out does, then
/// sends `head`.
struct Silent {
    silent: u32,
    head: io::Cursor<Vec<u8>>,
}

impl Read for Silent {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.silent > 0 {
            self.silent -= 1;
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.head.read(buf)
    }
}

fn head_after(silent: u32) -> Head {
    let (_keep, stop) = mpsc::channel();
    let mut stream = Silent {
        silent,
        head: io::Cursor::new(b"GET /?code=ok HTTP/1.1\r\n\r\n".to_vec()),
    };
    read_head(&mut stream, &stop)
}

#[test]
fn a_connection_silent_for_the_bound_is_dropped_and_one_poll_less_is_read() {
    assert!(matches!(head_after(SILENT_POLLS), Head::Gone));
    assert!(matches!(head_after(SILENT_POLLS - 1), Head::Complete(_)));
}
