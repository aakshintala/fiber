//! Paired device tables and revocation (`docs/invocation.md`, "Remote clients").
use crate::{
    CliError, finish, sessions::request_value, sessions_search::utc, table::pad, write_line,
};
use contract::commands::hub::{Device, HubCommand, NoArgs};
use serde_json::Value;
use std::io::{self, BufReader, Write};

/// Lists the paired devices through the hub's local socket.
pub fn hub_token_list(connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>) -> i32 {
    finish(list(&mut io::stdout(), connect))
}
/// Revokes a device and prints its name once the hub accepts the change.
pub fn hub_token_revoke(
    device: &str,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> i32 {
    finish(revoke(&mut io::stdout(), device, connect))
}
fn list(
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> Result<(), CliError> {
    let (stream, _) = connect().map_err(CliError::Hub)?;
    let result = request_value(
        &mut BufReader::new(stream),
        "c_devices",
        HubCommand::Devices(NoArgs {}),
    )?;
    let malformed = || CliError::DevicesMalformed;
    let devices = result
        .get("devices")
        .and_then(Value::as_array)
        .ok_or_else(malformed)?;
    let mut rows = vec![vec![
        "name".to_owned(),
        "paired".to_owned(),
        "last connected".to_owned(),
    ]];
    for device in devices {
        let name = device
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(malformed)?;
        let time = |key| -> Result<String, CliError> {
            match device.get(key) {
                None => Ok("-".to_owned()),
                Some(value) => Ok(utc(value.as_u64().ok_or_else(malformed)? / 1000)),
            }
        };
        rows.push(vec![
            name.to_owned(),
            time("paired_at")?,
            time("last_connected_at")?,
        ]);
    }
    for row in pad(&rows) {
        write_line(out, &row)?;
    }
    Ok(())
}
fn revoke(
    out: &mut dyn Write,
    device: &str,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
) -> Result<(), CliError> {
    let (stream, _) = connect().map_err(CliError::Hub)?;
    request_value(
        &mut BufReader::new(stream),
        "c_revoke",
        HubCommand::Revoke(Device {
            device: device.to_owned(),
        }),
    )?;
    write_line(out, device)?;
    Ok(())
}
#[cfg(test)]
#[path = "hub_token_tests.rs"]
mod tests;
