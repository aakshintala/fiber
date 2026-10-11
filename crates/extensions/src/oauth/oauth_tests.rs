use std::net::SocketAddr;
use std::sync::{Arc, Mutex, mpsc};
use std::time::UNIX_EPOCH;

use contract::clock::Clock;

use super::*;
use fakes::Deadline;

/// How long a test waits for a spawned program to report.
const WAIT: Duration = Duration::from_secs(15);

#[test]
fn the_rfc_7636_appendix_b_verifier_gives_its_published_challenge() {
    assert_eq!(
        challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
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
fn the_default_callback_listener_binds_loopback_only() {
    let listener = SystemBrowser::default().callback_listener(0).unwrap();
    assert_eq!(
        listener.local_addr().unwrap(),
        SocketAddr::from((Ipv4Addr::LOCALHOST, listener.local_addr().unwrap().port()))
    );
}

#[derive(Clone)]
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CaptureWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn the_system_browser_shows_device_instructions_to_its_sink() {
    let output = Arc::new(Mutex::new(Vec::new()));
    let browser = SystemBrowser::with_writer("unused", CaptureWriter(Arc::clone(&output)));

    browser.show("https://auth.example/device", "ABCD-1234");

    assert_eq!(
        String::from_utf8(output.lock().unwrap().clone()).unwrap(),
        "Go to https://auth.example/device and enter the code ABCD-1234\n"
    );
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
    let seen = Deadline::after(WAIT).recv(&rx).unwrap().unwrap();
    assert_eq!(seen, "https://auth.example/authorize?x=1");
}

#[test]
fn a_browser_that_cannot_start_is_not_an_error() {
    SystemBrowser::with_program("/nonexistent/fiber-browser").open("https://auth.example/");
}

#[test]
fn the_system_browser_has_nobody_attached() {
    assert!(!SystemBrowser::default().attended());
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
    let held = Held::new(Holder::File(lock), clock);
    let due = |expires: i64| held.due(&serde_json::json!({ "token": "t", "expires_at": expires }));
    assert!(!due(wall + 301));
    assert!(due(wall + 300));
    assert!(due(wall - 1));
    assert!(held.due(&serde_json::json!({ "token": "", "expires_at": wall + 9999 })));
    assert!(held.due(&serde_json::json!({ "token": "t", "expires_at": 1.5 })));
    assert!(held.due(&serde_json::json!("t")));
}

#[test]
fn holder_and_held_debug_output_is_exact_and_redacted() {
    let root = fakes::TempDir::new("fiber-held-debug");
    let lock = CredentialFile::new(root.path(), "codex", "default")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap();
    let expected_file = format!("File({lock:?})");
    let file_holder = Holder::File(lock);
    assert_eq!(format!("{file_holder:?}"), expected_file);
    let held_file = Held::new(file_holder, fakes::clock::FakeClock::new());
    assert_eq!(format!("{held_file:?}"), "Held { holder: \"file\", .. }");

    let filled_lock = CredentialFile::new(root.path(), "codex", "filled")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap();
    filled_lock
        .write(&serde_json::json!({
            "token": "file-secret-token",
            "expires_at": 4_102_444_800u64,
        }))
        .unwrap();
    let expected_filled_file = format!("File({filled_lock:?})");
    assert!(!expected_filled_file.contains("file-secret-token"));
    assert_eq!(
        format!("{:?}", Holder::File(filled_lock)),
        expected_filled_file
    );

    let empty: LoginSlot = Arc::new(Mutex::new(None));
    assert_eq!(
        format!("{:?}", Holder::Login(Arc::clone(&empty))),
        "Login(false)"
    );

    let slot: LoginSlot = Arc::new(Mutex::new(Some(serde_json::json!({
        "token": "sk-live-secret-token",
        "refresh_token": "rt-live-secret",
        "expires_at": 4_102_444_800u64,
        "account_id": "acct_1",
    }))));
    let login_holder = Holder::Login(Arc::clone(&slot));
    assert_eq!(format!("{login_holder:?}"), "Login(true)");
    let held_login = Held::new(login_holder, fakes::clock::FakeClock::new());
    assert_eq!(format!("{held_login:?}"), "Held { holder: \"login\", .. }");
    // The slot still holds its value: the redaction is in the printing,
    // not a wipe.
    assert!(slot.lock().unwrap().is_some());
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

/// The reply `work` delivers: every failure carries its code and message.
#[track_caller]
fn delivered(work: impl FnOnce(&Deliver)) -> Reply {
    let (tx, rx) = mpsc::channel();
    let deliver: Deliver = Arc::new(move |reply| match tx.send(reply) {
        Ok(()) | Err(_) => {}
    });
    work(&deliver);
    Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited {WAIT:?} for the reply")
}

fn failed(reply: Reply) -> (contract::ErrorCode, String) {
    match reply {
        Reply::Query(Err(failed)) => failed,
        Reply::Lock(Err(crate::host::LockError::Coded(failed))) => failed,
        Reply::Lock(Err(crate::host::LockError::Arg(message))) => {
            panic!("a string failure was delivered: {message}")
        }
        Reply::Http(_)
        | Reply::Drive(_)
        | Reply::Exec(_)
        | Reply::Query(_)
        | Reply::Lock(_)
        | Reply::Ask(_)
        | Reply::Slept => {
            panic!("no failure was delivered")
        }
    }
}

/// A browser holding a listener it bound at port 0 and never released,
/// handed to `listen` through `callback_listener`: choosing and binding
/// leave no gap for another listener. It records each port `listen` asked
/// it to bind; an empty slot binds `port` itself.
struct HeldBrowser {
    listener: Mutex<Option<TcpListener>>,
    seen: Mutex<Vec<u16>>,
}

impl HeldBrowser {
    fn holding(listener: TcpListener) -> Self {
        Self {
            listener: Mutex::new(Some(listener)),
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl Browser for HeldBrowser {
    fn open(&self, _url: &str) {}

    fn show(&self, _url: &str, _code: &str) {}

    fn attended(&self) -> bool {
        true
    }

    fn callback_listener(&self, port: u16) -> io::Result<TcpListener> {
        self.seen.lock().unwrap().push(port);
        if let Some(listener) = self.listener.lock().unwrap().take() {
            return Ok(listener);
        }
        TcpListener::bind((Ipv4Addr::LOCALHOST, port))
    }
}

#[test]
fn a_released_port_lets_a_foreign_listener_reset_the_redirect() {
    // The bind-and-release pattern this removes: the port number outlives
    // the listener that chose it, so another process can bind it first.
    let released = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // A foreign listener takes the released port before the package binds
    // it. It accepts, waits for the request bytes, then drops the
    // connection with them unread, which resets it: the ticket's
    // `ConnectionReset` in the redirect's `read_to_string`.
    let foreign = TcpListener::bind((Ipv4Addr::LOCALHOST, released))
        .expect("the released port stayed free for the foreign listener");
    let reset = thread::spawn(move || {
        let (stream, _) = foreign.accept().expect("the redirect connects");
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let mut probe = [0_u8; 1];
        // Bounded: the redirect writes its request next, so its bytes
        // arrive; anything else fails the test instead of hanging it.
        for _ in 0..500 {
            if stream.peek(&mut probe).is_ok() {
                break;
            }
        }
        drop(stream);
    });
    let mut stream = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, released)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    std::io::Write::write_all(
        &mut stream,
        b"GET /cb?code=1 HTTP/1.1\r\nHost: x\r\n\r\n",
    )
    .unwrap();
    let mut reply = String::new();
    let err = stream.read_to_string(&mut reply).unwrap_err();
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::ConnectionReset,
        "a reset foreign listener fails the redirect"
    );
    reset.join().unwrap();
}

#[test]
fn a_held_listener_serves_the_redirect_with_no_gap_for_a_foreign_listener() {
    // Bound at port 0 and never released, so choosing the port and binding
    // it leave no gap: no other process can take the port in between.
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    assert!(
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err(),
        "no foreign listener takes the held port"
    );
    let browser = HeldBrowser::holding(held);
    let (tx, rx) = mpsc::channel();
    let deliver: Deliver = Arc::new(move |reply| match tx.send(reply) {
        Ok(()) | Err(_) => {}
    });
    let Ok(cancel) = listen(port, None, &deliver, &browser) else {
        panic!("the callback serves the redirect on the held port");
    };
    let mut stream = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    std::io::Write::write_all(
        &mut stream,
        b"GET /?code=abc&state=s HTTP/1.1\r\nHost: x\r\n\r\n",
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    let Reply::Query(Ok(pairs)) = Deadline::after(WAIT).recv(&rx).expect("the reply arrives")
    else {
        panic!("the callback delivered its query");
    };
    assert_eq!(
        pairs,
        [
            ("code".to_owned(), "abc".to_owned()),
            ("state".to_owned(), "s".to_owned())
        ]
    );
    drop(cancel);
}

#[test]
fn listen_binds_through_the_browser_callback_listener() {
    // The port stays held, so binding it directly would fail: serving the
    // request proves `listen` took the browser's listener, and the record
    // proves it asked for the port it was given.
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let browser = HeldBrowser::holding(held);
    let (tx, rx) = mpsc::channel();
    let deliver: Deliver = Arc::new(move |reply| match tx.send(reply) {
        Ok(()) | Err(_) => {}
    });
    let Ok(cancel) = listen(port, None, &deliver, &browser) else {
        panic!("the handover binds");
    };
    assert_eq!(browser.seen.lock().unwrap().as_slice(), [port]);
    let mut stream = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    std::io::Write::write_all(
        &mut stream,
        b"GET /?code=abc HTTP/1.1\r\nHost: x\r\n\r\n",
    )
    .unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    let Reply::Query(Ok(pairs)) = Deadline::after(WAIT).recv(&rx).expect("the reply arrives")
    else {
        panic!("the callback delivered its query");
    };
    assert_eq!(pairs, [("code".to_owned(), "abc".to_owned())]);
    drop(cancel);
}

#[test]
fn listen_through_an_empty_handover_binds_the_port_itself() {
    let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = taken.local_addr().unwrap().port();
    let browser = HeldBrowser {
        listener: Mutex::new(None),
        seen: Mutex::new(Vec::new()),
    };
    let (tx, rx) = mpsc::channel();
    let deliver: Deliver = Arc::new(move |reply| match tx.send(reply) {
        Ok(()) | Err(_) => {}
    });
    let Err(Reply::Query(Err((code, message)))) = listen(port, None, &deliver, &browser) else {
        panic!("binding a taken port fails");
    };
    assert_eq!(code, contract::ErrorCode::IoFailed);
    assert!(message.contains(&format!("port {port}")), "{message}");
    assert_eq!(browser.seen.lock().unwrap().as_slice(), [port]);
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "nothing was delivered"
    );
    drop(taken);
}

#[test]
fn a_request_whose_query_cannot_be_read_is_unreadable_reply() {
    // Held, never released: the package binds through the browser's
    // handover, so no gap remains between choosing and binding the port.
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let browser = HeldBrowser::holding(held);
    let (tx, rx) = mpsc::channel();
    let deliver: Deliver = Arc::new(move |reply| match tx.send(reply) {
        Ok(()) | Err(_) => {}
    });
    let Ok(cancel) = listen(port, None, &deliver, &browser) else {
        panic!("the listener binds");
    };
    let mut stream = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    std::io::Write::write_all(
        &mut stream,
        b"GET /cb?code=SECRET%zz HTTP/1.1\r\nHost: x\r\n\r\n",
    )
    .unwrap();
    let (code, message) = failed(Deadline::after(WAIT).recv(&rx).expect("the reply arrives"));
    assert_eq!(code, contract::ErrorCode::UnreadableReply);
    assert!(message.starts_with("host.oauth.callback: "), "{message}");
    assert!(!message.contains("SECRET"), "{message}");
    drop(cancel);
}

#[test]
fn a_lock_whose_file_cannot_be_written_is_io_failed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fakes::TempDir::new("fiber-oauth-lock");
    let credentials = dir.path().join("credentials");
    std::fs::create_dir(&credentials).unwrap();
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o555)).unwrap();
    let pair = crate::CredentialPair {
        credential: "acme".to_owned(),
        label: "default".to_owned(),
    };
    let (code, message) = failed(delivered(|deliver| {
        // The directories check passes; the spawned wait fails writing
        // the lock file and delivers the failure.
        let Ok(_waiting) = lock(dir.path(), &pair, deliver) else {
            panic!("the wait started");
        };
    }));
    assert_eq!(code, contract::ErrorCode::IoFailed);
    assert!(message.starts_with("host.oauth.refresh: "), "{message}");
    std::fs::set_permissions(&credentials, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn a_lock_for_an_unusable_pair_returns_its_failure_without_delivering() {
    let dir = fakes::TempDir::new("fiber-oauth-lock-reserved");
    let pair = crate::CredentialPair {
        credential: "acme".to_owned(),
        label: "default.lock".to_owned(),
    };
    let (tx, rx) = mpsc::channel();
    let deliver: Deliver = Arc::new(move |reply| match tx.send(reply) {
        Ok(()) | Err(_) => {}
    });
    let Err(reply) = lock(dir.path(), &pair, &deliver) else {
        panic!("a reserved label started no wait");
    };
    let (code, message) = failed(reply);
    assert_eq!(code, contract::ErrorCode::IoFailed);
    assert!(message.starts_with("host.oauth.refresh: "), "{message}");
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "nothing was delivered"
    );
}

/// Calls each `held:` method in Lua and converts the caught failure to its
/// code and message.

#[test]
fn a_credential_file_that_cannot_be_read_or_written_is_io_failed() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fakes::TempDir::new("fiber-oauth-held");
    let file = CredentialFile::new(dir.path(), "acme", "default").unwrap();
    {
        let lock = file.try_lock().unwrap().unwrap();
        lock.write(&serde_json::json!({ "token": "old", "expires_at": 1 }))
            .unwrap();
    }
    let cred_dir = dir.path().join("credentials").join("acme");
    let path = cred_dir.join("default");
    // Each phase runs in its own VM: dropping it frees the held lock.
    // The methods return `(nil, code, message)` on a coded failure, which
    // the refresh half raises as the table; assert the triple and the
    // table it builds.
    let failed = |held: Held, method: &str| {
        let lua = Lua::new();
        let lib = crate::host::failure::install(&lua).unwrap();
        lua.globals().set("held", held).unwrap();
        let values: mlua::MultiValue = lua.load(format!("return {method}")).eval().unwrap();
        let mut values = values.into_vec();
        assert_eq!(values.len(), 3, "{method} returned no failure triple");
        let message = values.pop().unwrap();
        let code = values.pop().unwrap();
        assert_eq!(values.pop().unwrap(), mlua::Value::Nil);
        let (code, message) = match (code, message) {
            (mlua::Value::String(code), mlua::Value::String(message)) => (
                code.to_str().unwrap().to_owned(),
                message.to_str().unwrap().to_owned(),
            ),
            _ => panic!("{method} raised no failure triple"),
        };
        let table: mlua::Table = lib.failure.call((code.clone(), message.clone())).unwrap();
        let (tcode, tmessage): (String, String) =
            (table.get("code").unwrap(), table.get("message").unwrap());
        assert_eq!((tcode, tmessage), (code.clone(), message.clone()));
        (code, message)
    };
    // An unreadable file fails the read.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let held = Held::new(Holder::File(file.try_lock().unwrap().unwrap()), clock);
    let (code, message) = failed(held, "held:read()");
    assert_eq!(code, "io_failed");
    assert!(message.contains("default"), "{message}");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    // A read-only directory fails the atomic write's rename. The value is
    // one `write` would otherwise accept: an expired one is refused before
    // any write is attempted (`docs/extensions.md`, "Host calls").
    let clock = fakes::clock::FakeClock::new();
    let held = Held::new(Holder::File(file.try_lock().unwrap().unwrap()), clock);
    std::fs::set_permissions(&cred_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (code, message) = failed(held, "held:write({ token = 't', expires_at = 1700003600 })");
    assert_eq!(code, "io_failed");
    assert!(message.contains("default"), "{message}");
    std::fs::set_permissions(&cred_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// A credential file that is not JSON, or a path that is a symlink, fails the
/// held read as `io_failed`, the code the file layer's
/// `config_invalid` must not replace on this path.
#[test]
fn a_malformed_or_symlinked_credential_is_io_failed_through_held() {
    let triple = |held: Held, method: &str| {
        let lua = Lua::new();
        crate::host::failure::install(&lua).unwrap();
        lua.globals().set("held", held).unwrap();
        let values: mlua::MultiValue = lua.load(format!("return {method}")).eval().unwrap();
        let values = values.into_vec();
        assert_eq!(values.len(), 3, "{method} returned no failure triple");
        let mlua::Value::String(code) = &values[1] else {
            panic!("{method} raised no code");
        };
        code.to_str().unwrap().to_owned()
    };
    let dir = fakes::TempDir::new("fiber-oauth-held-shape");
    let file = CredentialFile::new(dir.path(), "acme", "default").unwrap();
    {
        let lock = file.try_lock().unwrap().unwrap();
        lock.write(&serde_json::json!({ "token": "old", "expires_at": 1 }))
            .unwrap();
    }
    let path = dir.path().join("credentials").join("acme").join("default");
    std::fs::write(&path, b"not json").unwrap();
    let clock = fakes::clock::FakeClock::new();
    let held = Held::new(Holder::File(file.try_lock().unwrap().unwrap()), clock);
    assert_eq!(triple(held, "held:read()"), "io_failed");
    std::fs::remove_file(&path).unwrap();
    let elsewhere = dir.path().join("elsewhere");
    std::fs::write(&elsewhere, b"{}").unwrap();
    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
    let clock = fakes::clock::FakeClock::new();
    let held = Held::new(Holder::File(file.try_lock().unwrap().unwrap()), clock);
    assert_eq!(triple(held, "held:read()"), "io_failed");
}

fn unattended_oauth_lua() -> Lua {
    let lua = Lua::new();
    let failures = crate::host::failure::install(&lua).unwrap();
    let host = lua.create_table().unwrap();
    let tag = lua.create_table().unwrap();
    install(
        &lua,
        &host,
        &tag,
        super::Context {
            browser: Arc::new(SystemBrowser::default()),
            script: None,
        },
        Rc::new(Cell::new(false)),
        failures.failure,
        failures.note_failure,
    )
    .unwrap();
    lua.globals().set("host", host).unwrap();
    lua.globals().set("tag", tag).unwrap();
    lua
}

#[test]
fn unattended_oauth_calls_raise_tables_to_coroutine_resume() {
    let lua = unattended_oauth_lua();
    for (call, message) in [
        (
            "host.oauth.open('https://example.test')",
            "host.oauth.open needs a person to log in, and nobody is attached",
        ),
        (
            "host.oauth.callback({ port = 1 })",
            "host.oauth.callback needs a person to log in, and nobody is attached",
        ),
        (
            "host.oauth.poll({ url = 'https://example.test' })",
            "host.oauth.poll needs a person to log in, and nobody is attached",
        ),
    ] {
        let (ok, err): (bool, mlua::Value) = lua
            .load(format!(
                "return coroutine.resume(coroutine.create(function() {call} end))"
            ))
            .eval()
            .unwrap();
        assert!(!ok, "{call} unexpectedly succeeded");
        let mlua::Value::Table(failed) = err else {
            panic!("{call} raised no failure table");
        };
        assert_eq!(
            failed.get::<String>("code").unwrap(),
            "authentication_failed"
        );
        assert_eq!(failed.get::<String>("message").unwrap(), message);
    }
}

#[test]
fn refresh_passes_the_original_failure_table_through_unchanged() {
    let lua = unattended_oauth_lua();
    let dir = fakes::TempDir::new("fiber-oauth-table-identity");
    let lock = CredentialFile::new(dir.path(), "acme", "default")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap();
    lua.globals()
        .set(
            "held",
            Held::new(Holder::File(lock), fakes::clock::FakeClock::new()),
        )
        .unwrap();
    let same: bool = lua
        .load(
            "local original = { code = 'connection_failed', message = 'offline', extra = 'kept' }
             local co = coroutine.create(function()
               return pcall(function()
                 return host.oauth.refresh(function() error(original, 0) end)
               end)
             end)
             local started, yielded, kind = coroutine.resume(co)
             if not started or yielded ~= tag or kind ~= 'lock' then return false end
             local resumed, ok, caught = coroutine.resume(co, held)
             return resumed and not ok and rawequal(caught, original)
               and caught.code == 'connection_failed' and caught.message == 'offline'
               and caught.extra == 'kept'",
        )
        .eval()
        .unwrap();
    assert!(same, "refresh replaced or changed the user's failure table");
}

#[test]
fn a_refresh_function_raising_a_string_reaches_coroutine_resume_as_credential_failed() {
    let lua = unattended_oauth_lua();
    let dir = fakes::TempDir::new("fiber-oauth-string-refresh");
    let lock = CredentialFile::new(dir.path(), "acme", "default")
        .unwrap()
        .try_lock()
        .unwrap()
        .unwrap();
    lua.globals()
        .set(
            "held",
            Held::new(Holder::File(lock), fakes::clock::FakeClock::new()),
        )
        .unwrap();
    let (code, message): (String, String) = lua
        .load(
            "local co = coroutine.create(function()
               return host.oauth.refresh(function() error('boom', 0) end)
             end)
             local started, yielded, kind = coroutine.resume(co)
             assert(started and yielded == tag and kind == 'lock')
             local resumed, caught = coroutine.resume(co, held)
             assert(not resumed and type(caught) == 'table')
             return caught.code, caught.message",
        )
        .eval()
        .unwrap();
    assert_eq!(
        (code.as_str(), message.as_str()),
        ("credential_failed", "boom")
    );
}

#[test]
fn a_held_write_with_no_usable_credential_is_a_string() {
    let dir = fakes::TempDir::new("fiber-oauth-held-string");
    let file = CredentialFile::new(dir.path(), "acme", "default").unwrap();
    let lock = file.try_lock().unwrap().unwrap();
    let held = Held::new(Holder::File(lock), fakes::clock::FakeClock::new());
    let lua = Lua::new();
    lua.globals().set("held", held).unwrap();
    // The raw method returns `(nil, message)`, which the refresh half raises
    // as the string: an error in the calling code (ruling 17).
    let values: mlua::MultiValue = lua.load("return held:write({})").eval().unwrap();
    let values = values.into_vec();
    assert_eq!(values.len(), 2, "a string failure returns one message");
    assert_eq!(values[0], mlua::Value::Nil);
    let mlua::Value::String(message) = &values[1] else {
        panic!("held:write raised no string failure");
    };
    assert!(
        message.to_str().unwrap().contains("`token` string"),
        "{message:?}"
    );
}

#[test]
fn a_held_read_after_release_is_a_string() {
    let dir = fakes::TempDir::new("fiber-oauth-held-released");
    let file = CredentialFile::new(dir.path(), "acme", "default").unwrap();
    let lock = file.try_lock().unwrap().unwrap();
    let held = Held::new(Holder::File(lock), fakes::clock::FakeClock::new());
    let lua = Lua::new();
    lua.globals().set("held", held).unwrap();
    lua.load("held:release()").exec().unwrap();
    let values: mlua::MultiValue = lua.load("return held:read()").eval().unwrap();
    let values = values.into_vec();
    assert_eq!(values.len(), 2, "a string failure returns one message");
    assert_eq!(values[0], mlua::Value::Nil);
    assert!(matches!(values[1], mlua::Value::String(_)));
}

#[test]
fn a_login_hold_reads_nil_and_keeps_writes_in_its_slot() {
    let slot: LoginSlot = Arc::new(Mutex::new(None));
    let clock = fakes::clock::FakeClock::new();
    let wall = i64::try_from(clock.wall().duration_since(UNIX_EPOCH).unwrap().as_secs()).unwrap();
    let held = Held::new(Holder::Login(Arc::clone(&slot)), clock);
    assert!(held.login());

    let lua = Lua::new();
    lua.globals().set("held", held).unwrap();
    // A login holds no file: its function logs in when it sees nil.
    let read: mlua::Value = lua.load("return held:read()").eval().unwrap();
    assert_eq!(read, mlua::Value::Nil);
    // `login()` on the Rust side agrees.
    let login: bool = lua.load("return held:login()").eval().unwrap();
    assert!(login);

    let write = |method: &str| {
        let values: mlua::MultiValue = lua.load(format!("return {method}")).eval().unwrap();
        assert!(values.into_vec().is_empty(), "{method} returned values");
    };
    write(&format!(
        "held:write({{ token = 't', expires_at = {} }})",
        wall + 3600
    ));
    assert_eq!(
        slot.lock()
            .unwrap()
            .as_ref()
            .and_then(|value| value.get("token"))
            .and_then(serde_json::Value::as_str),
        Some("t")
    );
    // A second write replaces the first.
    write(&format!(
        "held:write({{ token = 'u', expires_at = {} }})",
        wall + 7200
    ));
    assert_eq!(
        slot.lock()
            .unwrap()
            .as_ref()
            .and_then(|value| value.get("token"))
            .and_then(serde_json::Value::as_str),
        Some("u")
    );
    // An unusable value is refused with the existing message, and an
    // expired one with the expired code and message; neither touches the
    // slot.
    let returned = |method: &str| {
        let values: mlua::MultiValue = lua.load(format!("return {method}")).eval().unwrap();
        values.into_vec()
    };
    // An empty token and a missing expiry both fail the usable check.
    let unusable = returned("held:write({ token = '', expires_at = 1700003600 })");
    let [mlua::Value::Nil, mlua::Value::String(message)] = unusable.as_slice() else {
        panic!("an unusable write returned no string failure: {unusable:?}");
    };
    let message = message.to_str().unwrap().to_owned();
    assert!(message.contains("`token` string"), "{message}");
    let missing = returned("held:write({ token = 't' })");
    let [mlua::Value::Nil, mlua::Value::String(missing)] = missing.as_slice() else {
        panic!("a missing expiry returned no string failure: {missing:?}");
    };
    assert!(
        missing.to_str().unwrap().contains("`token` string"),
        "{missing:?}"
    );
    let expired = returned(&format!(
        "held:write({{ token = 't', expires_at = {wall} }})"
    ));
    let [
        mlua::Value::Nil,
        mlua::Value::String(code),
        mlua::Value::String(message),
    ] = expired.as_slice()
    else {
        panic!("an expired write returned no failure triple: {expired:?}");
    };
    assert_eq!(code.to_str().unwrap(), "authentication_failed");
    let message = message.to_str().unwrap().to_owned();
    assert!(message.contains("has already expired"), "{message}");
    assert_eq!(
        slot.lock()
            .unwrap()
            .as_ref()
            .and_then(|value| value.get("token"))
            .and_then(serde_json::Value::as_str),
        Some("u")
    );
    // Releasing the slot keeps what it holds.
    let () = lua.load("held:release()").exec().unwrap();
    assert!(
        slot.lock().unwrap().is_some(),
        "release dropped the login's result"
    );
}
