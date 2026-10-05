//! `host.oauth` (`docs/extensions.md`, "Host calls"): what a provider's
//! `credential()` builds an OAuth login from. `open` and `pkce` run on the
//! extension's thread. `callback`, `poll` and `refresh` suspend the callback
//! like `host.http`: a localhost listener, a wait on the extension's clock
//! and the lock on the stored credential run off the thread, and the thread
//! resumes the callback with their answer.
//!
//! An off-thread wait polls its cancel receiver every [`POLL`]. The scheduler
//! keeps the sender while the callback waits, so a callback that ends, times
//! out or is dropped frees its port and stops contending for the lock.

use std::cell::{Cell, RefCell};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use config::{CredentialFile, CredentialLock};
use contract::clock::Clock;
use mlua::{Lua, Table, UserData, UserDataMethods, Value as LuaValue};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::Value;

use crate::host::{self, Reply};
use crate::lua_provider::REFRESH_BEFORE;

/// The label a provider's stored credential has until `fiber login --as`
/// names others.
const LABEL: &str = "default";

/// How often an off-thread wait looks at its cancel receiver.
const POLL: Duration = Duration::from_millis(20);

/// The longest request head the callback listener reads. Past it the client
/// gets a 431 and the listener serves the next connection.
const MAX_HEAD: usize = 8 * 1024;

/// How many polls in a row a connection may send nothing before the listener
/// drops it and serves the next (Ruling 7 as amended on #309). The listener
/// serves one connection at a time and browsers open idle speculative
/// connections, so silence on one cannot last to the callback's timeout. The
/// count restarts whenever bytes arrive, so pauses between bytes do not add
/// up; the callback's own timeout still ends the whole call.
const SILENT_POLLS: u32 = 100;

/// What the browser shows after the redirect.
const PAGE: &str =
    "<!doctype html><title>Fiber</title><p>You can close this tab and return to Fiber.</p>";

/// Sends a [`Reply`] to the call that waits for it.
pub(crate) type Deliver = Arc<dyn Fn(Reply) + Send + Sync>;

/// Opens URLs in a person's browser.
pub trait Browser: Send + Sync {
    /// Shows `url` and tries to open it.
    fn open(&self, url: &str);
}

/// The system's browser: the URL goes to stderr to copy, then `open` (macOS)
/// or `xdg-open` (elsewhere) is started with it.
pub struct SystemBrowser {
    program: PathBuf,
}

impl SystemBrowser {
    /// A browser that starts `program` with the URL as its one argument.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }
}

impl Default for SystemBrowser {
    fn default() -> Self {
        Self::with_program(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        })
    }
}

impl Browser for SystemBrowser {
    fn open(&self, url: &str) {
        match writeln!(io::stderr(), "{url}") {
            Ok(()) | Err(_) => {}
        }
        // The URL is shown, so a browser that will not start is not an error.
        let spawned = Command::new(&self.program)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawned {
            // Reaps the child so it is not left a zombie.
            let reaper =
                thread::Builder::new()
                    .name("browser".to_owned())
                    .spawn(move || match child.wait() {
                        Ok(_) | Err(_) => {}
                    });
            match reaper {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

/// The Lua half of `host.oauth`: the calls that wait. Run with the `host`
/// table, the yield tag and a function that says whether the entry script is
/// running, which can wait on nothing.
const LUA: &str = r#"
local host, tag, in_entry, refresh_failed = ...
local oauth = host.oauth
local yield, resume, status = coroutine.yield, coroutine.resume, coroutine.status
-- The armed `coroutine.create` the prelude installed.
local create = coroutine.create
local pack, unpack = table.pack, table.unpack

-- Like `pcall(f, ...)`, but `f` may suspend on a host call. The VM's `pcall`
-- cannot be yielded across, so `f` runs in a coroutine of its own and each
-- yield it makes is passed up to the host, and the answer back down.
local function run(f, ...)
  local co = create(f)
  local args = pack(...)
  while true do
    local r = pack(resume(co, unpack(args, 1, args.n)))
    if not r[1] then return false, r[2] end
    if status(co) == "dead" then return true, r[2] end
    args = pack(yield(unpack(r, 2, r.n)))
  end
end

-- Runs the refresh function `f` the way `run` does, and sees each `host.http`
-- yield and its answer. A function that raises fails the refresh, and the host
-- is told whether the last request got no reply at all.
local function attempt(f, ...)
  local co = create(f)
  local args = pack(...)
  local unreached = false
  while true do
    local r = pack(resume(co, unpack(args, 1, args.n)))
    if not r[1] then refresh_failed(not unreached, tostring(r[2])) end
    if status(co) == "dead" then return r[2] end
    local http = r[2] == tag and r[3] == "http"
    args = pack(yield(unpack(r, 2, r.n)))
    if http then unreached = args[1] == nil end
  end
end

local function in_callback(call)
  if in_entry() then
    error("host.oauth." .. call .. ": needs a running callback, not the entry script", 3)
  end
end

function oauth.callback(opts)
  in_callback("callback")
  local port = type(opts) == "table" and opts.port or nil
  if math.type(port) ~= "integer" or port < 1 or port > 65535 then
    error("host.oauth.callback: `port` must be a whole number from 1 to 65535", 2)
  end
  local query, err = yield(tag, "callback", { port = port })
  if query == nil then error(err, 0) end
  return query
end

function oauth.poll(opts)
  in_callback("poll")
  if type(opts) ~= "table" or type(opts.url) ~= "string" then
    error("host.oauth.poll: `url` must be a string", 2)
  end
  local interval = opts.interval
  if interval == nil then interval = 5 end
  if math.type(interval) ~= "integer" or interval > 3600 then
    error("host.oauth.poll: `interval` must be a whole number of seconds, at most 3600", 2)
  end
  if interval < 1 then interval = 1 end
  local request = {
    url = opts.url,
    method = opts.method or "POST",
    headers = opts.headers,
    body = opts.body,
  }
  while true do
    local reply = host.http(request)
    local ok, body = pcall(json.decode, reply.body)
    if not ok or type(body) ~= "table" then
      error("host.oauth.poll: status " .. reply.status .. " with a body that is not a JSON object", 0)
    end
    local code = body.error
    if code == nil then
      if reply.status < 200 or reply.status > 299 then
        error("host.oauth.poll: status " .. reply.status, 0)
      end
      return body
    elseif code == "slow_down" then
      interval = interval + 5
      if interval > 3600 then
        error("host.oauth.poll: the server asked for more than 3600 seconds between polls", 0)
      end
    elseif code ~= "authorization_pending" then
      local detail = type(body.error_description) == "string" and (": " .. body.error_description) or ""
      error("host.oauth.poll: " .. tostring(code) .. detail, 0)
    end
    yield(tag, "sleep", interval)
  end
end

function oauth.refresh(fn)
  in_callback("refresh")
  if type(fn) ~= "function" then
    error("host.oauth.refresh: takes a function", 2)
  end
  local held, err = yield(tag, "lock")
  if held == nil then error(err, 0) end
  local ok, result = run(function()
    local stored = held:read()
    if stored ~= nil and not held:due(stored) then return stored end
    local fresh = attempt(fn, stored)
    held:write(fresh)
    return fresh
  end)
  held:release()
  if not ok then error(result, 0) end
  return result
end
"#;

/// Sets `host.oauth`. `entry` is true while the entry script runs.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    tag: &Table,
    browser: Arc<dyn Browser>,
    entry: Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let oauth = lua.create_table()?;
    oauth.set(
        "open",
        lua.create_function(move |_, url: String| {
            browser.open(&url);
            Ok(())
        })?,
    )?;
    oauth.set(
        "pkce",
        lua.create_function(|lua, ()| {
            let verifier = verifier().map_err(mlua::Error::runtime)?;
            let pair = lua.create_table()?;
            pair.set("challenge", challenge(&verifier))?;
            pair.set("verifier", verifier)?;
            Ok(pair)
        })?,
    )?;
    host.set("oauth", oauth)?;
    let in_entry = lua.create_function(move |_, ()| Ok(entry.get()))?;
    let refresh_failed = lua.create_function(|_, (reached, message): (bool, String)| {
        Err::<(), _>(mlua::Error::external(RefreshFailed { reached, message }))
    })?;
    lua.load(LUA).set_name("=host.oauth").call::<()>((
        host.clone(),
        tag.clone(),
        in_entry,
        refresh_failed,
    ))
}

/// What the refresh function's failure carries out of Lua: whether the token
/// endpoint was reached, so the host can tell a rejection from a network
/// failure. `reached` is false when the last `host.http` call the function
/// made before it raised got no reply.
#[derive(Debug)]
pub(crate) struct RefreshFailed {
    pub(crate) reached: bool,
    pub(crate) message: String,
}

impl std::fmt::Display for RefreshFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RefreshFailed {}

/// The refresh failure `e` carries, if it is one.
pub(crate) fn refresh_failure(e: &mlua::Error) -> Option<&RefreshFailed> {
    e.downcast_ref()
}

/// 32 random bytes as base64url without padding, 43 characters (RFC 7636,
/// section 4.1).
fn verifier() -> Result<String, String> {
    let mut bytes = [0_u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "host.oauth.pkce: the system has no random bytes".to_owned())?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// The S256 challenge of `verifier` (RFC 7636, section 4.2).
fn challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(digest::digest(&digest::SHA256, verifier.as_bytes()))
}

/// A stored credential held under its lock, as Lua sees it. `release()` and
/// a collected handle both free the lock.
pub(crate) struct Held {
    lock: RefCell<Option<CredentialLock>>,
    clock: Arc<dyn Clock>,
}

impl Held {
    pub(crate) fn new(lock: CredentialLock, clock: Arc<dyn Clock>) -> Self {
        Self {
            lock: RefCell::new(Some(lock)),
            clock,
        }
    }

    fn with<T>(&self, f: impl FnOnce(&CredentialLock) -> Result<T, String>) -> mlua::Result<T> {
        let held = self.lock.borrow();
        let lock = held.as_ref().ok_or_else(|| {
            mlua::Error::runtime("host.oauth.refresh: the credential is no longer held")
        })?;
        f(lock).map_err(mlua::Error::runtime)
    }

    /// Whether `stored` needs refreshing: not a usable credential, or one
    /// that expires within [`REFRESH_BEFORE`].
    fn due(&self, stored: &Value) -> bool {
        let now = self
            .clock
            .wall()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        let window = i64::try_from(REFRESH_BEFORE.as_secs()).unwrap_or(i64::MAX);
        usable(stored).is_none_or(|expires| expires <= now.saturating_add(window))
    }
}

/// The expiry of a stored credential: an object with a non-empty string
/// `token` and an integer `expires_at`. None for anything else.
fn usable(value: &Value) -> Option<i64> {
    let token = value.get("token").and_then(Value::as_str)?;
    if token.is_empty() {
        return None;
    }
    value.get("expires_at").and_then(Value::as_i64)
}

impl UserData for Held {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("read", |lua, this, ()| {
            let stored = this.with(|lock| lock.read().map_err(|e| e.to_string()))?;
            stored.map_or(Ok(LuaValue::Nil), |value| host::to_lua(lua, &value))
        });
        methods.add_method("due", |_, this, stored: LuaValue| {
            Ok(host::to_json(&stored).map_or(true, |value| this.due(&value)))
        });
        methods.add_method("write", |_, this, fresh: LuaValue| {
            let value = host::to_json(&fresh)?;
            if usable(&value).is_none() {
                return Err(mlua::Error::runtime(
                    "host.oauth.refresh: the function must return a table with a `token` string and an `expires_at` whole number of seconds",
                ));
            }
            this.with(|lock| lock.write(&value).map_err(|e| e.to_string()))
        });
        methods.add_method("release", |_, this, ()| {
            this.lock.borrow_mut().take();
            Ok(())
        });
    }
}

/// Binds the callback listener: loopback only.
fn bind(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port))
}

/// Listens on `port` for one request and delivers its query parameters. The
/// returned sender is the cancel handle: dropping it ends the listener and
/// frees the port. None when nothing is listening, in which case the error is
/// already delivered.
pub(crate) fn listen(port: u16, deliver: &Deliver) -> Option<Sender<()>> {
    let fail = |why: &dyn std::fmt::Display| {
        deliver(Reply::Query(Err(format!(
            "host.oauth.callback: port {port}: {why}"
        ))));
    };
    let listener = match bind(port).and_then(|l| l.set_nonblocking(true).map(|()| l)) {
        Ok(listener) => listener,
        Err(e) => {
            fail(&e);
            return None;
        }
    };
    let (cancel, stop) = mpsc::channel();
    let send = Arc::clone(deliver);
    let spawned = thread::Builder::new()
        .name("oauth callback".to_owned())
        .spawn(move || {
            if let Some(result) = serve(&listener, &stop) {
                send(Reply::Query(result));
            }
        });
    match spawned {
        Ok(_) => Some(cancel),
        Err(e) => {
            fail(&e);
            None
        }
    }
}

type Query = Result<Vec<(String, String)>, String>;

/// Serves connections until one is a request, then returns its query. None
/// when cancelled.
fn serve(listener: &TcpListener, stop: &mpsc::Receiver<()>) -> Option<Query> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(result) = answer(stream, stop) {
                    return Some(result);
                }
            }
            // Nothing waiting, or an accept that failed: either way the
            // listener waits a poll and tries again, and the callback's own
            // timeout bounds one that keeps failing.
            Err(_) => match stop.recv_timeout(POLL) {
                Err(RecvTimeoutError::Timeout) => {}
                Ok(()) | Err(RecvTimeoutError::Disconnected) => return None,
            },
        }
    }
}

/// Answers one connection. Some once it was a request, well formed or not
/// in its query. None when the connection said nothing usable.
fn answer(mut stream: TcpStream, stop: &mpsc::Receiver<()>) -> Option<Query> {
    // A BSD accept inherits the listener's non-blocking mode.
    stream.set_nonblocking(false).ok()?;
    stream.set_read_timeout(Some(POLL)).ok()?;
    let head = match read_head(&mut stream, stop) {
        Head::Complete(head) => head,
        Head::TooLong => {
            respond(&mut stream, "431 Request Header Fields Too Large");
            return None;
        }
        Head::Gone => return None,
    };
    let Some(target) = request_target(&head) else {
        respond(&mut stream, "400 Bad Request");
        return None;
    };
    match parse_query(target.split_once('?').map_or("", |(_, query)| query)) {
        Ok(pairs) => {
            respond(&mut stream, "200 OK");
            Some(Ok(pairs))
        }
        Err(why) => {
            respond(&mut stream, "400 Bad Request");
            Some(Err(format!("host.oauth.callback: {why}")))
        }
    }
}

enum Head {
    Complete(Vec<u8>),
    TooLong,
    /// Closed, silent or failing for [`SILENT_POLLS`] reads in a row, or
    /// cancelled.
    Gone,
}

/// Reads through the blank line that ends the head, so closing the socket
/// leaves nothing unread for the kernel to reset the reply over.
fn read_head(stream: &mut impl Read, stop: &mpsc::Receiver<()>) -> Head {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    let mut silent = 0;
    while !head.ends_with(b"\r\n\r\n") && !head.ends_with(b"\n\n") {
        // Every pass, not only after a silent read: a client that keeps
        // sending must not outlive the callback.
        if matches!(stop.try_recv(), Err(TryRecvError::Disconnected)) {
            return Head::Gone;
        }
        if head.len() >= MAX_HEAD {
            return Head::TooLong;
        }
        match stream.read(&mut byte) {
            Ok(0) => return Head::Gone,
            Ok(_) => {
                head.push(byte[0]);
                silent = 0;
            }
            // A read that times out is one silent poll. Any other error
            // counts as one too, so a connection that keeps failing ends
            // within the same bound.
            Err(_) => {
                silent += 1;
                if silent >= SILENT_POLLS {
                    return Head::Gone;
                }
            }
        }
    }
    Head::Complete(head)
}

/// The target of the request line `METHOD target HTTP/x`.
fn request_target(head: &[u8]) -> Option<&str> {
    let line = head.split(|b| *b == b'\n').next()?;
    let line = std::str::from_utf8(line).ok()?;
    let mut parts = line.split_whitespace();
    let (_method, target, version) = (parts.next()?, parts.next()?, parts.next()?);
    (version.starts_with("HTTP/") && parts.next().is_none()).then_some(target)
}

fn respond(stream: &mut TcpStream, status: &str) {
    let body = if status.starts_with("200") { PAGE } else { "" };
    let reply = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    match stream.write_all(reply.as_bytes()) {
        Ok(()) | Err(_) => {}
    }
}

/// A query string's parameters, in order, percent-decoded (`+` is a space).
/// A bare `?` or an empty pair gives nothing; a key without `=` has an empty
/// value. An invalid `%` escape is an error.
fn parse_query(query: &str) -> Result<Vec<(String, String)>, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            Ok((decode(key)?, decode(value)?))
        })
        .collect()
}

fn decode(text: &str) -> Result<String, String> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = |b: Option<u8>| b.and_then(|b| char::from(b).to_digit(16));
                let (high, low) = (hex(bytes.next()), hex(bytes.next()));
                let value = high
                    .zip(low)
                    .and_then(|(high, low)| u8::try_from(high * 16 + low).ok())
                    .ok_or_else(|| format!("`{text}` has an invalid % escape"))?;
                out.push(value);
            }
            other => out.push(other),
        }
    }
    String::from_utf8(out).map_err(|_| format!("`{text}` is not UTF-8 once decoded"))
}

/// Waits for the lock on `provider`'s stored credential, polling
/// [`CredentialFile::try_lock`], and delivers it. The returned sender is the
/// cancel handle. None when no wait started, in which case the error is
/// already delivered.
pub(crate) fn lock(home: &Path, provider: &str, deliver: &Deliver) -> Option<Sender<()>> {
    let fail = |why: &dyn std::fmt::Display| {
        deliver(Reply::Lock(Err(format!("host.oauth.refresh: {why}"))));
    };
    let file = match CredentialFile::new(home, provider, LABEL) {
        Ok(file) => file,
        Err(e) => {
            fail(&e);
            return None;
        }
    };
    let (cancel, stop) = mpsc::channel();
    let send = Arc::clone(deliver);
    let spawned = thread::Builder::new()
        .name("oauth lock".to_owned())
        .spawn(move || {
            loop {
                match file.try_lock() {
                    Ok(Some(lock)) => return send(Reply::Lock(Ok(lock))),
                    Ok(None) => {}
                    Err(e) => {
                        return send(Reply::Lock(Err(format!("host.oauth.refresh: {e}"))));
                    }
                }
                match stop.recv_timeout(POLL) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
    match spawned {
        Ok(_) => Some(cancel),
        Err(e) => {
            fail(&e);
            None
        }
    }
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
