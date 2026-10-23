//! `fiber hub refresh` (`docs/invocation.md`, "Commands and flags"):
//! rebuilds the hub's environment for the sessions it starts from now on,
//! then prints whether the login-shell capture succeeded. It reaches the
//! hub through `connect`, starting one when none runs.

use std::io::{self, BufReader, Write};

use contract::commands::hub::{HubCommand, NoArgs};
use serde_json::Value;

use crate::sessions::request_value;
use crate::{CliError, finish, write_line};

/// The `id` of the `refresh` command `fiber hub refresh` sends.
const REFRESH_ID: &str = "c_refresh";

/// The `id` of the `status` command it reads the new environment from.
const STATUS_ID: &str = "c_status";

/// `fiber hub refresh` through the hub `connect` reaches, starting one
/// when none runs. Exits 0 when both commands are accepted; a rejection
/// prints `fiber: <message>` and exits 1, as `sessions delete` does.
pub fn hub_refresh(connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>) -> i32 {
    finish(run(&mut io::stdout(), connect))
}

/// Sends `refresh`, then `status`, and prints one line for the new
/// environment on `out`: what sessions started from now on get.
pub(crate) fn run(
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> Result<(), CliError> {
    let (stream, _) = connect().map_err(CliError::Hub)?;
    let mut read = BufReader::new(stream);
    request_value(&mut read, REFRESH_ID, HubCommand::Refresh(NoArgs {}))?;
    let result = request_value(&mut read, STATUS_ID, HubCommand::Status(NoArgs {}))?;
    let source = result
        .get("variables")
        .and_then(|variables| variables.get("source"))
        .and_then(Value::as_str)
        .ok_or(CliError::StatusNoVariables)?;
    // The hub answers only its own capture's source: anything else is a
    // reply that does not fit `status`.
    let line = if source == "login_shell" {
        "sessions now start with the login shell's environment"
    } else if source == "inherited" {
        "the login-shell capture failed; sessions start with the hub's inherited environment"
    } else {
        return Err(CliError::StatusNoVariables);
    };
    write_line(out, line)?;
    Ok(())
}

#[cfg(test)]
#[path = "hub_refresh_tests.rs"]
mod tests;
