//! The host calls a provider's Lua needs (`docs/extensions.md`, "Host
//! calls"): `host.secret`, `host.http`, `host.sha256`, `host.hmac_sha256` and
//! `json`, and converting between Lua values and JSON.

use std::path::PathBuf;
use std::time::Duration;

use mlua::{Lua, LuaSerdeExt, LuaString, MultiValue, Table, Value as LuaValue};
use ring::{digest, hmac};
use serde_json::{Map, Number, Value};
use ureq::Agent;
use ureq::tls::{RootCerts, TlsConfig};

/// `host.http` yields this tag and the request table. The extension's thread
/// runs the request off to the side and resumes the coroutine with the reply
/// (`docs/extensions.md`, "A host call suspends the code that made it").
const HTTP: &str = r#"
local host, tag = ...
function host.http(opts)
  if type(opts) ~= "table" or type(opts.url) ~= "string" then
    error("host.http: `url` must be a string", 2)
  end
  local status, body = coroutine.yield(tag, opts)
  if status == nil then error(body, 0) end
  return { status = status, body = body }
end
"#;

/// How deep a Lua table may nest to become JSON. Deeper, such as a table
/// that holds itself, is an error rather than a stack overflow.
const MAX_DEPTH: usize = 128;

/// Sets the `host` and `json` globals. The returned tag is what `host.http`
/// yields, so the scheduler can tell that yield from any other.
pub(crate) fn install(lua: &Lua, home: PathBuf) -> mlua::Result<LuaValue> {
    let host = lua.create_table()?;
    host.set(
        "secret",
        lua.create_function(move |_, name: String| {
            config::read_secret(&home, &name)
                .map(|secret| secret.map(|s| s.expose().trim().to_owned()))
                .map_err(mlua::Error::external)
        })?,
    )?;
    let tag = lua.create_table()?;
    lua.load(HTTP)
        .set_name("=host.http")
        .call::<()>((host.clone(), tag.clone()))?;
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
    Ok(LuaValue::Table(tag))
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
/// thread. `timeout` is the callback's own deadline, plus a grace so the
/// scheduler, not this request, is what stops the callback.
pub(crate) struct HttpRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    timeout: Option<Duration>,
}

/// Reads the request `host.http` yielded.
pub(crate) fn request_from(opts: &Table, timeout: Option<Duration>) -> mlua::Result<HttpRequest> {
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

/// The values `coroutine.yield` returns to `host.http`: a status and a body,
/// or nil and the error text.
pub(crate) fn resume_values(
    lua: &Lua,
    result: Result<(u16, Vec<u8>), String>,
) -> mlua::Result<MultiValue> {
    let (status, body) = match result {
        Ok((status, bytes)) => (
            LuaValue::Integer(i64::from(status)),
            lua.create_string(bytes)?,
        ),
        Err(message) => (LuaValue::Nil, lua.create_string(message)?),
    };
    Ok(MultiValue::from_vec(vec![status, LuaValue::String(body)]))
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
