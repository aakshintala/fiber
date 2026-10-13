//! Paired device tables and revocation (`docs/invocation.md`, "Remote clients").
use crate::{failed, finish, sessions::request, sessions_search::utc, table::pad, write_line};
use contract::{ErrorCode, shapes::Failure};
use serde_json::{Map, Value};
use std::io::{self, BufReader, Write};

/// Lists the paired devices through the hub's local socket.
pub fn hub_token_list(connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>) -> i32 {
    finish(list(&mut io::stdout(), connect))
}
/// Revokes a device and prints its name once the hub accepts the change.
pub fn hub_token_revoke(
    device: &str,
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> i32 {
    finish(revoke(&mut io::stdout(), device, connect))
}
fn list(
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> Result<(), Failure> {
    let (stream, _) =
        connect().map_err(|e| failed(ErrorCode::IoFailed, format!("the hub: {e}")))?;
    let result = request(
        &mut BufReader::new(stream),
        "c_devices",
        "devices",
        Map::new(),
    )?;
    let malformed = || {
        failed(
            ErrorCode::IoFailed,
            "the hub's answer to `devices` is not a device list",
        )
    };
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
        let time = |key| -> Result<String, Failure> {
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
    connect: &mut dyn FnMut() -> io::Result<doors::hub::Hub>,
) -> Result<(), Failure> {
    let (stream, _) =
        connect().map_err(|e| failed(ErrorCode::IoFailed, format!("the hub: {e}")))?;
    request(
        &mut BufReader::new(stream),
        "c_revoke",
        "revoke",
        Map::from_iter([("device".to_owned(), Value::from(device))]),
    )?;
    write_line(out, device)
}
#[cfg(test)]
#[path = "hub_token_tests.rs"]
mod tests;
