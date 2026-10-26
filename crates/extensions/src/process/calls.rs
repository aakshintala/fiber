//! Routing one process `call` (`docs/extensions.md`, "Process
//! extensions").
//!
//! A `call` is a registration, a `host` call or a `state` call. A name the
//! Lua side registers with — `fiber.*` — registers here with the same
//! validation and the same wording; `host.drive` sends a driver command
//! through the `Drive` seam. Every other name dispatches through the
//! injected host services, which own the host and state call matrix.

use serde_json::{Map, Value};

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
