use crate::host::admission::decode_bytes;

use serde_json::{Map, Value};

use contract::ErrorCode;

use crate::process::context::HostOperation;

mod filesystem;
mod http_exec;
mod model;
mod oauth;

use self::{filesystem::*, http_exec::*, model::*, oauth::*};

/// One decoded process host or state call: through the services, held
/// as a question, or minted as a nested function first.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum DecodedCall {
    /// Runs through the services.
    Service(HostOperation),
    /// Parks on the callback until it ends, then declines.
    Ask(contract::events::Interaction),
    /// Mints the nested function request, waits for its answer, then
    /// refreshes with it.
    Refresh,
}

/// The `index`th argument, or null when the call passed nothing there:
/// a missing argument reads as null, so the type sentence for its
/// position names it, as in Lua.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn arg(args: &[Value], index: usize) -> &Value {
    args.get(index).unwrap_or(&Value::Null)
}

/// One required string argument in text.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn text_arg(
    call: &str,
    args: &[Value],
    index: usize,
    what: &str,
) -> Result<String, (ErrorCode, String)> {
    match arg(args, index) {
        Value::String(text) => Ok(text.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                format!("{call}: `{what}` must be a string"),
            ))
        }
    }
}

/// One bytes-position argument: a base64 string, rejected naming the
/// call and the argument when it is not.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn bytes_arg(
    call: &str,
    args: &[Value],
    index: usize,
    what: &str,
) -> Result<Vec<u8>, (ErrorCode, String)> {
    match arg(args, index) {
        Value::String(text) => decode_bytes(call, what, text),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                format!("{call}: `{what}` must be a base64 string"),
            ))
        }
    }
}

/// One required object argument.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn object_arg(
    call: &str,
    args: &[Value],
    index: usize,
    what: &str,
) -> Result<Map<String, Value>, (ErrorCode, String)> {
    match arg(args, index) {
        Value::Object(map) => Ok(map.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Err((
                ErrorCode::InvalidArguments,
                format!("{call}: `{what}` must be a table"),
            ))
        }
    }
}

/// Decodes one `host.*` or `state.*` call's JSON arguments with the Lua
/// half's validation and sentences. Byte positions read base64; every
/// other string reads text. A missing argument reads as null, so the
/// type sentence for its position names it, as in Lua.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn decode_host(call: &str, args: &[Value]) -> Result<DecodedCall, (ErrorCode, String)> {
    use DecodedCall::{Ask as Held, Refresh, Service};
    let op = match call {
        "host.http" => {
            let opts = http_opts(args)?;
            HostOperation::Http {
                method: http_method(&opts)?,
                url: http_url(&opts)?,
                headers: http_headers(&opts)?,
                body: http_body(&opts)?,
            }
        }
        "host.exec" => {
            let opts = exec_opts(args)?;
            HostOperation::Exec {
                program: exec_program(args)?,
                args: exec_args(args)?,
                cwd: exec_cwd(&opts)?,
                env: exec_env(&opts)?,
                input: exec_input(&opts)?,
            }
        }
        "host.tool" => HostOperation::Tool {
            name: tool_name(args)?,
            arguments: tool_arguments(args)?,
        },
        "host.model" => HostOperation::Model {
            request: model_request(args)?,
        },
        "host.delegate" => {
            let (arguments, timeout, run) = delegate_request(args)?;
            HostOperation::Delegate {
                arguments,
                timeout,
                run,
            }
        }
        "host.ask" => return Ok(Held(ask_interaction(args)?)),
        "host.oauth.callback" => HostOperation::OAuthCallback {
            port: oauth_port(args)?,
            path: oauth_path(args)?,
        },
        "host.oauth.open" => HostOperation::OAuthOpen {
            url: oauth_url(args, "host.oauth.open: url must be a string")?,
        },
        "host.oauth.show" => {
            let (url, code) = oauth_show(args)?;
            HostOperation::OAuthShow { url, code }
        }
        "host.oauth.pkce" => HostOperation::OAuthPkcs,
        "host.oauth.poll" => HostOperation::OAuthPoll {
            url: oauth_url(args, "host.oauth.poll: `url` must be a string")?,
            method: oauth_method(args)?,
            headers: oauth_headers(args)?,
            body: oauth_body(args)?,
            pending: oauth_pending(args)?,
            interval: oauth_interval(args)?,
        },
        "host.oauth.refresh" => {
            refresh_function(args)?;
            return Ok(Refresh);
        }
        "host.fs.read" => HostOperation::FsRead {
            path: fs_path("host.fs.read", args, 0)?,
        },
        "host.fs.write" => HostOperation::FsWrite {
            path: fs_path("host.fs.write", args, 0)?,
            contents: fs_contents(args)?,
            lock: fs_lock(args, 2)?,
        },
        "host.fs.list" => HostOperation::FsList {
            path: fs_path("host.fs.list", args, 0)?,
        },
        "host.fs.stat" => HostOperation::FsStat {
            path: fs_path("host.fs.stat", args, 0)?,
        },
        "host.fs.mkdir" => HostOperation::FsMkdir {
            path: fs_path("host.fs.mkdir", args, 0)?,
            lock: fs_lock(args, 1)?,
        },
        "host.fs.remove" => HostOperation::FsRemove {
            path: fs_path("host.fs.remove", args, 0)?,
            lock: fs_lock(args, 1)?,
        },
        "host.fs.rename" => HostOperation::FsRename {
            from: fs_path("host.fs.rename", args, 0)?,
            to: fs_path("host.fs.rename", args, 1)?,
            lock: fs_lock(args, 2)?,
        },
        "host.config.get" => HostOperation::ConfigGet {
            key: config_key("host.config.get", args)?,
        },
        "host.config.set" => HostOperation::ConfigSet {
            key: config_key("host.config.set", args)?,
            value: config_value(args)?,
            scope: config_scope(args)?,
        },
        "host.secret" => HostOperation::Secret {
            name: secret_name(args)?,
        },
        "host.data_dir" => HostOperation::DataDir {
            scope: data_dir_scope(args)?,
        },
        "host.sha256" => HostOperation::Sha256 {
            data: bytes_arg(call, args, 0, "data")?,
        },
        "host.hmac_sha256" => HostOperation::HmacSha256 {
            key: bytes_arg(call, args, 0, "key")?,
            data: bytes_arg(call, args, 1, "data")?,
        },
        "host.status" => HostOperation::Status {
            text: text_arg(call, args, 0, "text")?,
        },
        "host.widget" => HostOperation::Widget {
            id: text_arg(call, args, 0, "id")?,
            lines: widget_lines(args)?,
        },
        "host.emit" => HostOperation::Emit {
            data: arg(args, 0).clone(),
        },
        "host.log" => HostOperation::Log {
            message: log_message(args)?,
        },
        "state.get" => HostOperation::StateGet {
            key: state_key(args)?,
        },
        "state.keys" => HostOperation::StateKeys,
        "state.set" => {
            let (key, value, on_fork) = state_set(args)?;
            HostOperation::StateSet {
                key,
                value,
                on_fork,
            }
        }
        "state.unset" => HostOperation::StateUnset {
            key: state_key(args)?,
        },
        "host.after" | "host.every" => {
            return Err((
                ErrorCode::InvalidArguments,
                format!("{call} is not a process call; the program keeps its own timers"),
            ));
        }
        "host.drive" | "host.json" => {
            return Err((
                ErrorCode::InvalidArguments,
                format!("{call} is not a process call"),
            ));
        }
        _ => {
            return Err((
                ErrorCode::InvalidArguments,
                format!("unknown call `{call}`"),
            ));
        }
    };
    Ok(Service(op))
}
