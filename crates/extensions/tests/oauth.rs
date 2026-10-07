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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{Setup, write};
use config::{CredentialFile, Secret, store_secret};
use contract::ErrorCode;
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

-- Like `pcall(host.http, opts)`, which cannot yield across the VM's `pcall`:
-- the call runs in a coroutine and its yield is passed up.
local function try_http(opts)
  local co = coroutine.create(host.http)
  local r = table.pack(coroutine.resume(co, opts))
  local answer = table.pack(coroutine.yield(table.unpack(r, 2, r.n)))
  return coroutine.resume(co, table.unpack(answer, 1, answer.n))
end

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
  if host.secret("mode") == "outside" then error("outside") end
  return host.oauth.refresh(function(stored)
    local mode = host.secret("mode")
    if mode == "raise" then error("boom") end
    if mode == "pcall_dead" then
      try_http({ url = host.secret("dead") .. "/token" })
      error("my own failure")
    end
    if mode == "dead_then_live" then
      try_http({ url = host.secret("dead") .. "/token" })
      host.http({ url = host.secret("url") .. "/token", method = "POST" })
      error("my own failure")
    end
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

/// A provider whose `credential()` builds its login from one interactive
/// `host.oauth` helper, named by the secret `mode`.
const LOGIN: &str = r#"
-- Like `pcall(host.http, opts)`, which cannot yield across the VM's `pcall`:
-- the call runs in a coroutine and its yield is passed up.
local function try_http(opts)
  local co = coroutine.create(host.http)
  local r = table.pack(coroutine.resume(co, opts))
  local answer = table.pack(coroutine.yield(table.unpack(r, 2, r.n)))
  return coroutine.resume(co, table.unpack(answer, 1, answer.n))
end

fiber.provider("acme", { credential = { timeout = 60000, run = function()
  local mode = host.secret("mode")
  local url = host.secret("url")
  if mode == "open" then host.oauth.open(url .. "/verify") end
  if mode == "callback" then host.oauth.callback({ port = tonumber(host.secret("port")) }) end
  if mode == "poll" then
    local reply = host.oauth.poll({
      url = url .. "/device/token",
      body = "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code=d",
      headers = { ["content-type"] = "application/x-www-form-urlencoded" },
    })
    return { token = reply.access_token, expires_at = 1700003600 }
  end
  if mode == "device" then
    host.oauth.open(url .. "/verify")
    local reply = host.oauth.poll({
      url = url .. "/device/token",
      body = "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code=d",
      headers = { ["content-type"] = "application/x-www-form-urlencoded" },
    })
    return { token = reply.access_token, expires_at = 1700003600 }
  end
  if mode == "refresh_poll" then
    return host.oauth.refresh(function(stored)
      try_http({ url = host.secret("dead") .. "/token" })
      local reply = host.oauth.poll({
        url = host.secret("url") .. "/device/token",
        body = "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code&device_code=d",
        headers = { ["content-type"] = "application/x-www-form-urlencoded" },
      })
      return { token = reply.access_token, expires_at = 1700003600 }
    end)
  end
  return { token = "unreached", expires_at = 1700003600 }
end } })
"#;

/// The `LOGIN` fixture with the default browser, which has nobody attached.
fn login(env: &Env) -> Arc<LuaExtension> {
    let dir = env.setup.root().join("extensions").join("login");
    write(&dir.join("init.lua"), LOGIN);
    Arc::new(LuaExtension::new(
        "login",
        dir,
        env.home(),
        env.clock.clone(),
    ))
}

/// The `LOGIN` fixture opening URLs with `browser`.
fn login_with(env: &Env, browser: Arc<dyn Browser>) -> Arc<LuaExtension> {
    let dir = env.setup.root().join("extensions").join("login");
    write(&dir.join("init.lua"), LOGIN);
    Arc::new(LuaExtension::new("login", dir, env.home(), env.clock.clone()).with_browser(browser))
}

/// An entry script that opens a login URL before it registers anything.
const ENTRY_OPEN: &str = r#"
host.oauth.open("https://auth.example/")
fiber.provider("acme", { credential = { timeout = 60000, run = function()
  return { token = "t", expires_at = 1700003600 }
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
            .with_browser(Arc::new(Recording::always()))
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

/// Waits until `callers` callers wait past `deadline` and `threads` extension
/// threads are parked at it. A caller waits at `deadline` while its call is
/// queued, and a grace past it once the call has started, so the first wait
/// proves every call started. A thread wakes to start a call, and parks at
/// `deadline` again only once that callback has made its host request: a
/// listener is bound, a lock wait has begun. One thread parks once for all
/// the callbacks it holds.
fn await_callbacks_parked(env: &Env, deadline: Instant, callers: usize, threads: usize) {
    assert!(
        env.clock
            .await_parked_count(deadline + GRACE, callers, WAIT),
        "the callers never began waiting past their deadline"
    );
    assert!(
        env.clock.await_parked_count(deadline, threads, WAIT),
        "the callbacks never parked"
    );
}

/// Starts the `callback` command on `port` and waits until its listener is
/// bound and the callback is parked on it.
fn listening(
    env: &Env,
    ext: &Arc<LuaExtension>,
    port: u16,
) -> mpsc::Receiver<Result<String, Error>> {
    let deadline = env.clock.now() + TIMEOUT;
    let rx = start(ext, "callback", &port.to_string());
    await_callbacks_parked(env, deadline, 1, 1);
    rx
}

// ---------------------------------------------------------------- open, pkce

#[derive(Default)]
struct Recording {
    opened: Mutex<Vec<String>>,
    attended: AtomicUsize,
}

impl Recording {
    /// A browser with a person attached.
    fn always() -> Self {
        Self::times(usize::MAX)
    }

    /// A browser with nobody attached.
    fn never() -> Self {
        Self::times(0)
    }

    /// A browser with a person attached for the next `n` checks.
    fn times(n: usize) -> Self {
        Self {
            opened: Mutex::new(Vec::new()),
            attended: AtomicUsize::new(n),
        }
    }

    fn opened(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }
}

impl Browser for Recording {
    fn open(&self, url: &str) {
        self.opened.lock().unwrap().push(url.to_owned());
    }

    fn attended(&self) -> bool {
        self.attended
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
    }
}

#[test]
fn open_reaches_the_browser_with_the_url() {
    let env = Env::new();
    let browser = Arc::new(Recording::always());
    let ext = Arc::new(env.bare(INIT, "fixture").with_browser(browser.clone()));
    assert_eq!(
        run(&ext, "open", "https://auth.example/authorize?x=1").unwrap(),
        "opened"
    );
    assert_eq!(browser.opened(), ["https://auth.example/authorize?x=1"]);
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
    let reply = get(port, "GET /?code=SECRETCODE%zz HTTP/1.1\r\n\r\n");
    assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
    let message = lua_message(&finish(&rx).unwrap_err());
    assert!(
        message.contains("the `code` parameter's value"),
        "{message}"
    );
    assert!(!message.contains("SECRETCODE"), "{message}");
}

#[test]
fn callback_skips_a_connection_that_is_not_a_request_and_a_head_that_is_too_large() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    let junk = get(port, "not http\r\n\r\n");
    assert!(junk.starts_with("HTTP/1.1 400"), "{junk}");
    let huge = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(9000));
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
fn callback_serves_the_next_connection_after_dropping_a_silent_one() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    // Queued first, never sends: the listener waits SILENT_POLLS on it, drops
    // it, and goes on to the request queued behind it.
    let silent = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    let reply = get(port, "GET /?code=ok HTTP/1.1\r\n\r\n");
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
    drop(silent);
    let query: Value = serde_json::from_str(&finish(&rx).unwrap()).unwrap();
    assert_eq!(query, json!({ "code": "ok" }));
}

#[test]
fn callback_serves_a_2000_byte_head() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    let request = format!("GET /?code=ok HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(2000));
    let reply = get(port, &request);
    assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
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
fn callback_dropped_at_its_timeout_frees_the_port_from_a_client_that_keeps_sending() {
    let env = Env::new();
    let ext = env.extension();
    let port = free_port();
    let rx = listening(&env, &ext, port);
    // The listener accepts in the order clients connect. `first` is queued
    // before `client`, so once `first` has its answer the listener takes
    // `client` next, with no wait between: from then on it is reading
    // `client`'s head.
    let mut first = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    first.set_read_timeout(Some(WAIT)).unwrap();
    // An incomplete head, a byte every 5 ms: no read ever times out.
    let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    client.write_all(b"GET / HTTP/1.1\r\nX: ").unwrap();
    let sender = std::thread::spawn(move || {
        // Never sent on: the pause between bytes is its timeout.
        let (_keep, pause) = mpsc::channel::<()>();
        for _ in 0..1000 {
            if client.write_all(b"a").is_err() {
                return;
            }
            match pause.recv_timeout(Duration::from_millis(5)) {
                Ok(()) | Err(_) => {}
            }
        }
    });
    first.write_all(b"not http\r\n\r\n").unwrap();
    let mut junk = String::new();
    first.read_to_string(&mut junk).unwrap();
    assert!(junk.starts_with("HTTP/1.1 400"), "{junk}");
    env.clock.advance(TIMEOUT);
    assert!(matches!(finish(&rx), Err(Error::Timeout { .. })));
    let (_keep, idle) = mpsc::channel::<()>();
    let attempts = PORT_FREE_WITHIN.as_millis() / 10;
    let freed = (0..attempts).any(|_| {
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
            || idle.recv_timeout(Duration::from_millis(10)).is_ok()
    });
    assert!(freed, "the port was still held after {PORT_FREE_WITHIN:?}");
    sender.join().unwrap();
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
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
    assert!(error.to_string().contains("boom"), "{error}");
    assert_eq!(env.stored().unwrap(), before);
    // The lock is free: the next refresh runs.
    env.secret("mode", "wantold");
    assert_eq!(finish(&start_token(&provider)).unwrap(), "later");
}

/// The refresh function's secret `dead` is a URL nothing listens on.
fn dead_url() -> String {
    format!("http://127.0.0.1:{}", free_port())
}

#[test]
fn a_refresh_the_token_endpoint_rejects_is_authentication_failed() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![
        OauthReply::raw(400, r#"{"error":"invalid_grant"}"#),
        OauthReply::token("later", "rt", 3600),
    ]);
    let provider = env.provider(&ext, &server, "wantold");
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let before = env.stored().unwrap();
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error}");
    assert!(error.to_string().contains("refresh failed: 400"), "{error}");
    assert_eq!(env.stored().unwrap(), before);
    // The lock is free: the next refresh runs.
    assert_eq!(finish(&start_token(&provider)).unwrap(), "later");
}

#[test]
fn a_refresh_that_never_reached_the_token_endpoint_is_connection_failed() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "ok");
    env.secret("url", &dead_url());
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let before = env.stored().unwrap();
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConnectionFailed, "{error}");
    assert_eq!(env.stored().unwrap(), before);
}

#[test]
fn a_function_that_catches_the_transport_error_and_raises_its_own_is_connection_failed() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "pcall_dead");
    env.secret("dead", &dead_url());
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConnectionFailed, "{error}");
    assert!(error.to_string().contains("my own failure"), "{error}");
}

#[test]
fn a_later_reply_clears_the_transport_failure() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![OauthReply::raw(400, "{}")]);
    let provider = env.provider(&ext, &server, "dead_then_live");
    env.secret("dead", &dead_url());
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error}");
}

#[test]
fn a_lua_error_in_credential_outside_the_refresh_is_credential_failed() {
    let env = Env::new();
    let ext = env.extension();
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "outside");
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::CredentialFailed, "{error}");
    assert!(error.to_string().contains("outside"), "{error}");
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
        assert_eq!(error.code(), ErrorCode::CredentialFailed, "{bad}");
        assert_eq!(env.stored().unwrap(), before, "{bad}");
    }
}

/// A token endpoint that holds its first reply until the test releases it.
/// The script has one token: a second refresh request would get the fake's
/// `script_exhausted`.
fn held_endpoint() -> OauthServer {
    let server = OauthServer::start(vec![OauthReply::token("shared", "rt", 3600)]);
    server.hold();
    server
}

/// Two refreshes race for one credential, on `threads` extension threads.
/// The first is held on the wire with the lock. The second starts once the
/// first has arrived, and is suspended on the lock once both callers wait
/// past the deadline and the threads are parked at it. Only then is the
/// first released: the second must find the first's credential.
fn refresh_race(
    env: &Env,
    first: &Arc<LuaProvider>,
    second: &Arc<LuaProvider>,
    threads: usize,
    held: &OauthServer,
) {
    let deadline = env.clock.now() + TIMEOUT;
    let a = start_token(first);
    assert!(
        held.await_requests(1, WAIT),
        "the first refresh never reached the endpoint"
    );
    let b = start_token(second);
    await_callbacks_parked(env, deadline, 2, threads);
    assert_eq!(held.request_count(), 1);
    held.release();
    assert_eq!(finish(&a).unwrap(), "shared");
    assert_eq!(finish(&b).unwrap(), "shared");
    assert_eq!(held.request_count(), 1);
}

#[test]
fn two_sessions_refreshing_together_refresh_once() {
    let env = Env::new();
    let held = held_endpoint();
    env.secret("url", &held.url());
    env.secret("mode", "ok");
    // Two extension instances on one home, as two sessions are.
    let (one, two) = (env.extension(), env.extension_with(INIT, "fixture"));
    let (first, second) = (
        LuaProvider::new(Arc::clone(&one), "acme"),
        LuaProvider::new(Arc::clone(&two), "acme"),
    );
    refresh_race(&env, &first, &second, 2, &held);
}

#[test]
fn two_callbacks_of_one_extension_refresh_once_without_deadlocking_the_vm() {
    let env = Env::new();
    let held = held_endpoint();
    env.secret("url", &held.url());
    env.secret("mode", "ok");
    let ext = env.extension();
    let first = LuaProvider::new(Arc::clone(&ext), "acme");
    let second = LuaProvider::new(Arc::clone(&ext), "acme");
    refresh_race(&env, &first, &second, 1, &held);
}

#[test]
fn a_refresh_that_times_out_in_its_function_leaves_the_file_and_frees_the_lock() {
    let env = Env::new();
    let ext = env.extension();
    // A server that records the request and never answers.
    let hang = OauthServer::start(vec![]);
    hang.hold();
    let provider = env.provider(&ext, &hang, "ok");
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let before = env.stored().unwrap();
    let deadline = env.clock.now() + TIMEOUT;
    let rx = start_token(&provider);
    // The function is on the wire, so it holds the lock.
    assert!(
        hang.await_requests(1, WAIT),
        "the refresh never reached the endpoint"
    );
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

#[test]
fn a_poll_with_nobody_attached_is_authentication_failed_and_sends_nothing() {
    let env = Env::new();
    let ext = login(&env);
    let server = OauthServer::start(vec![OauthReply::token("at", "rt", 3600)]);
    let provider = env.provider(&ext, &server, "poll");
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error}");
    assert!(error.to_string().contains("host.oauth.poll"), "{error}");
    assert!(server.requests().is_empty());
}

#[test]
fn open_with_nobody_attached_is_authentication_failed_and_opens_nothing() {
    let env = Env::new();
    let browser = Arc::new(Recording::never());
    let ext = login_with(&env, browser.clone());
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "open");
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error}");
    assert!(error.to_string().contains("host.oauth.open"), "{error}");
    assert!(browser.opened().is_empty());
}

#[test]
fn callback_with_nobody_attached_is_authentication_failed_and_listens_on_nothing() {
    let env = Env::new();
    let browser = Arc::new(Recording::never());
    let ext = login_with(&env, browser.clone());
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "callback");
    let port = free_port();
    env.secret("port", &port.to_string());
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error}");
    assert!(error.to_string().contains("host.oauth.callback"), "{error}");
    TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
}

#[test]
fn a_person_who_detaches_between_polls_stops_the_next_poll() {
    let env = Env::new();
    let browser = Arc::new(Recording::times(1));
    let ext = login_with(&env, browser);
    let server = OauthServer::start(vec![
        OauthReply::pending(),
        OauthReply::token("at", "rt", 3600),
    ]);
    let provider = env.provider(&ext, &server, "poll");
    let rx = start_token(&provider);
    assert!(env.clock.await_parked(wake(&env, 5), WAIT));
    env.clock.advance(Duration::from_secs(5));
    let error = finish(&rx).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error}");
    assert!(error.to_string().contains("host.oauth.poll"), "{error}");
    assert_eq!(server.request_count(), 1);
}

#[test]
fn an_attended_device_login_opens_polls_and_returns_the_token() {
    let env = Env::new();
    let browser = Arc::new(Recording::always());
    let ext = login_with(&env, browser.clone());
    let server = OauthServer::start(vec![OauthReply::token("at", "rt", 3600)]);
    let provider = env.provider(&ext, &server, "device");
    assert_eq!(finish(&start_token(&provider)).unwrap(), "at");
    assert_eq!(browser.opened(), [format!("{}/verify", server.url())]);
    assert_eq!(server.request_count(), 1);
}

#[test]
fn an_entry_script_that_opens_with_nobody_attached_is_authentication_failed() {
    let env = Env::new();
    let browser: Arc<Recording> = Arc::new(Recording::never());
    let dir = env.setup.root().join("extensions").join("entry_open");
    write(&dir.join("init.lua"), ENTRY_OPEN);
    let ext = Arc::new(
        LuaExtension::new("entry_open", dir, env.home(), env.clock.clone())
            .with_browser(browser.clone()),
    );
    let provider = LuaProvider::new(ext, "acme");
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error}");
    assert!(error.to_string().contains("host.oauth.open"), "{error}");
    assert!(browser.opened().is_empty());
}

#[test]
fn a_refresh_function_refused_after_a_failed_request_is_authentication_failed() {
    let env = Env::new();
    let ext = login_with(&env, Arc::new(Recording::never()));
    let server = OauthServer::start(vec![]);
    let provider = env.provider(&ext, &server, "refresh_poll");
    env.secret("dead", &dead_url());
    env.store(&json!({ "token": "old", "expires_at": WALL + 100 }));
    let before = env.stored().unwrap();
    let error = finish(&start_token(&provider)).unwrap_err();
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed, "{error}");
    assert!(error.to_string().contains("host.oauth.poll"), "{error}");
    assert_eq!(env.stored().unwrap(), before);
    assert_eq!(server.request_count(), 0);
}
