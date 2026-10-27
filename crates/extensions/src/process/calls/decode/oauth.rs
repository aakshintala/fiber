use serde_json::Value;

use contract::ErrorCode;

use super::{arg, decode_bytes};

/// One `host.oauth.*` URL in text: `open` names the bare URL, `poll`
/// the backticked one, as their Lua halves do.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_url(args: &[Value], message: &str) -> Result<String, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::String(url) => Ok(url.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((ErrorCode::InvalidArguments, message.to_owned()))
        }
    }
}

/// The `host.oauth.callback` port: a whole number from 1 to 65535, as
/// the Lua half checks. A float reads as no integer, like `math.type`
/// rejecting it.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_port(args: &[Value]) -> Result<u16, (ErrorCode, String)> {
    let fail = || {
        (
            ErrorCode::InvalidArguments,
            "host.oauth.callback: `port` must be a whole number from 1 to 65535".to_owned(),
        )
    };
    match arg(args, 0).as_u64() {
        Some(port) if (1..=65_535).contains(&port) => u16::try_from(port).map_err(|_| fail()),
        Some(_) | None => Err(fail()),
    }
}

/// The `host.oauth.callback` path: absent or null reads as none, as the
/// Lua half does; a string must start with `/`.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_path(args: &[Value]) -> Result<Option<String>, (ErrorCode, String)> {
    match arg(args, 1) {
        Value::Null => Ok(None),
        Value::String(path) if path.starts_with('/') => Ok(Some(path.clone())),
        Value::String(_)
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.oauth.callback: `path` must be a string starting with `/`".to_owned(),
        )),
    }
}

/// The `host.oauth.show` URL with its device code: both are required
/// strings, and the one sentence names both, as the Lua half does.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_show(args: &[Value]) -> Result<(String, String), (ErrorCode, String)> {
    let (Value::String(url), Value::String(code)) = (arg(args, 0), arg(args, 1)) else {
        return Err((
            ErrorCode::InvalidArguments,
            "host.oauth.show: `url` and `code` must be strings".to_owned(),
        ));
    };
    Ok((url.clone(), code.clone()))
}

/// The `host.oauth.poll` method: absent or null posts, as the Lua half
/// defaults. Anything set must be a string, which the poll hands to
/// `host.http` unchecked, as the Lua half does.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_method(args: &[Value]) -> Result<String, (ErrorCode, String)> {
    match arg(args, 1) {
        Value::Null => Ok("POST".to_owned()),
        Value::String(method) => Ok(method.clone()),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.oauth.poll: `method` must be a string".to_owned(),
        )),
    }
}

/// The `host.oauth.poll` headers: a table of strings, handed to
/// `host.http` unchecked, as the Lua half passes them through.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_headers(args: &[Value]) -> Result<Vec<(String, String)>, (ErrorCode, String)> {
    let table = || {
        (
            ErrorCode::InvalidArguments,
            "host.oauth.poll: `headers` must be a table".to_owned(),
        )
    };
    let strings = || {
        (
            ErrorCode::InvalidArguments,
            "host.oauth.poll: `headers` must be a table of strings".to_owned(),
        )
    };
    match arg(args, 2) {
        Value::Null => Ok(Vec::new()),
        Value::Object(headers) => headers
            .iter()
            .map(|(name, value)| match value {
                Value::String(value) => Ok((name.clone(), value.clone())),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => Err(strings()),
            })
            .collect(),
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => Err(table()),
    }
}

/// The `host.oauth.poll` body: absent or null sends none. A string
/// carries bytes as base64, since the wire has no raw byte string.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_body(args: &[Value]) -> Result<Option<Vec<u8>>, (ErrorCode, String)> {
    match arg(args, 3) {
        Value::Null => Ok(None),
        Value::String(text) => decode_bytes("host.oauth.poll", "body", text).map(Some),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.oauth.poll: `body` must be a base64 string".to_owned(),
        )),
    }
}

/// The `host.oauth.poll` waiting statuses: absent or null waits on the
/// device flow's pending reply, as the Lua half does with no list. A
/// list holds whole-number statuses from 100 to 599; JSON lists carry
/// their own 1-based order, so only the values are checked.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_pending(args: &[Value]) -> Result<Option<Vec<u16>>, (ErrorCode, String)> {
    let fail = || {
        (
            ErrorCode::InvalidArguments,
            "host.oauth.poll: `pending` must be a list of whole-number statuses from 100 to 599"
                .to_owned(),
        )
    };
    match arg(args, 4) {
        Value::Null => Ok(None),
        Value::Array(pending) => pending
            .iter()
            .map(|status| match status.as_u64() {
                Some(status) if (100..=599).contains(&status) => {
                    u16::try_from(status).map_err(|_| fail())
                }
                Some(_) | None => Err(fail()),
            })
            .collect::<Result<Vec<u16>, _>>()
            .map(Some),
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Object(_) => Err(fail()),
    }
}

/// The `host.oauth.poll` seconds between polls: absent or null waits
/// five, as the Lua half defaults. A shorter wait reads as one second,
/// as the Lua half clamps; longer than an hour fails.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn oauth_interval(args: &[Value]) -> Result<u64, (ErrorCode, String)> {
    const MAX_INTERVAL: u64 = 3600;
    let fail = || {
        (
            ErrorCode::InvalidArguments,
            "host.oauth.poll: `interval` must be a whole number of seconds, at most 3600"
                .to_owned(),
        )
    };
    match arg(args, 5) {
        Value::Null => Ok(5),
        Value::Number(interval) => match interval.as_u64() {
            Some(interval) if interval <= MAX_INTERVAL => Ok(interval.max(1)),
            _ => Err(fail()),
        },
        Value::Bool(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => Err(fail()),
    }
}

/// The `host.oauth.refresh` function marker: the program passes `true`
/// where the Lua half takes the function, since the nested call's body
/// arrives as its own request, not as a value on the wire.
#[allow(dead_code, reason = "asserted by tests; the process seam exercises it")]
pub(super) fn refresh_function(args: &[Value]) -> Result<(), (ErrorCode, String)> {
    if !matches!(arg(args, 0), Value::Bool(true)) {
        return Err((
            ErrorCode::InvalidArguments,
            "host.oauth.refresh: takes a function".to_owned(),
        ));
    }
    Ok(())
}
