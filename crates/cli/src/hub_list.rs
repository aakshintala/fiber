//! A client's hub list and device tokens (`docs/invocation.md`, "Several hubs").
use crate::login::{KeyReader, NoEcho, Plain};
use crate::{failed, usage, with_dirs};
use config::{ConfigError, CredentialFile, CredentialLock, Secret};
use contract::{ErrorCode, clock::Clock, shapes::Failure};
use doors::hub::{Address, Hub};
use serde_json::Value;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

/// Adds a named hub, pairing websocket clients and optionally making it default.
pub fn hub_add(name: &str, address: &str, default: bool, clock: &dyn Clock) -> i32 {
    with_dirs(|home, _| {
        let mut input = io::stdin().lock();
        let mut err = io::stderr();
        let mut out = io::stdout();
        let mut keys: Box<dyn KeyReader> = if io::stdin().is_terminal() && err.is_terminal() {
            Box::new(NoEcho)
        } else {
            Box::new(Plain)
        };
        add_run(
            home,
            name,
            address,
            default,
            keys.as_mut(),
            &mut input,
            &mut err,
            &mut out,
            &mut |address| doors::hub::connect_unpaired(address, clock),
        )
    })
}

/// Removes a named hub, its device token and a matching default.
pub fn hub_remove(name: &str) -> i32 {
    with_dirs(|home, _| remove_run(home, name, &mut io::stdout()))
}

#[allow(
    clippy::too_many_arguments,
    reason = "the command's tested seam injects its streams, key reader and connection"
)]
fn add_run(
    home: &Path,
    name: &str,
    address: &str,
    default: bool,
    keys: &mut dyn KeyReader,
    input: &mut dyn BufRead,
    err: &mut dyn Write,
    out: &mut dyn Write,
    connect: &mut dyn FnMut(&Address) -> Result<Hub, doors::Error>,
) -> Result<(), Failure> {
    let _ = out;
    doors::hub::check_name(name).map_err(door_failure)?;
    let parsed = Address::parse(address).map_err(door_failure)?;
    let held = lock_name(home, name)?;
    if config::get_global(home, &format!("hubs.\"{name}\""))
        .map_err(crate::mcp::config_failure)?
        .is_some()
    {
        return Err(already_declared(name));
    }
    let paired = matches!(parsed, Address::Web(_));
    if paired {
        let code = keys
            .read_key("pairing code: ", input, err)
            .map_err(|e| failed(ErrorCode::IoFailed, format!("standard input: {e}")))?;
        if code.expose().is_empty() {
            return Err(usage(
                "No pairing code was given. Run `fiber --help` for usage.",
            ));
        }
        let (stream, _) = connect(&parsed).map_err(door_failure)?;
        let answer = crate::sessions::request(
            &mut io::BufReader::new(stream),
            "c_pair",
            "pair",
            serde_json::Map::from_iter([("code".into(), Value::String(code.expose().into()))]),
        )
        .map_err(|mut error| {
            error.message = error.message.replace(code.expose(), "[redacted]");
            error
        })?;
        let token = answer
            .get("token")
            .and_then(Value::as_str)
            .ok_or_else(|| failed(ErrorCode::IoFailed, "the hub's pairing answer has no token"))?;
        let token = Secret::new(token.into());
        if let Err(error) = config::store_credential(home, "hubs", name, &token) {
            delete_token(home, name, &held, err);
            return Err(crate::mcp::config_failure(error));
        }
    }
    let project = config::ProjectKey::new("hub-list").map_err(crate::mcp::config_failure)?;
    match config::insert_entry(
        home,
        home,
        &project,
        config::Layer::Global,
        "hubs",
        name,
        serde_json::json!({"address":address}),
    ) {
        Ok(true) => {}
        Ok(false) => {
            if paired {
                delete_token(home, name, &held, err);
            }
            return Err(already_declared(name));
        }
        Err(error) => {
            if paired {
                settle_token(home, name, &error, &held, err);
            }
            let message = if paired && matches!(error, ConfigError::Unsynced { .. }) {
                format!("{error} The device token the entry refers to was kept.")
            } else {
                error.to_string()
            };
            return Err(failed(error.code(), message));
        }
    }
    if default {
        config::replace_global(home, "hub.default", Some(Value::String(name.into())))
            .map_err(crate::mcp::config_failure)?;
    }
    Ok(())
}

fn remove_run(home: &Path, name: &str, out: &mut dyn Write) -> Result<(), Failure> {
    let _ = out;
    doors::hub::check_name(name).map_err(door_failure)?;
    let held = lock_name(home, name)?;
    let project = config::ProjectKey::new("hub-list").map_err(crate::mcp::config_failure)?;
    let removed = config::remove_entry(home, home, &project, config::Layer::Global, "hubs", name);
    let unsynced = match removed {
        Ok(true) => None,
        Ok(false) => {
            return Err(usage(format!(
                "No hub named `{name}` in this client's list. Run `fiber --help` for usage."
            )));
        }
        Err(error @ ConfigError::Unsynced { .. }) => Some(error),
        Err(error) => return Err(crate::mcp::config_failure(error)),
    };
    config::delete_credential_held(home, "hubs", name, &held)
        .map_err(crate::mcp::config_failure)?;
    config::clear_global_if_equals(home, "hub.default", &Value::String(name.into()))
        .map_err(crate::mcp::config_failure)?;
    if let Some(error) = unsynced {
        return Err(crate::mcp::config_failure(error));
    }
    Ok(())
}

fn settle_token(
    home: &Path,
    name: &str,
    error: &ConfigError,
    lock: &CredentialLock,
    err: &mut dyn Write,
) {
    if !matches!(error, ConfigError::Unsynced { .. }) {
        delete_token(home, name, lock, err);
    }
}

fn door_failure(error: doors::Error) -> Failure {
    failed(error.code(), error)
}

fn lock_name(home: &Path, name: &str) -> Result<CredentialLock, Failure> {
    CredentialFile::new(home, "hubs", name)
        .map_err(crate::mcp::config_failure)?
        .try_lock()
        .map_err(crate::mcp::config_failure)?
        .ok_or_else(|| {
            failed(
                ErrorCode::IoFailed,
                format!("another `fiber hub add` or `fiber hub remove` of `{name}` is running"),
            )
        })
}

fn already_declared(name: &str) -> Failure {
    usage(format!(
        "`{name}` is already declared in the global layer. Run `fiber hub remove {name}` to remove it first. Run `fiber --help` for usage."
    ))
}

fn delete_token(home: &Path, name: &str, held: &CredentialLock, err: &mut dyn Write) {
    if let Err(error) = config::delete_credential_held(home, "hubs", name, held) {
        // A failed rollback is reported in addition to the original failure.
        let _reported = writeln!(
            err,
            "fiber: could not remove the unused device token: {error}"
        );
    }
}

#[cfg(test)]
#[path = "hub_list_tests.rs"]
mod tests;
