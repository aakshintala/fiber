//! `host.oauth` through the public API (`docs/extensions.md`, "Host calls"):
//! a fixture extension written into a temporary directory, real localhost
//! sockets, the OAuth fake and a fake clock.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]
#![allow(
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{Setup, write};
use config::{CredentialFile, Secret, store_secret};
use contract::clock::Clock;
use extensions::{Browser, Error, LuaExtension, LuaProvider};
use fakes::OauthReply;
use fakes::OauthServer;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

/// How long a test waits for one call, a socket or a server.
const WAIT: Duration = Duration::from_secs(15);

/// The callbacks' declared timeout, in fake-clock time.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How long past its deadline the caller waits for a callback the hook has not
/// stopped (`docs/extensions.md`, "How an extension runs").
const GRACE: Duration = Duration::from_secs(1);

/// How long a freed port may take to come back: the listener looks at its
/// cancel receiver every 20 ms.
const PORT_FREE_WITHIN: Duration = Duration::from_secs(2);

/// Fake-clock wall time at construction, in Unix seconds.
const WALL: u64 = 1_700_000_000;

const INIT: &str = r#"
local function opts_from(text) return load("return " .. text)() end

fiber.command("callback", { timeout = 60000, run = function(text)
  return json.encode(host.oauth.callback({ port = tonumber(text) }))
end })

fiber.command("callback_opts", { timeout = 60000, run = function(text)
  return json.encode(host.oauth.callback(opts_from(text)))
end })

fiber.command("open", { timeout = 60000, run = function(url)
  host.oauth.open(url)
  return "opened"
end })

fiber.command("pkce", { timeout = 60000, run = function()
  return json.encode(host.oauth.pkce())
end })

fiber.command("poll", { timeout = 60000, run = function(base)
  return json.encode(host.oauth.poll({
    url = base .. "/device/token",
    body = "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code=d",
    headers = { ["content-type"] = "application/x-www-form-urlencoded" },
  }))
end })

fiber.command("refresh", { timeout = 60000, run = function()
  return json.encode(host.oauth.refresh(function() return { token = "t", expires_at = 1 } end))
end })

-- The refresh function does what the secret `mode` says.
fiber.provider("acme", { credential = { timeout = 60000, run = function()
  return host.oauth.refresh(function(stored)
    local mode = host.secret("mode")
    if mode == "raise" then error("boom") end
    if mode == "notoken" then return { expires_at = 1700003600 } end
    if mode == "numbertoken" then return { token = 5, expires_at = 1700003600 } end
    if mode == "floatexpiry" then return { token = "t", expires_at = 1.5 } end
    if mode == "wantnil" and stored ~= nil then error("stored was not nil") end
    if mode == "wantold" and stored.token ~= "old" then error("stored was not the old credential") end
    local reply = host.http({
      url = host.secret("url") .. "/token",
      method = "POST",
      body = "grant_type=refresh_token&refresh_token=r",
      headers = { ["content-type"] = "application/x-www-form-urlencoded" },
    })
    if reply.status ~= 200 then error("refresh failed: " .. reply.status) end
    return { token = json.decode(reply.body).access_token, expires_at = 1700003600, vendor = "kept" }
  end)
end } })
"#;

/// A provider whose refresh function spins when the secret `mode` says so,
/// after it signals through `go_spin`.
const SPIN: &str = r#"
fiber.provider("acme", { credential = { timeout = 60000, run = function()
  return host.oauth.refresh(function()
    if host.secret("mode") == "spin" then
      require("go_spin")
      while true do end
    end
    return { token = "after", expires_at = 1700003600 }
  end)
end } })
"#;

/// `require("go_spin")` signals that the code reached it, then reads an empty
/// module, so the code runs on (a fifo opened for write returns once the
/// loader has opened it for read).
fn go_spin(dir: &Path) -> mpsc::Receiver<()> {
    let path = dir.join("go_spin.lua");
    let made = std::process::Command::new("mkfifo").arg(&path).status();
    assert!(made.unwrap().success(), "mkfifo {path:?}");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let held = fs::OpenOptions::new().write(true).open(&path).unwrap();
        tx.send(()).unwrap();
        drop(held);
    });
    rx
}

struct Env {
    setup: Setup,
    clock: Arc<FakeClock>,
}

impl Env {
    fn new() -> Self {
        Self {
            setup: Setup::new(),
            clock: FakeClock::new(),
        }
    }

    fn home(&self) -> PathBuf {
        self.setup.home()
    }

    /// An extension of the fixture's `init.lua`, in this home, on this clock.
    fn extension(&self) -> Arc<LuaExtension> {
        self.extension_with(INIT, "fixture")
    }

    fn extension_with(&self, init: &str, name: &str) -> Arc<LuaExtension> {
        Arc::new(self.bare(init, name))
    }

    fn bare(&self, init: &str, name: &str) -> LuaExtension {
        let dir = self.setup.root().join("extensions").join(name);
        write(&dir.join("init.lua"), init);
        LuaExtension::new(name, dir, self.home(), self.clock.clone())
    }

    fn secret(&self, name: &str, value: &str) {
        store_secret(&self.home(), name, &Secret::new(value.to_owned())).unwrap();
    }

    /// The provider `acme` of `ext`, refreshing against `server`.
    fn provider(
        &self,
        ext: &Arc<LuaExtension>,
        server: &OauthServer,
        mode: &str,
    ) -> Arc<LuaProvider> {
        self.secret("url", &server.url());
        self.secret("mode", mode);
        LuaProvider::new(Arc::clone(ext), "acme")
    }

    fn credential(&self) -> PathBuf {
        self.home().join("credentials/acme/default")
    }

    fn stored(&self) -> Option<Vec<u8>> {
        fs::read(self.credential()).ok()
    }

    fn store(&self, value: &Value) {
        let lock = CredentialFile::new(&self.home(), "acme", "default")
            .unwrap()
            .try_lock()
            .unwrap()
            .unwrap();
        lock.write(value).unwrap();
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Starts `command` on its own thread; the result arrives on the receiver.
fn start(
    ext: &Arc<LuaExtension>,
    command: &'static str,
    text: &str,
) -> mpsc::Receiver<Result<String, Error>> {
    let (tx, rx) = mpsc::channel();
    let (ext, text) = (Arc::clone(ext), text.to_owned());
    std::thread::spawn(move || tx.send(ext.command(command, &text)));
    rx
}

fn finish<T>(rx: &mpsc::Receiver<T>) -> T {
    rx.recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("the call did not return within {WAIT:?}"))
}

fn run(ext: &Arc<LuaExtension>, command: &'static str, text: &str) -> Result<String, Error> {
    finish(&start(ext, command, text))
}

/// Starts `provider.token()`; the token's text arrives on the receiver.
fn start_token(provider: &Arc<LuaProvider>) -> mpsc::Receiver<Result<String, Error>> {
    let (tx, rx) = mpsc::channel();
    let provider = Arc::clone(provider);
    std::thread::spawn(move || tx.send(provider.token().map(|secret| secret.expose().to_owned())));
    rx
}

fn lua_message(error: &Error) -> String {
    if let Error::Credential(inner) = error {
        return lua_message(inner);
    }
    let Error::Lua { message, .. } = error else {
        panic!("not a Lua error: {error}");
    };
    message.clone()
}

/// A port nothing listens on.
fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Sends `request` to `port` and reads the reply to its end.
fn get(port: u16, request: &str) -> String {
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).unwrap();
    reply
}

/// Starts the `callback` command on `port` and waits until its listener is
/// bound: the extension's thread is parked at the callback's deadline.
fn listening(
    env: &Env,
    ext: &Arc<LuaExtension>,
    port: u16,
) -> mpsc::Receiver<Result<String, Error>> {
    let deadline = env.clock.now() + TIMEOUT;
    let rx = start(ext, "callback", &port.to_string());
    assert!(
        env.clock.await_parked(deadline, WAIT),
        "the callback never parked"
    );
    rx
}

// ---------------------------------------------------------------- open, pkce

#[derive(Default)]
struct Recording(Mutex<Vec<String>>);

impl Browser for Recording {
    fn open(&self, url: &str) {
        self.0.lock().unwrap().push(url.to_owned());
    }
}

#[test]
fn open_reaches_the_browser_with_the_url() {
    let env = Env::new();
    let browser = Arc::new(Recording::default());
    let ext = Arc::new(env.bare(INIT, "fixture").with_browser(browser.clone()));
    assert_eq!(
        run(&ext, "open", "https://auth.example/authorize?x=1").unwrap(),
        "opened"
    );
    assert_eq!(
        *browser.0.lock().unwrap(),
        ["https://auth.example/authorize?x=1"]
    );
}

#[test]
fn pkce_returns_a_verifier_and_its_challenge() {
    let env = Env::new();
    let ext = env.extension();
    let pair: Value = serde_json::from_str(&run(&ext, "pkce", "").unwrap()).unwrap();
    let verifier = pair["verifier"].as_str().unwrap();
    let challenge = pair["challenge"].as_str().unwrap();
    assert_eq!(verifier.len(), 43);
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    assert_eq!(challenge, URL_SAFE_NO_PAD.encode(digest));
}

// ------------------------------------------------------------------ callback

#[test]
fn callback_serves_one_request_and_returns_its_query() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    let reply = get(
        port,
        "GET /?code=a%20b&state=s HTTP/1.1\r\nHost: localhost\r\n\r\n",
    );
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    assert!(reply.contains("<p>You can close this tab"), "{reply}");
    let query: Value = serde_json::from_str(&finish(&rx).unwrap()).unwrap();
    assert_eq!(query, json!({ "code": "a b", "state": "s" }));
}

#[test]
fn callback_with_no_query_returns_an_empty_table() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    get(port, "GET /cb? HTTP/1.1\r\n\r\n");
    assert_eq!(finish(&rx).unwrap(), "{}");
}

#[test]
fn callback_with_a_repeated_key_keeps_the_last_value() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    get(port, "GET /?a=1&a=2 HTTP/1.1\r\n\r\n");
    let query: Value = serde_json::from_str(&finish(&rx).unwrap()).unwrap();
    assert_eq!(query, json!({ "a": "2" }));
}

#[test]
fn callback_with_an_invalid_escape_is_a_lua_error_and_the_client_gets_a_400() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    let reply = get(port, "GET /?code=%zz HTTP/1.1\r\n\r\n");
    assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
    assert!(lua_message(&finish(&rx).unwrap_err()).contains("invalid % escape"));
}

#[test]
fn callback_skips_a_connection_that_is_not_a_request_and_a_head_that_is_too_large() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    let junk = get(port, "not http\r\n\r\n");
    assert!(junk.starts_with("HTTP/1.1 400"), "{junk}");
    let huge = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(20 * 1024));
    let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    // The listener answers once the head passes its bound and closes; the
    // rest of the head may meet a closed socket.
    match stream.write_all(huge.as_bytes()) {
        Ok(()) | Err(_) => {}
    }
    let mut reply = String::new();
    match stream.read_to_string(&mut reply) {
        Ok(_) | Err(_) => {}
    }
    assert!(
        reply.is_empty() || reply.starts_with("HTTP/1.1 431"),
        "{reply}"
    );
    get(port, "GET /?code=ok HTTP/1.1\r\n\r\n");
    let query: Value = serde_json::from_str(&finish(&rx).unwrap()).unwrap();
    assert_eq!(query, json!({ "code": "ok" }));
}

#[test]
fn callback_on_a_taken_port_is_an_error_naming_it() {
    let env = Env::new();
    let ext = env.extension();
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let message = lua_message(&run(&ext, "callback", &port.to_string()).unwrap_err());
    assert!(message.contains(&format!("port {port}")), "{message}");
}

#[test]
fn callback_dropped_at_its_timeout_frees_the_port() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    env.clock.advance(TIMEOUT);
    assert!(matches!(finish(&rx), Err(Error::Timeout { .. })));
    // The listener sees its cancel receiver disconnect within one poll.
    let (_keep, idle) = mpsc::channel::<()>();
    let attempts = PORT_FREE_WITHIN.as_millis() / 10;
    let freed = (0..attempts).any(|_| {
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
            || idle.recv_timeout(Duration::from_millis(10)).is_ok()
    });
    assert!(freed, "the port was still held after {PORT_FREE_WITHIN:?}");
}

#[test]
fn callback_port_must_be_a_whole_number_from_1_to_65535() {
    let env = Env::new();
    let ext = env.extension();
    for opts in [
        "{}",
        "{ port = 0 }",
        "{ port = 65536 }",
        "{ port = \"80\" }",
        "{ port = 1.5 }",
        "5",
    ] {
        let message = lua_message(&run(&ext, "callback_opts", opts).unwrap_err());
        assert!(message.contains("`port` must be"), "{opts}: {message}");
    }
}

// ---------------------------------------------------------------------- poll

/// The instant the extension's thread should park at to wake `after` from
/// the fake clock's origin.
fn wake(env: &Env, after: u64) -> Instant {
    env.clock.origin() + Duration::from_secs(after)
}

fn device_server(script: Vec<OauthReply>) -> OauthServer {
    OauthServer::start(script)
}

#[test]
fn poll_repeats_at_the_interval_until_the_token() {
    let env = Env::new();
    let ext = env.extension();
    let server = device_server(vec![
        OauthReply::pending(),
        OauthReply::pending(),
        OauthReply::token("at", "rt", 3600),
    ]);
    let rx = start(&ext, "poll", &server.url());
    for step in [5, 10] {
        assert!(
            env.clock.await_parked(wake(&env, step), WAIT),
            "no wake at {step}"
        );
        env.clock.advance(Duration::from_secs(5));
    }
    let token: Value = serde_json::from_str(&finish(&rx).unwrap()).unwrap();
    assert_eq!(token["access_token"], "at");
    // Two wakes, so three requests, each to the device endpoint.
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|r| r.path == "/device/token"));
    assert!(
        requests[0]
            .form
            .contains(&("device_code".into(), "d".into()))
    );
}

#[test]
fn poll_slow_down_adds_five_seconds() {
    let env = Env::new();
    let ext = env.extension();
    let server = device_server(vec![
        OauthReply::slow_down(),
        OauthReply::token("at", "rt", 3600),
    ]);
    let rx = start(&ext, "poll", &server.url());
    assert!(env.clock.await_parked(wake(&env, 10), WAIT));
    env.clock.advance(Duration::from_secs(5));
    // Still waiting on the instant 10 s out.
    assert!(env.clock.await_parked(wake(&env, 10), WAIT));
    assert_eq!(server.request_count(), 1);
    env.clock.advance(Duration::from_secs(5));
    let token: Value = serde_json::from_str(&finish(&rx).unwrap()).unwrap();
    assert_eq!(token["access_token"], "at");
    assert_eq!(server.request_count(), 2);
}

#[test]
fn poll_raises_a_terminal_error_or_an_unusable_reply() {
    let env = Env::new();
    let ext = env.extension();
    for (reply, wanted) in [
        (OauthReply::denied(), "access_denied"),
        (OauthReply::expired(), "expired_token"),
        (OauthReply::raw(200, "not json"), "not a JSON object"),
        (OauthReply::raw(500, "{}"), "status 500"),
    ] {
        let server = device_server(vec![reply]);
        let message = lua_message(&run(&ext, "poll", &server.url()).unwrap_err());
        assert!(message.starts_with("host.oauth.poll: "), "{message}");
        assert!(message.contains(wanted), "{wanted}: {message}");
        assert_eq!(server.request_count(), 1);
    }
}

#[test]
fn poll_that_times_out_while_sleeping_is_a_timeout() {
    let env = Env::new();
    let ext = env.extension();
    let server = device_server(vec![OauthReply::pending(), OauthReply::pending()]);
    let rx = start(&ext, "poll", &server.url());
    assert!(env.clock.await_parked(wake(&env, 5), WAIT));
    env.clock.advance(TIMEOUT);
    assert!(matches!(finish(&rx), Err(Error::Timeout { .. })));
    assert_eq!(server.request_count(), 1);
}

// ------------------------------------------------------------------ the entry

#[test]
fn the_waiting_calls_are_lua_errors_in_the_entry_script() {
    for call in [
        "host.oauth.callback({ port = 8080 })",
        "host.oauth.poll({ url = 'http://127.0.0.1:1/' })",
        "host.oauth.refresh(function() end)",
    ] {
        let env = Env::new();
        let ext = env.extension_with(&format!("{call}\n{INIT}"), "entry");
        let name = call.split('(').next().unwrap();
        let message = lua_message(&ext.command("pkce", "").unwrap_err());
        assert!(message.contains(name), "{call}: {message}");
        assert!(message.contains("entry script"), "{call}: {message}");
    }
}

// ------------------------------------------------------------------- refresh

#[test]
fn refresh_of_a_fresh_token_skips_the_function() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "ok");
    env.store(&json!({ "token": "fresh", "expires_at": WALL + 3600 }));
    assert_eq!(finish(&start_token(&provider)).unwrap(), "fresh");
    assert_eq!(server.request_count(), 0);
}

#[test]
fn refresh_inside_the_window_calls_the_function_once_and_stores_its_result() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![OauthReply::token("new", "rt", 3600)]);
    let provider = env.provider(&ext, &server, "wantold");
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    assert_eq!(finish(&start_token(&provider)).unwrap(), "new");
    assert_eq!(server.request_count(), 1);
    let stored: Value = serde_json::from_slice(&env.stored().unwrap()).unwrap();
    assert_eq!(
        stored,
        json!({ "token": "new", "expires_at": WALL + 3600, "vendor": "kept" })
    );
    assert_eq!(mode(&env.credential()), 0o600);
    assert_eq!(mode(env.credential().parent().unwrap()), 0o700);
    assert_eq!(
        mode(&env.home().join("credentials/acme/default.lock")),
        0o600
    );
}

#[test]
fn refresh_of_an_absent_file_calls_the_function_with_nil() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![OauthReply::token("first", "rt", 3600)]);
    let provider = env.provider(&ext, &server, "wantnil");
    assert_eq!(finish(&start_token(&provider)).unwrap(), "first");
    let stored: Value = serde_json::from_slice(&env.stored().unwrap()).unwrap();
    assert_eq!(stored["token"], "first");
    assert_eq!(mode(&env.credential()), 0o600);
}

#[test]
fn a_function_that_raises_leaves_the_file_and_releases_the_lock() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![OauthReply::token("later", "rt", 3600)]);
    let provider = env.provider(&ext, &server, "raise");
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let before = env.stored().unwrap();
    let error = finish(&start_token(&provider)).unwrap_err();
    assert!(matches!(error, Error::Credential(_)));
    assert!(lua_message(&error).contains("boom"));
    assert_eq!(env.stored().unwrap(), before);
    // The lock is free: the next refresh runs.
    env.secret("mode", "wantold");
    assert_eq!(finish(&start_token(&provider)).unwrap(), "later");
}

#[test]
fn a_function_the_hook_stops_leaves_the_file_and_frees_the_lock() {
    let env = Env::new();
    let dir = env.setup.root().join("extensions/spin");
    write(&dir.join("init.lua"), SPIN);
    let went = go_spin(&dir);
    let ext = Arc::new(LuaExtension::new(
        "spin",
        &dir,
        env.home(),
        env.clock.clone(),
    ));
    env.secret("mode", "spin");
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let before = env.stored().unwrap();
    let provider = LuaProvider::new(ext, "acme");
    let asked = env.clock.now();
    let rx = start_token(&provider);
    finish(&went);
    assert!(env.clock.await_parked(asked + TIMEOUT + GRACE, WAIT));
    env.clock.advance(TIMEOUT);
    let error = finish(&rx).unwrap_err();
    assert!(matches!(&error, Error::Credential(inner) if matches!(**inner, Error::Timeout { .. })));
    assert_eq!(env.stored().unwrap(), before);
    env.secret("mode", "ok");
    assert_eq!(finish(&start_token(&provider)).unwrap(), "after");
}

#[test]
fn a_function_that_returns_no_usable_credential_leaves_the_file() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "ok");
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let before = env.stored().unwrap();
    for bad in ["notoken", "numbertoken", "floatexpiry"] {
        env.secret("mode", bad);
        let error = finish(&start_token(&provider)).unwrap_err();
        assert!(
            lua_message(&error).contains("`token` string"),
            "{bad}: {error}"
        );
        assert_eq!(env.stored().unwrap(), before, "{bad}");
    }
}

#[test]
fn two_sessions_refreshing_together_refresh_once() {
    let env = Env::new();
    let server = OauthServer::start(vec![OauthReply::token("shared", "rt", 3600)]);
    // Two extension instances on one home, as two sessions are.
    let (one, two) = (env.extension(), env.extension_with(INIT, "fixture"));
    let (first, second) = (
        env.provider(&one, &server, "ok"),
        LuaProvider::new(Arc::clone(&two), "acme"),
    );
    let (a, b) = (start_token(&first), start_token(&second));
    assert_eq!(finish(&a).unwrap(), "shared");
    assert_eq!(finish(&b).unwrap(), "shared");
    assert_eq!(server.request_count(), 1);
}

#[test]
fn two_callbacks_of_one_extension_refresh_once_without_deadlocking_the_vm() {
    let env = Env::new();
    let server = OauthServer::start(vec![OauthReply::token("shared", "rt", 3600)]);
    let ext = env.extension();
    let first = env.provider(&ext, &server, "ok");
    let second = LuaProvider::new(Arc::clone(&ext), "acme");
    let (a, b) = (start_token(&first), start_token(&second));
    assert_eq!(finish(&a).unwrap(), "shared");
    assert_eq!(finish(&b).unwrap(), "shared");
    assert_eq!(server.request_count(), 1);
}

#[test]
fn a_refresh_that_times_out_in_its_function_leaves_the_file_and_frees_the_lock() {
    let env = Env::new();
    let ext = env.extension();
    // A server that takes the connection and never answers.
    let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let hang = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &hang, "ok");
    env.secret("url", &format!("http://{}", silent.local_addr().unwrap()));
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let before = env.stored().unwrap();
    let deadline = env.clock.now() + TIMEOUT;
    let (accepted_tx, accepted) = mpsc::channel();
    std::thread::spawn(move || {
        let held = silent.accept().unwrap();
        accepted_tx.send(()).unwrap();
        // Hold the connection until the test ends the process.
        std::thread::park();
        drop(held);
    });
    let rx = start_token(&provider);
    // The function is on the wire, so it holds the lock.
    finish(&accepted);
    assert!(env.clock.await_parked(deadline, WAIT));
    env.clock.advance(TIMEOUT);
    let error = finish(&rx).unwrap_err();
    assert!(matches!(&error, Error::Credential(inner) if matches!(**inner, Error::Timeout { .. })));
    assert_eq!(env.stored().unwrap(), before);
    // The next refresh takes the lock.
    let server = OauthServer::start(vec![OauthReply::token("again", "rt", 3600)]);
    env.secret("url", &server.url());
    assert_eq!(finish(&start_token(&provider)).unwrap(), "again");
    assert_eq!(server.request_count(), 1);
}

#[test]
fn refresh_from_a_command_is_a_lua_error() {
    let env = Env::new();
    let ext = env.extension();
    let message = lua_message(&run(&ext, "refresh", "").unwrap_err());
    assert!(message.contains("host.oauth.refresh"), "{message}");
    assert!(env.stored().is_none());
}

#[test]
fn a_symbolic_link_credentials_directory_is_refused() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "ok");
    let elsewhere = env.setup.root().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    // `credentials/` already holds the secrets the fixture reads; replace it.
    let credentials = env.home().join("credentials");
    fs::remove_dir_all(&credentials).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &credentials).unwrap();
    let error = finish(&start_token(&provider)).unwrap_err();
    assert!(matches!(error, Error::Credential(_)), "{error}");
    assert_eq!(server.request_count(), 0);
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
}
