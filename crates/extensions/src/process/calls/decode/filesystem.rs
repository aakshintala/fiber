use serde_json::Value;

use contract::ErrorCode;

use super::{arg, decode_bytes};

/// One `host.fs.*` path in text: the message names the call without
/// backticks, as the Lua half's shared sentence does.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn fs_path(
    call: &str,
    args: &[Value],
    index: usize,
) -> Result<String, (ErrorCode, String)> {
    match arg(args, index) {
        Value::String(path) => Ok(path.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                format!("{call}: path must be a string"),
            ))
        }
    }
}

/// The `host.fs.write` contents: a string carrying bytes as base64,
/// since the wire has no raw byte string.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn fs_contents(args: &[Value]) -> Result<Vec<u8>, (ErrorCode, String)> {
    match arg(args, 1) {
        Value::String(text) => decode_bytes("host.fs.write", "data", text),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "host.fs.write: data must be a string".to_owned(),
            ))
        }
    }
}

/// The lock option of a mutating `host.fs` call: absent or missing is
/// no lock, and a boolean is itself, as the Lua half reads. Anything
/// else is the caller's error; a non-table names the options.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn fs_lock(args: &[Value], index: usize) -> Result<bool, (ErrorCode, String)> {
    match arg(args, index) {
        Value::Null => Ok(false),
        Value::Object(opts) => match opts.get("lock").unwrap_or(&Value::Null) {
            Value::Null => Ok(false),
            Value::Bool(lock) => Ok(*lock),
            Value::Number(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => Err((
                ErrorCode::InvalidArguments,
                "host.fs: `lock` must be a boolean".to_owned(),
            )),
        },
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => Err((
            ErrorCode::InvalidArguments,
            "host.fs: options must be a table".to_owned(),
        )),
    }
}

/// One `host.config.*` dotted key in text. The message names the call,
/// as the Lua half's shared sentence does.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn config_key(call: &str, args: &[Value]) -> Result<String, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::String(key) => Ok(key.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                format!("{call}: key must be a string"),
            ))
        }
    }
}

/// The `host.config.set` value: any JSON but null, since null is no
/// value, as the Lua half refuses nil.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn config_value(args: &[Value]) -> Result<Value, (ErrorCode, String)> {
    let value = arg(args, 1);
    if value.is_null() {
        return Err((
            ErrorCode::InvalidArguments,
            "host.config.set: value must not be nil".to_owned(),
        ));
    }
    Ok(value.clone())
}

/// The `host.config.set` scope: `machine` or `project`, as the Lua
/// half reads.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn config_scope(
    args: &[Value],
) -> Result<crate::process::context::ConfigScope, (ErrorCode, String)> {
    use crate::process::context::ConfigScope::{Machine, Project};
    match arg(args, 2) {
        Value::String(scope) if scope == "machine" => Ok(Machine),
        Value::String(scope) if scope == "project" => Ok(Project),
        Value::String(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.config.set: scope must be \"machine\" or \"project\"".to_owned(),
        )),
    }
}

/// The `host.secret` name: any string, as the Lua half checks. The
/// service refuses a name the manifest does not declare.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn secret_name(args: &[Value]) -> Result<String, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::String(name) => Ok(name.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "host.secret: name must be a string".to_owned(),
            ))
        }
    }
}

/// The `host.data_dir` scope: `machine` or `project`, as the Lua half
/// reads.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn data_dir_scope(
    args: &[Value],
) -> Result<crate::process::context::DataScope, (ErrorCode, String)> {
    use crate::process::context::DataScope::{Machine, Project};
    match arg(args, 0) {
        Value::String(scope) if scope == "machine" => Ok(Machine),
        Value::String(scope) if scope == "project" => Ok(Project),
        Value::String(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.data_dir: scope must be \"machine\" or \"project\"".to_owned(),
        )),
    }
}

/// The `host.widget` lines: a list of strings, as the Lua half checks
/// each line.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn widget_lines(args: &[Value]) -> Result<Vec<String>, (ErrorCode, String)> {
    match arg(args, 1) {
        Value::Array(lines) => lines
            .iter()
            .map(|line| match line {
                Value::String(line) => Ok(line.clone()),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_) => Err((
                    ErrorCode::InvalidArguments,
                    "host.widget: `lines` must be a list of strings".to_owned(),
                )),
            })
            .collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "host.widget: `lines` must be a list of strings".to_owned(),
            ))
        }
    }
}

/// The `host.log` message: a number logs as its text, as the Lua half
/// converts, and anything else must be a string.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn log_message(args: &[Value]) -> Result<String, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::String(message) => Ok(message.clone()),
        Value::Number(message) => Ok(message.to_string()),
        Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.log: the message must be a string".to_owned(),
        )),
    }
}

/// One `state.*` key in text: the message names `state`, as the Lua
/// half's shared sentence does.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn state_key(args: &[Value]) -> Result<String, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::String(key) => Ok(key.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "state: key must be a string".to_owned(),
            ))
        }
    }
}

/// The `state.set` value with its fork rule: null is no value, as the
/// Lua half refuses nil, and the rule defaults to the point the call
/// runs at, as the Lua half does.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(super) fn state_set(
    args: &[Value],
) -> Result<(String, Value, contract::events::OnFork), (ErrorCode, String)> {
    use contract::events::OnFork::{AtPoint, Fresh, Latest};
    let key = state_key(args)?;
    let value = arg(args, 1);
    if value.is_null() {
        return Err((
            ErrorCode::InvalidArguments,
            "state.set: value is nil; use state.unset to remove the key".to_owned(),
        ));
    }
    let value = value.clone();
    let on_fork = match arg(args, 2) {
        Value::Null => AtPoint,
        Value::Object(options) => match options.get("on_fork").unwrap_or(&Value::Null) {
            Value::Null => AtPoint,
            Value::String(rule) if rule == "at_point" => AtPoint,
            Value::String(rule) if rule == "latest" => Latest,
            Value::String(rule) if rule == "fresh" => Fresh,
            Value::String(_)
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => {
                return Err((
                    ErrorCode::InvalidArguments,
                    "state.set: on_fork must be at_point, latest or fresh".to_owned(),
                ));
            }
        },
        Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "state.set: options must be a table".to_owned(),
            ));
        }
    };
    Ok((key, value, on_fork))
}
