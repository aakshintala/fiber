//! A client's hub list and device tokens (`docs/invocation.md`, "Several hubs").
use crate::login::{KeyReader, NoEcho, Plain};
use crate::{CliError, with_dirs};
use config::{ConfigError, CredentialFile, CredentialLock, Secret};
use contract::clock::Clock;
use contract::commands::hub::{HubCommand, Pair};
use doors::hub::{Address, Hub};
use serde_json::Value;
use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;

/// Adds a named hub, pairing websocket clients and optionally making it default.
pub fn hub_add(name: &str, address: &str, default: bool, clock: &dyn Clock) -> i32 {
    with_dirs(|home, _| {
        let mut input = io::stdin().lock();
        let mut err = io::stderr();

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
            &mut |address| doors::hub::connect_unpaired(address, clock),
        )
    })
}

/// Removes a named hub, its device token and a matching default.
pub fn hub_remove(name: &str) -> i32 {
    with_dirs(|home, _| remove_run(home, name))
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
    connect: &mut dyn FnMut(&Address) -> Result<Hub, doors::Error>,
) -> Result<(), CliError> {
    doors::hub::check_name(name).map_err(CliError::Hub)?;
    let parsed = Address::parse(address).map_err(CliError::Hub)?;
    let held = lock_name(home, name)?;
    if config::get_global(home, &format!("hubs.\"{name}\""))
        .map_err(CliError::Config)?
        .is_some()
    {
        return Err(CliError::HubAlreadyDeclared {
            name: name.to_owned(),
        });
    }
    let paired = matches!(parsed, Address::Web(_));
    if paired {
        let code = keys
            .read_key("pairing code: ", input, err)
            .map_err(CliError::StandardInput)?;
        if code.expose().is_empty() {
            return Err(CliError::HubPairingCodeMissing);
        }
        let (stream, _) = connect(&parsed).map_err(CliError::Hub)?;
        let answer = crate::sessions::request(
            &mut io::BufReader::new(stream),
            "c_pair",
            HubCommand::Pair(Pair { code: code.clone() }),
        )
        .map_err(|mut error| {
            error.message = error.message.replace(code.expose(), "[redacted]");
            CliError::Forwarded(error)
        })?;
        let token = answer
            .get("token")
            .and_then(Value::as_str)
            .ok_or(CliError::HubPairingTokenMissing)?;
        let token = Secret::new(token.into());
        if let Err(error) = config::store_credential(home, "hubs", name, &token) {
            delete_token(home, name, &held, err);
            return Err(CliError::Config(error));
        }
    }
    let project = config::ProjectKey::new("hub-list").map_err(CliError::Config)?;
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
            return Err(CliError::HubAlreadyDeclared {
                name: name.to_owned(),
            });
        }
        Err(error) => {
            if paired {
                settle_token(home, name, &error, &held, err);
            }
            return Err(entry_failure(error, paired));
        }
    }
    if default {
        config::replace_global(home, "hub.default", Some(Value::String(name.into())))
            .map_err(CliError::Config)?;
    }
    Ok(())
}

fn remove_run(home: &Path, name: &str) -> Result<(), CliError> {
    doors::hub::check_name(name).map_err(CliError::Hub)?;
    let held = lock_name(home, name)?;
    let project = config::ProjectKey::new("hub-list").map_err(CliError::Config)?;
    let removed = config::remove_entry(home, home, &project, config::Layer::Global, "hubs", name);
    let unsynced = match removed {
        Ok(true) => None,
        Ok(false) => {
            return Err(CliError::HubNotFound {
                name: name.to_owned(),
            });
        }
        Err(error @ ConfigError::Unsynced { .. }) => Some(error),
        Err(error) => return Err(CliError::Config(error)),
    };
    config::delete_credential_held(home, "hubs", name, &held).map_err(CliError::Config)?;
    config::clear_global_if_equals(home, "hub.default", &Value::String(name.into()))
        .map_err(CliError::Config)?;
    if let Some(error) = unsynced {
        return Err(CliError::Config(error));
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

fn entry_failure(error: ConfigError, paired: bool) -> CliError {
    CliError::HubEntryFailed {
        source: error,
        paired,
    }
}

fn lock_name(home: &Path, name: &str) -> Result<CredentialLock, CliError> {
    CredentialFile::new(home, "hubs", name)
        .map_err(CliError::Config)?
        .try_lock()
        .map_err(CliError::Config)?
        .ok_or_else(|| CliError::HubListLocked {
            name: name.to_owned(),
        })
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
