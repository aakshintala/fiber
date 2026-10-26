use serde_json::{Map, Value};

use contract::ErrorCode;

use super::{arg, decode_bytes};

/// The `host.http` options table. The Lua half checks the table and its
/// `url` together, so a non-table reads as the missing URL.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn http_opts(args: &[Value]) -> Result<Map<String, Value>, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::Object(opts) => Ok(opts.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "host.http: `url` must be a string".to_owned(),
            ))
        }
    }
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn http_method(opts: &Map<String, Value>) -> Result<String, (ErrorCode, String)> {
    match opts.get("method").unwrap_or(&Value::Null) {
        Value::Null => Ok("GET".to_owned()),
        Value::String(method) => Ok(method.clone()),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.http: `method` must be a string".to_owned(),
        )),
    }
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn http_url(opts: &Map<String, Value>) -> Result<String, (ErrorCode, String)> {
    match opts.get("url").unwrap_or(&Value::Null) {
        Value::String(url) => Ok(url.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "host.http: `url` must be a string".to_owned(),
            ))
        }
    }
}

/// The `host.http` headers: a table of strings, as the Lua half reads
/// them. Anything else names the table; a non-string value names what the
/// table must hold.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn http_headers(
    opts: &Map<String, Value>,
) -> Result<Vec<(String, String)>, (ErrorCode, String)> {
    let table = || {
        (
            ErrorCode::InvalidArguments,
            "host.http: `headers` must be a table".to_owned(),
        )
    };
    let strings = || {
        (
            ErrorCode::InvalidArguments,
            "host.http: `headers` must be a table of strings".to_owned(),
        )
    };
    let Some(headers) = opts.get("headers") else {
        return Ok(Vec::new());
    };
    if headers.is_null() {
        return Ok(Vec::new());
    }
    let Value::Object(headers) = headers else {
        return Err(table());
    };
    headers
        .iter()
        .map(|(name, value)| match value {
            Value::String(value) => Ok((name.clone(), value.clone())),
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => Err(strings()),
        })
        .collect()
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn http_body(opts: &Map<String, Value>) -> Result<Option<Vec<u8>>, (ErrorCode, String)> {
    match opts.get("body").unwrap_or(&Value::Null) {
        Value::Null => Ok(None),
        Value::String(text) => decode_bytes("host.http", "body", text).map(Some),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.http: `body` must be a base64 string".to_owned(),
        )),
    }
}

/// The `host.exec` program: any string, as the Lua half checks. A bare
/// name or an absolute path runs unchanged, anything else resolves
/// against the workspace at run time.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn exec_program(args: &[Value]) -> Result<String, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::String(program) => Ok(program.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "host.exec: `program` must be a string".to_owned(),
            ))
        }
    }
}

/// The `host.exec` arguments: every element a string, as the Lua half
/// checks each list element. Absent or null arguments are none.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn exec_args(args: &[Value]) -> Result<Vec<String>, (ErrorCode, String)> {
    match arg(args, 1) {
        Value::Null => Ok(Vec::new()),
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => Ok(text.clone()),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => Err((
                    ErrorCode::InvalidArguments,
                    "host.exec: every argument must be a string".to_owned(),
                )),
            })
            .collect(),
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.exec: every argument must be a string".to_owned(),
        )),
    }
}

/// The `host.exec` options: absent or null reads as none, as the Lua
/// half fills in. Anything else must be a table holding the `cwd`,
/// `env` and `input` the decoders below read.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn exec_opts(args: &[Value]) -> Result<Map<String, Value>, (ErrorCode, String)> {
    match arg(args, 2) {
        Value::Null => Ok(Map::new()),
        Value::Object(opts) => Ok(opts.clone()),
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => Err((
            ErrorCode::InvalidArguments,
            "host.exec: `opts` must be a table".to_owned(),
        )),
    }
}

/// The `host.exec` working directory: absent or null reads as the
/// workspace, as the decoder resolves it at run time.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn exec_cwd(
    opts: &Map<String, Value>,
) -> Result<std::path::PathBuf, (ErrorCode, String)> {
    match opts.get("cwd").unwrap_or(&Value::Null) {
        Value::Null => Ok(std::path::PathBuf::new()),
        Value::String(cwd) => Ok(std::path::PathBuf::from(cwd)),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.exec: `cwd` must be a string".to_owned(),
        )),
    }
}

/// The `host.exec` extra environment: absent or null adds nothing. The
/// Lua half refuses empty names and the `=` and NUL bytes a spawn cannot
/// carry, and values carrying NUL, with the same sentences.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn exec_env(
    opts: &Map<String, Value>,
) -> Result<std::collections::BTreeMap<String, String>, (ErrorCode, String)> {
    let fail = || {
        (
            ErrorCode::InvalidArguments,
            "host.exec: `env` must be a table of strings".to_owned(),
        )
    };
    match opts.get("env").unwrap_or(&Value::Null) {
        Value::Null => Ok(std::collections::BTreeMap::new()),
        Value::Object(env) => env
            .iter()
            .map(|(name, value)| match value {
                Value::String(text) => {
                    if name.is_empty() || name.contains('=') || name.contains('\0') {
                        return Err((
                            ErrorCode::InvalidArguments,
                            "host.exec: `env` names must not be empty or contain `=` or NUL"
                                .to_owned(),
                        ));
                    }
                    if text.contains('\0') {
                        return Err((
                            ErrorCode::InvalidArguments,
                            "host.exec: `env` values must not contain NUL".to_owned(),
                        ));
                    }
                    Ok((name.clone(), text.clone()))
                }
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => Err(fail()),
            })
            .collect(),
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => Err(fail()),
    }
}

/// The `host.exec` standard input: absent or null reads as today's null
/// stdin. A string carries bytes as base64, since the wire has no raw
/// byte string; anything else is the caller's error.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn exec_input(
    opts: &Map<String, Value>,
) -> Result<Option<Vec<u8>>, (ErrorCode, String)> {
    match opts.get("input").unwrap_or(&Value::Null) {
        Value::Null => Ok(None),
        Value::String(text) => decode_bytes("host.exec", "input", text).map(Some),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.exec: `input` must be a string".to_owned(),
        )),
    }
}
