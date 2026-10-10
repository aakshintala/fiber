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

use std::cell::Cell;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use config::CredentialFile;
use mlua::{Lua, Table, Value as LuaValue};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};

mod held;

pub(crate) use held::{Held, Holder, LoginSlot};

use crate::host::Reply;
use crate::lua_provider::CredentialPair;

/// How often an off-thread wait looks at its cancel receiver.
const POLL: Duration = Duration::from_millis(20);

/// The longest request head the callback listener reads. Past it the client
/// gets a 431 and the listener serves the next connection.
const MAX_HEAD: usize = 8 * 1024;

/// How many polls in a row a connection may send nothing before the listener
/// drops it and serves the next. The listener serves one connection at a time
/// and browsers open idle speculative connections, so silence on one cannot
/// last to the callback's timeout. The count restarts whenever bytes arrive,
/// so pauses between bytes do not add up; the callback's own timeout still
/// ends the whole call.
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
    /// Shows the device-code `code` to enter at `url`; opens nothing.
    fn show(&self, url: &str, code: &str);
    /// Whether a person is attached to answer a login now. Read before each
    /// interactive step.
    fn attended(&self) -> bool;
}

/// The system's browser: the URL goes to stderr to copy, then `open` (macOS)
/// or `xdg-open` (elsewhere) is started with it.
pub struct SystemBrowser {
    program: PathBuf,
    stderr: Mutex<Box<dyn Write + Send>>,
}

impl SystemBrowser {
    /// A browser that starts `program` with the URL as its one argument.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            stderr: Mutex::new(Box::new(io::stderr())),
        }
    }

    #[cfg(test)]
    fn with_writer(program: impl Into<PathBuf>, writer: impl Write + Send + 'static) -> Self {
        Self {
            program: program.into(),
            stderr: Mutex::new(Box::new(writer)),
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
    fn show(&self, url: &str, code: &str) {
        // Told, never opened: the person types the code at the URL
        // (`docs/extensions.md`, "Host calls").
        let mut stderr = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match writeln!(stderr, "Go to {url} and enter the code {code}") {
            Ok(()) | Err(_) => {}
        }
    }

    fn open(&self, url: &str) {
        let mut stderr = self
            .stderr
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match writeln!(stderr, "{url}") {
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

    /// Never attached: a headless run refuses an interactive login with
    /// `authentication_failed` (`docs/model-routing.md`, "Keys, tokens and
    /// OAuth"). `fiber login` for a Lua OAuth provider supplies an attended
    /// browser.
    fn attended(&self) -> bool {
        false
    }
}

/// The Lua half of `host.oauth`: the calls that wait. Run with the `host`
/// table, the yield tag and a function that says whether the entry script is
/// running, which can wait on nothing.
const LUA: &str = r#"
local host, tag, failure, in_entry, note_failure, attended = ...
local oauth = host.oauth
local yield, resume, status = coroutine.yield, coroutine.resume, coroutine.status
-- The armed `coroutine.create` the prelude installed.
local create = coroutine.create
local pack, unpack = table.pack, table.unpack

-- The prelude's yield-forwarding `pcall`: `refresh` needs no HTTP tracking
-- around `held` reads and writes, so it shares the global. `attempt` keeps
-- its own loop to observe each `host.http` yield and its answer.
local run = pcall

-- Runs the refresh function `f` the way `run` does, and sees each `host.http`
-- yield and its answer. A function that raises fails the refresh: a table
-- passes through unchanged, anything else becomes `credential_failed`.
-- Either is recorded with whether the last request got a reply, which an
-- uncaught one fails the callback by.
local function attempt(f, ...)
  local co = create(f)
  local args = pack(...)
  local unreached = false
  while true do
    local r = pack(resume(co, unpack(args, 1, args.n)))
    if not r[1] then
      local err = r[2]
      local boundary = unreached and "refresh:unreached" or "refresh:reached"
      if type(err) == "table" then
        local text = tostring(err)
        local message = err.message
        note_failure(text, type(message) == "string" and message or text, boundary)
      else
        err = failure("credential_failed", tostring(err), boundary)
      end
      error(err, 0)
    end
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

local function need_person(call)
  if not attended() then
    local message = "host.oauth." .. call .. " needs a person to log in, and nobody is attached"
    error(failure("authentication_failed", message, "unattended:" .. call), 0)
  end
end

function oauth.callback(opts)
  in_callback("callback")
  local port = type(opts) == "table" and opts.port or nil
  if math.type(port) ~= "integer" or port < 1 or port > 65535 then
    error("host.oauth.callback: `port` must be a whole number from 1 to 65535", 2)
  end
  local path = type(opts) == "table" and opts.path or nil
  if path ~= nil and (type(path) ~= "string" or path:sub(1, 1) ~= "/") then
    error("host.oauth.callback: `path` must be a string starting with `/`", 2)
  end
  need_person("callback")
  local query, code, message = yield(tag, "callback", { port = port, path = path })
  if query == nil then error(failure(code, message), 0) end
  return query
end

function oauth.poll(opts)
  in_callback("poll")
  if type(opts) ~= "table" or type(opts.url) ~= "string" then
    error("host.oauth.poll: `url` must be a string", 2)
  end
  -- A device-code endpoint answers 403 or 404 while the person has not
  -- approved yet, so those statuses wait like `authorization_pending`
  -- (`docs/extensions.md`, "Host calls").
  local pending = opts.pending
  if pending ~= nil then
    if type(pending) ~= "table" then
      error("host.oauth.poll: `pending` must be a list of whole-number statuses from 100 to 599", 2)
    end
    local count, top = 0, 0
    for key, status in pairs(pending) do
      if math.type(key) ~= "integer" or key < 1
        or math.type(status) ~= "integer" or status < 100 or status > 599 then
        error("host.oauth.poll: `pending` must be a list of whole-number statuses from 100 to 599", 2)
      end
      count = count + 1
      if key > top then top = key end
    end
    if top ~= count then
      error("host.oauth.poll: `pending` must be a list of whole-number statuses from 100 to 599", 2)
    end
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
    need_person("poll")
    local reply = host.http(request)
    local waiting = false
    if pending ~= nil then
      for _, status in ipairs(pending) do
        if reply.status == status then waiting = true end
      end
    end
    if not waiting then
      local ok, body = pcall(json.decode, reply.body)
      -- A JSON array decodes to a Lua table too: only a `{` after any
      -- space opens the object ruling 17 requires.
      if not ok or type(body) ~= "table" or reply.body:match("^%s*(.)") ~= "{" then
        error(failure("unreadable_reply", "host.oauth.poll: status " .. reply.status .. " with a body that is not a JSON object"), 0)
      end
      local code = body.error
      if code == nil then
        if reply.status < 200 or reply.status > 299 then
          error(failure("http_error", "host.oauth.poll: status " .. reply.status), 0)
        end
        return body
      elseif code == "slow_down" then
        interval = interval + 5
        if interval > 3600 then
          error(failure("rate_limited", "host.oauth.poll: the server asked for more than 3600 seconds between polls"), 0)
        end
      elseif code ~= "authorization_pending" then
        local detail = type(body.error_description) == "string" and (": " .. body.error_description) or ""
        error(failure("authentication_failed", "host.oauth.poll: " .. tostring(code) .. detail), 0)
      end
    end
    yield(tag, "sleep", interval)
  end
end

function oauth.refresh(fn)
  in_callback("refresh")
  if type(fn) ~= "function" then
    error("host.oauth.refresh: takes a function", 2)
  end
  local held, code, message = yield(tag, "lock")
  if held == nil then
    if message == nil then error(code, 0) else error(failure(code, message), 0) end
  end
  local ok, result = run(function()
    local stored, code, message = held:read()
    if code ~= nil then
      if message == nil then error(code, 0) else error(failure(code, message), 0) end
    end
    if stored ~= nil and not held:due(stored) then return stored end
    -- A login holds no file: its function runs directly, so nothing it
    -- raises is recorded as a refresh failure, and the login reports it
    -- (`docs/model-routing.md`, "Logging in").
    local fresh
    if held:login() then
      fresh = fn(stored)
    else
      fresh = attempt(fn, stored)
    end
    local _, wcode, wmessage = held:write(fresh)
    if wcode ~= nil then
      -- An expired value outside a login maps as a rejected refresh; inside
      -- one the login reports it (`docs/extensions.md`, "Host calls").
      if not held:login() and wmessage ~= nil then
        error(failure(wcode, wmessage, "refresh:reached"), 0)
      end
      if wmessage == nil then error(wcode, 0) else error(failure(wcode, wmessage), 0) end
    end
    return fresh
  end)
  held:release()
  if not ok then error(result, 0) end
  return result
end
"#;

/// A Lua string as UTF-8: anything else is the caller's error.
fn utf8_string(value: &LuaValue, message: &str) -> Result<String, crate::host::failure::Raise> {
    if let LuaValue::String(text) = value
        && let Ok(text) = text.to_str()
    {
        return Ok(text.to_owned());
    }
    Err(crate::host::failure::Raise::Arg(message.to_owned()))
}

/// A Lua string as UTF-8, or `None` when it is no string at all: the caller
/// names the combined error.
fn utf8_opt(
    value: &LuaValue,
    message: &str,
) -> Result<Option<String>, crate::host::failure::Raise> {
    if matches!(value, LuaValue::Nil) {
        return Ok(None);
    }
    // A non-string is not `None`: the combined error names both.
    if let LuaValue::String(text) = value
        && let Ok(text) = text.to_str()
    {
        return Ok(Some(text.to_owned()));
    }
    Err(crate::host::failure::Raise::Arg(message.to_owned()))
}

/// Sets `host.oauth`. `entry` is true while the entry script runs.
/// `failure` raises a table; `note_failure` preserves refresh's outer mapping.
pub(crate) fn install(
    lua: &Lua,
    host: &Table,
    tag: &Table,
    browser: Arc<dyn Browser>,
    entry: Rc<Cell<bool>>,
    failure: mlua::Function,
    note_failure: mlua::Function,
) -> mlua::Result<()> {
    let oauth = lua.create_table()?;
    let open_browser = Arc::clone(&browser);
    let open = move |_lua: &mlua::Lua, url: LuaValue| {
        let url = utf8_string(&url, "host.oauth.open: url must be a string")?;
        if !open_browser.attended() {
            return Err(crate::host::failure::Raise::Failed(
                contract::ErrorCode::AuthenticationFailed,
                "host.oauth.open needs a person to log in, and nobody is attached".to_owned(),
            ));
        }
        open_browser.open(&url);
        Ok(mlua::MultiValue::new())
    };
    let show_browser = Arc::clone(&browser);
    let show = move |_lua: &mlua::Lua, (url, code): (LuaValue, LuaValue)| {
        // `show` runs on the thread like `open`: it shows the code and
        // opens nothing, so it never yields (`docs/extensions.md`, "Host
        // calls").
        let (Some(url), Some(code)) = (
            utf8_opt(&url, "host.oauth.show: `url` and `code` must be strings")?,
            utf8_opt(&code, "host.oauth.show: `url` and `code` must be strings")?,
        ) else {
            return Err(crate::host::failure::Raise::Arg(
                "host.oauth.show: `url` and `code` must be strings".to_owned(),
            ));
        };
        if !show_browser.attended() {
            return Err(crate::host::failure::Raise::Failed(
                contract::ErrorCode::AuthenticationFailed,
                "host.oauth.show needs a person to log in, and nobody is attached".to_owned(),
            ));
        }
        show_browser.show(&url, &code);
        Ok(mlua::MultiValue::new())
    };
    crate::host::failure::register(lua, &oauth, "show", &failure, Some("unattended:show"), show)?;
    crate::host::failure::register(lua, &oauth, "open", &failure, Some("unattended:open"), open)?;
    crate::host::failure::register(lua, &oauth, "pkce", &failure, None, |lua, ()| {
        let verifier = verifier().map_err(|message| {
            crate::host::failure::Raise::Failed(contract::ErrorCode::IoFailed, message)
        })?;
        let pair = lua
            .create_table()
            .map_err(|e| crate::host::failure::Raise::Arg(e.to_string()))?;
        pair.set("challenge", challenge(&verifier))
            .map_err(|e| crate::host::failure::Raise::Arg(e.to_string()))?;
        pair.set("verifier", verifier)
            .map_err(|e| crate::host::failure::Raise::Arg(e.to_string()))?;
        Ok(mlua::MultiValue::from_vec(vec![mlua::Value::Table(pair)]))
    })?;
    host.set("oauth", oauth)?;
    let in_entry = crate::host::failure::in_entry(lua, &entry)?;
    let attended = lua.create_function(move |_, ()| Ok(browser.attended()))?;
    lua.load(LUA).set_name("=host.oauth").call::<()>((
        host.clone(),
        tag.clone(),
        failure,
        in_entry,
        note_failure,
        attended,
    ))
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

/// Binds the callback listener: loopback only.
fn bind(port: u16) -> io::Result<TcpListener> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, port))
}

/// Binds `port` and serves one request off the thread, delivering its query.
/// Ok: the cancel handle; dropping it ends the listener and frees the port.
/// Err: nothing listens and nothing was delivered; the caller delivers it.
pub(crate) fn listen(
    port: u16,
    path: Option<String>,
    deliver: &Deliver,
) -> Result<Sender<()>, Reply> {
    // A port that cannot be bound is `io_failed`; a request whose query
    // cannot be read is `unreadable_reply`, both raised as `{ code,
    // message }` by the callback half.
    let fail = |why: &dyn std::fmt::Display| {
        Reply::Query(Err((
            contract::ErrorCode::IoFailed,
            format!("host.oauth.callback: port {port}: {why}"),
        )))
    };
    let listener = match bind(port).and_then(|l| l.set_nonblocking(true).map(|()| l)) {
        Ok(listener) => listener,
        Err(e) => {
            return Err(fail(&e));
        }
    };
    let (cancel, stop) = mpsc::channel();
    let send = Arc::clone(deliver);
    let spawned = thread::Builder::new()
        .name("oauth callback".to_owned())
        .spawn(move || {
            if let Some(result) = serve(&listener, &stop, &path) {
                send(Reply::Query(result));
            }
        });
    match spawned {
        Ok(_) => Ok(cancel),
        Err(e) => Err(fail(&e)),
    }
}

type Query = Result<Vec<(String, String)>, (contract::ErrorCode, String)>;

/// Serves connections until one is a request, then returns its query. None
/// when cancelled.
fn serve(
    listener: &TcpListener,
    stop: &mpsc::Receiver<()>,
    path: &Option<String>,
) -> Option<Query> {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if let Some(result) = answer(stream, stop, path) {
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
/// in its query. None when the connection said nothing usable, or when it
/// asked for another path than the callback serves: that gets a 404 and the
/// listener keeps waiting (`docs/extensions.md`, "Host calls").
fn answer(
    mut stream: TcpStream,
    stop: &mpsc::Receiver<()>,
    path: &Option<String>,
) -> Option<Query> {
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
    if let Some(served) = path
        && target.split_once('?').map_or(target, |(path, _)| path) != *served
    {
        respond(&mut stream, "404 Not Found");
        return None;
    }
    match parse_query(target.split_once('?').map_or("", |(_, query)| query)) {
        Ok(pairs) => {
            respond(&mut stream, "200 OK");
            Some(Ok(pairs))
        }
        Err(why) => {
            respond(&mut stream, "400 Bad Request");
            Some(Err((
                contract::ErrorCode::UnreadableReply,
                format!("host.oauth.callback: {why}"),
            )))
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
/// value. An invalid `%` escape is an error. An error names the malformed
/// parameter's recognised OAuth name, never any query bytes: any other key
/// gives a generic phrase.
fn parse_query(query: &str) -> Result<Vec<(String, String)>, String> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = decode(key).map_err(|phrase| format!("a parameter name {phrase}"))?;
            let value = decode(value).map_err(|phrase| {
                if NAMED.contains(&key.as_str()) {
                    format!("the `{key}` parameter's value {phrase}")
                } else {
                    format!("a parameter's value {phrase}")
                }
            })?;
            Ok((key, value))
        })
        .collect()
}

/// The recognised OAuth 2.0 callback parameters (RFC 6749, section 4.1.2)
/// and `iss` (RFC 9207): the only names an error ever repeats.
const NAMED: &[&str] = &[
    "code",
    "state",
    "error",
    "error_description",
    "error_uri",
    "iss",
];

fn decode(text: &str) -> Result<String, &'static str> {
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
                    .ok_or("has an invalid % escape")?;
                out.push(value);
            }
            other => out.push(other),
        }
    }
    String::from_utf8(out).map_err(|_| "is not UTF-8 once decoded")
}

/// Starts the off-thread wait for the pair's credential lock, which delivers
/// the lock. Ok: the cancel handle. Err: no wait started and nothing was
/// delivered; the caller delivers it.
pub(crate) fn lock(
    home: &Path,
    pair: &CredentialPair,
    deliver: &Deliver,
) -> Result<Sender<()>, Reply> {
    let fail = |why: &dyn std::fmt::Display| {
        Reply::Lock(Err(crate::host::LockError::Coded((
            contract::ErrorCode::IoFailed,
            format!("host.oauth.refresh: {why}"),
        ))))
    };
    let file = match CredentialFile::new(home, &pair.credential, &pair.label) {
        Ok(file) => file,
        Err(e) => {
            return Err(fail(&e));
        }
    };
    let (cancel, stop) = mpsc::channel();
    let send = Arc::clone(deliver);
    let spawned = thread::Builder::new()
        .name("oauth lock".to_owned())
        .spawn(move || {
            loop {
                match file.try_lock() {
                    Ok(Some(lock)) => return send(Reply::Lock(Ok(Holder::File(lock)))),
                    Ok(None) => {}
                    Err(e) => {
                        return send(Reply::Lock(Err(crate::host::LockError::Coded((
                            contract::ErrorCode::IoFailed,
                            format!("host.oauth.refresh: {e}"),
                        )))));
                    }
                }
                match stop.recv_timeout(POLL) {
                    Err(RecvTimeoutError::Timeout) => {}
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                }
            }
        });
    match spawned {
        Ok(_) => Ok(cancel),
        Err(e) => Err(fail(&e)),
    }
}

#[cfg(test)]
#[path = "oauth_tests.rs"]
mod tests;
