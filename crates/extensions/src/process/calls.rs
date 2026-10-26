//! Routing one process `call` (`docs/extensions.md`, "Process
//! extensions").
//!
//! A `call` is a registration, a `host` call or a `state` call. A name the
//! Lua side registers with — `fiber.*` — registers here with the same
//! validation and the same wording; `host.drive` sends a driver command
//! through the `Drive` seam. Every other name dispatches through the
//! injected host services, which own the host and state call matrix.

use serde_json::{Map, Value};

use contract::ErrorCode;

use crate::host::admission::decode_bytes;

use super::context::HostOperation;

/// Where one `call` routes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum Route {
    /// A registration call, answered through the registry.
    Register {
        /// The call's name as Lua writes it, such as `fiber.tool`.
        call: String,
        /// The Lua call's arguments.
        args: Vec<Value>,
    },
    /// A `host.drive` call: send a driver command through the `Drive`
    /// seam, preserving its validation and rights.
    Drive {
        /// The driver command, such as `steer`.
        command: String,
        /// Its arguments.
        args: Map<String, Value>,
    },
    /// A `host` or `state` call, dispatched through the injected host
    /// services.
    Host {
        /// The call's name as Lua writes it, such as `host.http`.
        call: String,
        /// The Lua call's arguments.
        args: Vec<Value>,
    },
}

/// What reading a `host.drive` call's arguments refuses with: the Lua
/// call's exact sentence, carried as `invalid_arguments`.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn drive_error(message: &str) -> (contract::ErrorCode, String) {
    (contract::ErrorCode::InvalidArguments, message.to_owned())
}

/// Routes one `call` with `args`: `fiber.*` registers, `host.drive`
/// drives, and every other name dispatches as a host or state call. A
/// misshapen `host.drive` refuses with the Lua call's sentence.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn route_call(
    call: &str,
    args: Vec<Value>,
) -> Result<Route, (contract::ErrorCode, String)> {
    if call.starts_with("fiber.") {
        return Ok(Route::Register {
            call: call.to_owned(),
            args,
        });
    }
    if call == "host.drive" {
        let (command, drive_args) = drive_request(&args)?;
        return Ok(Route::Drive {
            command,
            args: drive_args,
        });
    }
    Ok(Route::Host {
        call: call.to_owned(),
        args,
    })
}

/// Reads a `host.drive` call's arguments: the driver command and its
/// arguments as a JSON object. A missing argument reads as no arguments,
/// as an empty Lua table does; anything else refuses, as `host.drive`
/// does in Lua.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn drive_request(
    args: &[Value],
) -> Result<(String, Map<String, Value>), (contract::ErrorCode, String)> {
    let Some(command) = args.first().and_then(Value::as_str) else {
        return Err(drive_error("host.drive: `command` must be a string"));
    };
    let drive_args = match args.get(1) {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(Value::Array(items)) if items.is_empty() => Map::new(),
        Some(_) => {
            return Err(drive_error("host.drive: `args` must be a table"));
        }
    };
    Ok((command.to_owned(), drive_args))
}

/// The function keys one registration call installs its handler under:
/// every `true` the wire carries becomes a request Fiber sends. A table
/// `effects` runs no function, and a provider or harness installs one
/// handler per function its spec enables.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) fn handler_keys(call: &str, args: &[Value]) -> Vec<String> {
    let spec = args.get(1);
    match call {
        "fiber.tool" => {
            let mut keys = vec!["run".to_owned()];
            let effects_is_function = spec
                .and_then(Value::as_object)
                .and_then(|spec| spec.get("effects"))
                .is_some_and(|effects| *effects == Value::Bool(true));
            if effects_is_function {
                keys.push("effects".to_owned());
            }
            keys
        }
        "fiber.hook" | "fiber.watch" | "fiber.command" | "fiber.search_backend" => {
            vec!["run".to_owned()]
        }
        "fiber.provider" | "fiber.harness" => spec
            .and_then(Value::as_object)
            .map(|spec| {
                spec.iter()
                    .filter(|(_, entry)| {
                        entry
                            .as_object()
                            .and_then(|entry| entry.get("run"))
                            .is_some_and(|run| *run == Value::Bool(true))
                    })
                    .map(|(name, _)| name.clone())
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

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

/// The `host.http` options table. The Lua half checks the table and its
/// `url` together, so a non-table reads as the missing URL.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn http_opts(args: &[Value]) -> Result<Map<String, Value>, (ErrorCode, String)> {
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
fn http_method(opts: &Map<String, Value>) -> Result<String, (ErrorCode, String)> {
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
fn http_url(opts: &Map<String, Value>) -> Result<String, (ErrorCode, String)> {
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
fn http_headers(opts: &Map<String, Value>) -> Result<Vec<(String, String)>, (ErrorCode, String)> {
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
fn http_body(opts: &Map<String, Value>) -> Result<Option<Vec<u8>>, (ErrorCode, String)> {
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
fn exec_program(args: &[Value]) -> Result<String, (ErrorCode, String)> {
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
fn exec_args(args: &[Value]) -> Result<Vec<String>, (ErrorCode, String)> {
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
fn exec_opts(args: &[Value]) -> Result<Map<String, Value>, (ErrorCode, String)> {
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
fn exec_cwd(opts: &Map<String, Value>) -> Result<std::path::PathBuf, (ErrorCode, String)> {
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
fn exec_env(
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
fn exec_input(opts: &Map<String, Value>) -> Result<Option<Vec<u8>>, (ErrorCode, String)> {
    match opts.get("input").unwrap_or(&Value::Null) {
        Value::Null => Ok(None),
        Value::String(text) => decode_bytes("host.exec", "input", text).map(Some),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.exec: `input` must be a string".to_owned(),
        )),
    }
}

/// The `host.tool` name: any string, as the Lua half checks. The runner
/// refuses an unknown name as an ordinary return.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn tool_name(args: &[Value]) -> Result<String, (ErrorCode, String)> {
    match arg(args, 0) {
        Value::String(name) => Ok(name.clone()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err((
                ErrorCode::InvalidArguments,
                "host.tool: `name` must be a string".to_owned(),
            ))
        }
    }
}

/// The `host.tool` arguments: absent or null reads as no arguments, and
/// an empty table reads the same way, since an empty Lua table is an
/// empty JSON array. A non-empty array stays an array, and the runner
/// refuses it `invalid_arguments` as an ordinary return. Anything else
/// is the caller's error. Past 125 nested containers the parse the Lua
/// half replays would refuse, so deeper values fail naming the limit.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn tool_arguments(args: &[Value]) -> Result<Value, (ErrorCode, String)> {
    const TOOL_DEPTH: usize = 125;
    let arguments = match arg(args, 1) {
        Value::Null => return Ok(Value::Object(Map::new())),
        Value::Array(items) if items.is_empty() => return Ok(Value::Object(Map::new())),
        Value::Array(items) => Value::Array(items.clone()),
        Value::Object(arguments) => Value::Object(arguments.clone()),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.tool: `args` must be a table".to_owned(),
            ));
        }
    };
    if depth(&arguments) > TOOL_DEPTH {
        return Err((
            ErrorCode::InvalidArguments,
            "host.tool: `args` nests deeper than 125".to_owned(),
        ));
    }
    Ok(arguments)
}

/// How deeply `value` nests: a leaf reads 0, and each container adds one.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn depth(value: &Value) -> usize {
    match value {
        Value::Array(items) => items.iter().map(depth).max().unwrap_or(0) + 1,
        Value::Object(map) => map.values().map(depth).max().unwrap_or(0) + 1,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 0,
    }
}

/// The `host.model` request: the model reference with a slash, the
/// optional system string, the non-empty messages and the positive
/// token limit, with the same sentences as the Lua half.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn model_request(args: &[Value]) -> Result<contract::model::HostModelRequest, (ErrorCode, String)> {
    let opts = object_arg("host.model", args, 0, "opts")?;
    let model = match opts.get("model").unwrap_or(&Value::Null) {
        Value::String(model) if model.contains('/') => model.clone(),
        Value::String(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: `model` must be a model reference, not a role".to_owned(),
            ));
        }
    };
    let system = match opts.get("system").unwrap_or(&Value::Null) {
        Value::Null => None,
        Value::String(system) => Some(system.clone()),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: `system` must be a string".to_owned(),
            ));
        }
    };
    let messages = match opts.get("messages").unwrap_or(&Value::Null) {
        Value::Array(messages) if !messages.is_empty() => messages.clone(),
        Value::Array(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: `messages` must be a non-empty list".to_owned(),
            ));
        }
    };
    let mut request_messages = Vec::with_capacity(messages.len());
    for message in &messages {
        request_messages.push(model_message(message)?);
    }
    let max_tokens = match opts.get("max_tokens").unwrap_or(&Value::Null) {
        Value::Number(tokens) if tokens.as_u64().is_some_and(|tokens| tokens > 0) => {
            tokens.as_u64().unwrap_or(0)
        }
        Value::Number(_)
        | Value::Null
        | Value::Bool(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: `max_tokens` must be a positive integer".to_owned(),
            ));
        }
    };
    Ok(contract::model::HostModelRequest {
        model,
        system,
        messages: request_messages,
        max_tokens,
    })
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn model_message(message: &Value) -> Result<contract::model::HostMessage, (ErrorCode, String)> {
    const NOT_A_MESSAGE: &str = "host.model: `messages` must hold `{ role, content }` tables";
    let message = message
        .as_object()
        .ok_or((ErrorCode::InvalidArguments, NOT_A_MESSAGE.to_owned()))?;
    let role = match message.get("role").unwrap_or(&Value::Null) {
        Value::String(role) if role == "user" => contract::model::HostRole::User,
        Value::String(role) if role == "assistant" => contract::model::HostRole::Assistant,
        Value::String(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.model: a message `role` is `user` or `assistant`".to_owned(),
            ));
        }
    };
    let content = model_content(message.get("content").unwrap_or(&Value::Null))?;
    Ok(contract::model::HostMessage { role, content })
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn model_content(
    content: &Value,
) -> Result<Vec<contract::shapes::ContentPart>, (ErrorCode, String)> {
    match content {
        Value::String(text) => Ok(vec![contract::shapes::ContentPart::Text {
            text: text.clone(),
        }]),
        Value::Array(parts) => {
            let mut content_parts = Vec::with_capacity(parts.len());
            for part in parts {
                content_parts.push(model_part(part)?);
            }
            Ok(content_parts)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.model: a message `content` is a string or a list of parts".to_owned(),
        )),
    }
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn model_part(part: &Value) -> Result<contract::shapes::ContentPart, (ErrorCode, String)> {
    let part = part.as_object().ok_or((
        ErrorCode::InvalidArguments,
        "host.model: a content part is a table".to_owned(),
    ))?;
    match part.get("type").unwrap_or(&Value::Null) {
        Value::String(kind) if kind == "text" => match part.get("text") {
            Some(Value::String(text)) => {
                Ok(contract::shapes::ContentPart::Text { text: text.clone() })
            }
            _ => Err((
                ErrorCode::InvalidArguments,
                "host.model: a text part holds `text`, a string".to_owned(),
            )),
        },
        Value::String(kind) if kind == "image" || kind == "pdf" => {
            serde_json::from_value::<contract::shapes::ContentPart>(Value::Object(part.clone()))
                .map_err(|error| {
                    (
                        ErrorCode::InvalidArguments,
                        format!("host.model: a content part does not read: {error}"),
                    )
                })
        }
        Value::String(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::Array(_)
        | Value::Object(_) => Err((
            ErrorCode::InvalidArguments,
            "host.model: a content part `type` is `text`, `image` or `pdf`".to_owned(),
        )),
    }
}

/// The `host.delegate` call's parts: the spec arguments as a table with
/// the per-call timeout and the `run` function marker.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
type DelegateParts = (Map<String, Value>, std::time::Duration, bool);

/// The `host.delegate` call: the spec arguments as a table with the
/// per-call timeout and the `run` function marker.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn delegate_request(args: &[Value]) -> Result<DelegateParts, (ErrorCode, String)> {
    const CALL: &str = "host.delegate";
    // The Lua half snapshots the spec table: an empty table reads as `{}`,
    // which the service refuses for its missing fields, while a non-empty
    // list names no arguments.
    let spec = match arg(args, 0) {
        Value::Array(items) if items.is_empty() => Map::new(),
        Value::Object(spec) => spec.clone(),
        Value::Array(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.delegate: `spec` must be a table of named arguments".to_owned(),
            ));
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                format!("{CALL}: `spec` must be a table"),
            ));
        }
    };
    // The Lua half checks the options with the timeout, so missing
    // options read as the missing timeout.
    let opts = match arg(args, 1) {
        Value::Object(opts) => opts.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.delegate: `timeout` must be a whole number of milliseconds above 0"
                    .to_owned(),
            ));
        }
    };
    let timeout = match opts.get("timeout").unwrap_or(&Value::Null) {
        Value::Number(timeout) if timeout.as_u64().is_some_and(|timeout| timeout > 0) => {
            std::time::Duration::from_millis(timeout.as_u64().unwrap_or(0))
        }
        Value::Number(_)
        | Value::Null
        | Value::Bool(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.delegate: `timeout` must be a whole number of milliseconds above 0"
                    .to_owned(),
            ));
        }
    };
    if !matches!(opts.get("run").unwrap_or(&Value::Null), Value::Bool(true)) {
        return Err((
            ErrorCode::InvalidArguments,
            "host.delegate: `run` must be a function".to_owned(),
        ));
    }
    Ok((spec, timeout, true))
}

/// The `host.ask` interaction: the closed kind with its spec.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn ask_interaction(args: &[Value]) -> Result<contract::events::Interaction, (ErrorCode, String)> {
    let kind = match arg(args, 0) {
        Value::String(kind) => kind.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            return Err((
                ErrorCode::InvalidArguments,
                "host.ask: `kind` must be a string".to_owned(),
            ));
        }
    };
    let spec = arg(args, 1).clone();
    // `interaction` prefixes its own sentences with `host.ask: `, as the
    // Lua half's checker does.
    crate::lua::asks::interaction(&kind, &spec)
        .map_err(|message| (ErrorCode::InvalidArguments, message))
}

/// One `host.oauth.*` URL in text: `open` names the bare URL, `poll`
/// the backticked one, as their Lua halves do.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_url(args: &[Value], message: &str) -> Result<String, (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_port(args: &[Value]) -> Result<u16, (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_path(args: &[Value]) -> Result<Option<String>, (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_show(args: &[Value]) -> Result<(String, String), (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_method(args: &[Value]) -> Result<String, (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_headers(args: &[Value]) -> Result<Vec<(String, String)>, (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_body(args: &[Value]) -> Result<Option<Vec<u8>>, (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_pending(args: &[Value]) -> Result<Option<Vec<u16>>, (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn oauth_interval(args: &[Value]) -> Result<u64, (ErrorCode, String)> {
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
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn refresh_function(args: &[Value]) -> Result<(), (ErrorCode, String)> {
    if !matches!(arg(args, 0), Value::Bool(true)) {
        return Err((
            ErrorCode::InvalidArguments,
            "host.oauth.refresh: takes a function".to_owned(),
        ));
    }
    Ok(())
}

/// One `host.fs.*` path in text: the message names the call without
/// backticks, as the Lua half's shared sentence does.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn fs_path(call: &str, args: &[Value], index: usize) -> Result<String, (ErrorCode, String)> {
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
fn fs_contents(args: &[Value]) -> Result<Vec<u8>, (ErrorCode, String)> {
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
fn fs_lock(args: &[Value], index: usize) -> Result<bool, (ErrorCode, String)> {
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
fn config_key(call: &str, args: &[Value]) -> Result<String, (ErrorCode, String)> {
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
fn config_value(args: &[Value]) -> Result<Value, (ErrorCode, String)> {
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
fn config_scope(args: &[Value]) -> Result<super::context::ConfigScope, (ErrorCode, String)> {
    use super::context::ConfigScope::{Machine, Project};
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
fn secret_name(args: &[Value]) -> Result<String, (ErrorCode, String)> {
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
fn data_dir_scope(args: &[Value]) -> Result<super::context::DataScope, (ErrorCode, String)> {
    use super::context::DataScope::{Machine, Project};
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
fn widget_lines(args: &[Value]) -> Result<Vec<String>, (ErrorCode, String)> {
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
fn log_message(args: &[Value]) -> Result<String, (ErrorCode, String)> {
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
fn state_key(args: &[Value]) -> Result<String, (ErrorCode, String)> {
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
fn state_set(
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
