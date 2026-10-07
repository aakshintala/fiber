//! The host calls a provider's Lua needs (`docs/extensions.md`, "Host
//! calls"): `host.secret`, `host.http`, `host.sha256`, `host.hmac_sha256` and
//! `json`, and converting between Lua values and JSON.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use config::{Config, CredentialLock};
use contract::clock::Clock;
use contract::files::PathLock;

use mlua::{Lua, LuaSerdeExt, LuaString, MultiValue, Table, Value as LuaValue};
use ring::{digest, hmac};
use serde_json::{Map, Number, Value};
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

use crate::oauth::{self, Browser};

pub(crate) mod exec;
mod fs;
mod log;
mod settings;
pub(crate) mod timers;
mod ui;

#[cfg(test)]
pub(crate) use fs::FakeLock;

/// One Lua extension's session: its own clone of the session's
/// configuration, the settings keys a repository may set, and the session's
/// per-path lock (`docs/extensions.md`, "Host calls").
#[derive(Clone)]
pub struct Session {
    /// The extension's own clone: its `set` is visible to its own later
    /// `get` at once, and to other sessions at their next load.
    pub config: Config,
    /// The top-level settings keys the repository's file may set.
    pub repo_settings: Vec<String>,
    /// The session's per-path lock, offered to `host.fs`.
    pub locks: Arc<dyn PathLock>,
}

/// What `install` builds the host calls from.
#[derive(Clone)]
pub(crate) struct HostContext {
    /// Fiber home, anchoring secrets and data directories.
    pub home: PathBuf,
    /// The session's workspace, resolving relative `host.fs` paths.
    pub workspace: PathBuf,
    /// The extension's name, slugged for data directories and settings.
    pub extension: String,
    /// The session, absent without [`LuaExtension::with_session`].
    pub session: Option<Session>,
    /// The extension's memory cap in bytes, bounding `host.fs.read`.
    pub memory_cap: usize,
}

/// `host.http` yields this tag, `"http"` and the request table. The
/// extension's thread runs the request off to the side and resumes the
/// coroutine with the reply (`docs/extensions.md`, "A host call suspends the
/// code that made it"). Every host call that waits yields the tag and its
/// kind, as [`Request`] lists.
const HTTP: &str = r#"
local host, tag = ...
function host.http(opts)
  if type(opts) ~= "table" or type(opts.url) ~= "string" then
    error("host.http: `url` must be a string", 2)
  end
  local status, body = coroutine.yield(tag, "http", opts)
  if status == nil then error(body, 0) end
  return { status = status, body = body }
end
"#;

/// `host.exec` yields this tag, `"exec"` and the call's spec. The
/// extension's thread runs the program off to the side and resumes the
/// coroutine with the reply (`docs/extensions.md`, "A host call suspends the
/// code that made it"). Refused in the entry script before it yields, like
/// `host.oauth.callback`.
const EXEC: &str = r#"
local host, tag, in_entry = ...
function host.exec(program, args, opts)
  if in_entry() then
    error("host.exec: not available while init.lua runs", 2)
  end
  if type(program) ~= "string" then
    error("host.exec: `program` must be a string", 2)
  end
  if args == nil then args = {} end
  if type(args) ~= "table" then
    error("host.exec: every argument must be a string", 2)
  end
  for k, v in pairs(args) do
    if math.type(k) ~= "integer" or k < 1 or type(v) ~= "string" then
      error("host.exec: every argument must be a string", 2)
    end
  end
  if opts == nil then opts = {} end
  if type(opts) ~= "table" then
    error("host.exec: `opts` must be a table", 2)
  end
  local cwd = opts.cwd
  if cwd ~= nil and type(cwd) ~= "string" then
    error("host.exec: `cwd` must be a string", 2)
  end
  local result, err = coroutine.yield(tag, "exec", { program = program, args = args, cwd = cwd })
  if result == nil then error(err, 0) end
  return result
end
"#;

/// How deep a Lua table may nest to become JSON. Deeper, such as a table
/// that holds itself, is an error rather than a stack overflow.
const MAX_DEPTH: usize = 128;

/// Sets the `host` and `json` globals. The returned tag is what the host
/// calls yield, so the scheduler can tell that yield from any other; the
/// returned table holds the timers' functions by id, which each firing runs.
pub(crate) fn install(
    lua: &Lua,
    ctx: HostContext,
    browser: Arc<dyn Browser>,
    entry: Rc<Cell<bool>>,
    hub: &Arc<crate::lua::Hub>,
) -> mlua::Result<(LuaValue, Table)> {
    let HostContext {
        home,
        workspace,
        extension,
        session,
        memory_cap,
    } = ctx;
    let host = lua.create_table()?;
    let secret_home = home.clone();
    host.set(
        "secret",
        lua.create_function(move |_, name: String| {
            config::read_secret(&secret_home, &name)
                .map(|secret| secret.map(|s| s.expose().trim().to_owned()))
                .map_err(mlua::Error::external)
        })?,
    )?;
    fs::install(
        lua,
        &host,
        workspace,
        home,
        &extension,
        memory_cap,
        session.as_ref(),
    )?;
    settings::install(lua, &host, &extension, session)?;
    let tag = lua.create_table()?;
    lua.load(HTTP)
        .set_name("=host.http")
        .call::<()>((host.clone(), tag.clone()))?;
    let exec_entry = Rc::clone(&entry);
    let in_entry = lua.create_function(move |_, ()| Ok(exec_entry.get()))?;
    lua.load(EXEC)
        .set_name("=host.exec")
        .call::<()>((host.clone(), tag.clone(), in_entry))?;
    oauth::install(lua, &host, &tag, browser, entry)?;
    let timer_funcs = timers::install(lua, &host, hub)?;
    log::install(lua, &host, hub, &extension)?;
    ui::install(lua, &host, hub, &extension)?;
    host.set(
        "sha256",
        lua.create_function(|_, bytes: LuaString| Ok(sha256_hex(&bytes.as_bytes())))?,
    )?;
    host.set(
        "hmac_sha256",
        lua.create_function(|lua, (key, bytes): (LuaString, LuaString)| {
            let key = hmac::Key::new(hmac::HMAC_SHA256, &key.as_bytes());
            lua.create_string(hmac::sign(&key, &bytes.as_bytes()).as_ref())
        })?,
    )?;
    let json = lua.create_table()?;
    json.set(
        "encode",
        lua.create_function(|lua, value: LuaValue| {
            // mlua's serde keeps JSON null (a null lightuserdata) and an
            // array's metatable, so `[]` and `{}` stay distinct.
            let json: Value = lua
                .from_value(value)
                .map_err(|e| mlua::Error::RuntimeError(format!("json: {e}")))?;
            Ok(json.to_string())
        })?,
    )?;
    json.set(
        "decode",
        lua.create_function(|lua, text: LuaString| {
            let value: Value = serde_json::from_slice(&text.as_bytes())
                .map_err(|e| mlua::Error::RuntimeError(format!("json.decode: {e}")))?;
            lua.to_value(&value)
        })?,
    )?;
    let globals = lua.globals();
    globals.set("host", host)?;
    globals.set("json", json)?;
    Ok((LuaValue::Table(tag), timer_funcs))
}

/// SHA-256 of `bytes`, as lowercase hex.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    digest::digest(&digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One `host.http` call, copied out of Lua so it can run off the extension's
/// thread. `timeout` is a fixed real-time backstop: the callback's declared
/// timeout plus the grace, or the entry script's load bound plus the grace.
/// It does not follow the time still left on the clock. The scheduler, on
/// the injected clock, is what stops the callback.
pub(crate) struct HttpRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    timeout: Option<Duration>,
}

/// What a callback waits on when it yields to the host: the tag, a kind
/// string and the call's argument. The extension's thread answers each with
/// the [`Reply`] of the same name.
pub(crate) enum Request {
    /// `host.http`.
    Http(HttpRequest),
    /// `host.exec`: run a program in its own process group.
    Exec(exec::ExecRequest),
    /// `host.oauth.callback`: serve one request on this localhost port.
    Callback { port: u16 },
    /// `host.oauth.refresh`: take the lock on the provider's credential.
    Lock,
    /// `host.oauth.poll`: wait this long on the extension's clock.
    Sleep(Duration),
}

/// The answer to a [`Request`]. A failure is the text Lua raises.
pub(crate) enum Reply {
    Http(Result<(u16, Vec<u8>), String>),
    /// How a `host.exec` run ended, or the text `host.exec` raises.
    Exec(Result<exec::Ran, String>),
    /// The query parameters of the one request the callback served.
    Query(Result<Vec<(String, String)>, String>),
    Lock(Result<CredentialLock, String>),
    Slept,
}

/// Reads a yield: its kind and argument. `timeout` is the real-time backstop
/// of an `http` request ([`HttpRequest`]): the declared timeout plus the
/// grace, fixed when the request is made, not the time still left on the
/// clock. `workspace` resolves an `exec` `cwd` as `host.fs` paths are, and
/// `cap` bounds each of its streams. None when the kind or argument is not
/// one the host yields.
pub(crate) fn request_from(
    kind: &str,
    arg: Option<&LuaValue>,
    timeout: Option<Duration>,
    workspace: &Path,
    cap: usize,
) -> mlua::Result<Option<Request>> {
    Ok(Some(match (kind, arg) {
        ("http", Some(LuaValue::Table(opts))) => Request::Http(http_request(opts, timeout)?),
        ("exec", Some(LuaValue::Table(spec))) => Request::Exec(exec_request(spec, workspace, cap)?),
        ("callback", Some(LuaValue::Table(opts))) => Request::Callback {
            port: opts.get("port")?,
        },
        ("lock", _) => Request::Lock,
        ("sleep", Some(LuaValue::Integer(seconds))) => Request::Sleep(Duration::from_secs(
            u64::try_from(*seconds).unwrap_or_default(),
        )),
        _ => return Ok(None),
    }))
}

/// Reads a `host.exec` spec: the program, its argument list and the working
/// directory, resolved against `workspace` as `host.fs` paths are (part 1's
/// `Fs::absolute`). Missing or `nil` arguments are none; any non-string
/// element raises.
fn exec_request(spec: &Table, workspace: &Path, cap: usize) -> mlua::Result<exec::ExecRequest> {
    let program: String = spec
        .get("program")
        .map_err(|_| mlua::Error::RuntimeError("host.exec: `program` must be a string".into()))?;
    let args_table: Table = spec.get("args").map_err(|_| {
        mlua::Error::RuntimeError("host.exec: every argument must be a string".into())
    })?;
    let mut args = Vec::new();
    for i in 1..=args_table.len().unwrap_or(0) {
        let arg: LuaValue = args_table.get(i)?;
        let LuaValue::String(s) = arg else {
            return Err(mlua::Error::RuntimeError(
                "host.exec: every argument must be a string".into(),
            ));
        };
        args.push(s.to_str()?.to_owned());
    }
    let cwd: Option<String> = spec.get("cwd")?;
    // `join` replaces the workspace when `cwd` is absolute.
    let cwd = match cwd {
        Some(cwd) => workspace.join(Path::new(&cwd)),
        None => workspace.to_path_buf(),
    };
    Ok(exec::ExecRequest {
        program,
        args,
        cwd,
        cap,
    })
}

fn http_request(opts: &Table, timeout: Option<Duration>) -> mlua::Result<HttpRequest> {
    let url: String = opts.get("url")?;
    let method = opts
        .get::<Option<String>>("method")?
        .unwrap_or_else(|| "GET".to_owned());
    let body: Option<LuaString> = opts.get("body")?;
    let mut headers = Vec::new();
    if let Some(table) = opts.get::<Option<Table>>("headers")? {
        for pair in table.pairs::<String, String>() {
            headers.push(pair?);
        }
    }
    Ok(HttpRequest {
        method,
        url,
        headers,
        body: body.map(|b| b.as_bytes().to_vec()),
        timeout,
    })
}

/// Runs `request`. A status other than 2xx is a reply, not an error. The
/// error string is what `host.http` raises.
pub(crate) fn perform(request: &HttpRequest) -> Result<(u16, Vec<u8>), String> {
    let fail = |e: &dyn std::fmt::Display| format!("host.http: {e}");
    let config = Agent::config_builder()
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(request.timeout)
        .build();
    let agent: Agent = config.into();
    let mut builder = ureq::http::Request::builder()
        .method(request.method.as_str())
        .uri(&request.url);
    for (name, value) in &request.headers {
        builder = builder.header(name, value);
    }
    let response = match &request.body {
        Some(body) => agent.run(builder.body(body.clone()).map_err(|e| fail(&e))?),
        None => agent.run(builder.body(()).map_err(|e| fail(&e))?),
    };
    let mut response = response.map_err(|e| fail(&e))?;
    let bytes = response.body_mut().read_to_vec().map_err(|e| fail(&e))?;
    Ok((response.status().as_u16(), bytes))
}

/// The values `coroutine.yield` returns to the host call that yielded: its
/// result, or nil and the error text. `clock` is what a held credential's
/// expiry is judged by.
pub(crate) fn resume_values(
    lua: &Lua,
    clock: &Arc<dyn Clock>,
    reply: Reply,
) -> mlua::Result<MultiValue> {
    let failed = |message: String| -> mlua::Result<MultiValue> {
        Ok(MultiValue::from_vec(vec![
            LuaValue::Nil,
            LuaValue::String(lua.create_string(message)?),
        ]))
    };
    match reply {
        Reply::Http(Ok((status, bytes))) => Ok(MultiValue::from_vec(vec![
            LuaValue::Integer(i64::from(status)),
            LuaValue::String(lua.create_string(bytes)?),
        ])),
        Reply::Http(Err(message))
        | Reply::Query(Err(message))
        | Reply::Lock(Err(message))
        | Reply::Exec(Err(message)) => failed(message),
        Reply::Query(Ok(pairs)) => {
            let table = lua.create_table()?;
            for (key, value) in pairs {
                table.raw_set(key, value)?;
            }
            Ok(MultiValue::from_vec(vec![LuaValue::Table(table)]))
        }
        Reply::Lock(Ok(lock)) => Ok(MultiValue::from_vec(vec![LuaValue::UserData(
            lua.create_userdata(oauth::Held::new(lock, Arc::clone(clock)))?,
        )])),
        Reply::Exec(Ok(ran)) => {
            let returned = lua.create_table()?;
            if let Some(code) = ran.exit_code {
                returned.set("exit_code", code)?;
            }
            if let Some(signal) = ran.signal {
                returned.set("signal", signal)?;
            }
            returned.set("stdout", lua.create_string(&ran.stdout)?)?;
            returned.set("stderr", lua.create_string(&ran.stderr)?)?;
            Ok(MultiValue::from_vec(vec![LuaValue::Table(returned)]))
        }
        Reply::Slept => Ok(MultiValue::from_vec(vec![LuaValue::Boolean(true)])),
    }
}

/// A Lua value as JSON. A table whose keys are exactly 1 to its length is an
/// array, an empty table is `[]`, and any other table is an object whose keys
/// are strings or numbers.
pub(crate) fn to_json(value: &LuaValue) -> mlua::Result<Value> {
    to_json_at(value, 0)
}

fn to_json_at(value: &LuaValue, depth: usize) -> mlua::Result<Value> {
    let fail = |why: String| mlua::Error::RuntimeError(format!("json: {why}"));
    Ok(match value {
        LuaValue::Nil => Value::Null,
        // `Value::NULL`, the null lightuserdata mlua's serde uses, is a JSON
        // null that occupies a table slot. Lua `nil` cannot: assigning it
        // deletes the key, so `[null]` would become `[]`.
        LuaValue::LightUserData(_) if value.is_null() => Value::Null,
        LuaValue::Boolean(b) => Value::Bool(*b),
        LuaValue::Integer(i) => Value::from(*i),
        LuaValue::Number(n) => Value::Number(
            Number::from_f64(*n).ok_or_else(|| fail(format!("{n} is not a JSON number")))?,
        ),
        LuaValue::String(s) => Value::String(s.to_str()?.to_owned()),
        LuaValue::Table(t) => {
            if depth >= MAX_DEPTH {
                return Err(fail(format!("a table nests deeper than {MAX_DEPTH}")));
            }
            let mut pairs = Vec::new();
            for pair in t.pairs::<LuaValue, LuaValue>() {
                let (key, item) = pair?;
                pairs.push((key, to_json_at(&item, depth + 1)?));
            }
            let n = pairs.len();
            let index = |key: &LuaValue| {
                key.as_integer()
                    .and_then(|i| usize::try_from(i).ok())
                    .filter(|i| (1..=n).contains(i))
            };
            if pairs.iter().all(|(key, _)| index(key).is_some()) {
                // n distinct keys, each in 1..=n: every slot is filled once.
                let mut list = vec![Value::Null; n];
                for (key, item) in pairs {
                    if let Some(slot) = index(&key).and_then(|i| list.get_mut(i - 1)) {
                        *slot = item;
                    }
                }
                Value::Array(list)
            } else {
                let mut map = Map::new();
                for (key, item) in pairs {
                    let key = if let LuaValue::String(s) = &key {
                        s.to_str()?.to_owned()
                    } else if let Some(i) = key.as_integer() {
                        i.to_string()
                    } else if let Some(n) = key.as_number() {
                        n.to_string()
                    } else {
                        return Err(fail(format!("a {} key", key.type_name())));
                    };
                    map.insert(key, item);
                }
                Value::Object(map)
            }
        }
        other @ (LuaValue::LightUserData(_)
        | LuaValue::Function(_)
        | LuaValue::Thread(_)
        | LuaValue::UserData(_)
        | LuaValue::Error(_)
        | LuaValue::Other(_)) => {
            return Err(fail(format!("a {} cannot be JSON", other.type_name())));
        }
    })
}

/// JSON as a Lua value. `null` is [`LuaValue::NULL`], so a slot in an array
/// or an object keeps the null.
pub(crate) fn to_lua(lua: &Lua, value: &Value) -> mlua::Result<LuaValue> {
    Ok(match value {
        Value::Null => LuaValue::NULL,
        Value::Bool(b) => LuaValue::Boolean(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => LuaValue::Integer(i),
            None => LuaValue::Number(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => LuaValue::String(lua.create_string(s)?),
        Value::Array(items) => {
            let table = lua.create_table()?;
            for (i, item) in items.iter().enumerate() {
                table.raw_set(i + 1, to_lua(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
        Value::Object(map) => {
            let table = lua.create_table()?;
            for (key, item) in map {
                table.raw_set(key.as_str(), to_lua(lua, item)?)?;
            }
            LuaValue::Table(table)
        }
    })
}

#[cfg(test)]
#[path = "host_tests.rs"]
mod tests;
