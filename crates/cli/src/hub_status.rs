//! `fiber hub status [--json]` (`docs/invocation.md`, "The hub"): whether
//! this machine's hub is running, its version, its port, the other
//! connected clients, the paired devices, and whether its login service is
//! installed. It never starts a hub, and the whole question has one
//! deadline on the injected clock.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::shapes::Failure;
use contract::{ErrorCode, HubLine};
use serde::Serialize;
use serde_json::Value;

use crate::hub_service::manager;
use crate::hub_unit::{Manager, name, unit_path};
use crate::{fail, failed};

/// The `id` of the one command `fiber hub status` sends.
const STATUS_ID: &str = "c_status";

/// What `fiber hub status` reports. `--json` prints it as is, its fields in
/// this order.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct Status {
    pub(crate) running: bool,
    pub(crate) version: Option<String>,
    pub(crate) port: Option<u16>,
    /// The connected clients other than the one asking.
    pub(crate) clients: u64,
    pub(crate) devices: Vec<String>,
    pub(crate) installed: bool,
}

/// `fiber hub status [--json]` for this Fiber home. Exits 0 whether or not
/// a hub runs.
pub fn hub_status(json: bool, clock: &dyn Clock) -> i32 {
    let ran = config::fiber_home_from_env()
        .map_err(|e| failed(e.code(), e))
        .and_then(|home| {
            let installed = installed(manager(), &home, &|key| std::env::var_os(key))?;
            let status = probe(&home, clock, doors::hub::CONNECT_DEADLINE, installed)?;
            let text = if json {
                render_json(&status)?
            } else {
                render_text(&status)
            };
            writeln!(io::stdout(), "{text}")
                .map_err(|e| failed(ErrorCode::IoFailed, format!("standard output: {e}")))
        });
    ran.map_or_else(fail, |()| 0)
}

/// Whether `home`'s login service has its unit file.
pub(crate) fn installed(
    manager: Manager,
    home: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<bool, Failure> {
    let unit = unit_path(manager, &name(home)?, var)?;
    unit.try_exists().map_err(|e| {
        failed(
            ErrorCode::IoFailed,
            format!("reading {}: {e}", unit.display()),
        )
    })
}

/// Asks `home`'s hub for its status, all within `answer_within` on `clock`;
/// a hub that is absent or refuses is not running.
pub(crate) fn probe(
    home: &Path,
    clock: &dyn Clock,
    answer_within: Duration,
    installed: bool,
) -> Result<Status, Failure> {
    probe_with(home, clock, answer_within, installed, &mut || {})
}

/// [`probe`], running `before_read` just before each read of the answer.
fn probe_with(
    home: &Path,
    clock: &dyn Clock,
    answer_within: Duration,
    installed: bool,
    before_read: &mut dyn FnMut(),
) -> Result<Status, Failure> {
    let port = port(home)?;
    let devices = devices(home)?;
    let deadline = clock.now() + answer_within;
    let mut absent = false;
    let connected = {
        let mut start = || {
            absent = true;
            Err(io::Error::new(io::ErrorKind::NotFound, "no hub runs"))
        };
        doors::hub::connect_within(home, &mut start, clock, answer_within)
    };
    let stream = match connected {
        Ok((stream, _)) => stream,
        Err(_) if absent => {
            return Ok(Status {
                running: false,
                version: None,
                port,
                clients: 0,
                devices,
                installed,
            });
        }
        Err(e) => return Err(failed(ErrorCode::IoFailed, format!("the hub: {e}"))),
    };
    let (version, clients) = ask(&stream, deadline, answer_within, clock, before_read)?;
    Ok(Status {
        running: true,
        version: Some(version),
        port,
        // The hub counts the asker too.
        clients: clients.saturating_sub(1),
        devices,
        installed,
    })
}

/// The global `hub.port`; a project's value is never read.
fn port(home: &Path) -> Result<Option<u16>, Failure> {
    let Some(value) = config::get_global(home, "hub.port").map_err(|e| failed(e.code(), e))? else {
        return Ok(None);
    };
    value
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .map(Some)
        .ok_or_else(|| {
            failed(
                ErrorCode::ConfigInvalid,
                format!(
                    "hub.port in {} is not a port: {value}",
                    home.join("config.json").display()
                ),
            )
        })
}

/// The sorted names of the regular files in `hub/devices/`; none when it is
/// absent.
fn devices(home: &Path) -> Result<Vec<String>, Failure> {
    let dir = home.join("hub/devices");
    let lost = |e: io::Error| {
        failed(
            ErrorCode::IoFailed,
            format!("reading {}: {e}", dir.display()),
        )
    };
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(lost(e)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(lost)?;
        if entry.file_type().map_err(lost)?.is_file() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Sends `status` and reads lines until its answer, failing once
/// `deadline` passes on `clock`: before each read the stream's timeout is
/// the time left, so unrelated lines, trickled bytes and silence all end
/// there.
fn ask(
    mut stream: &UnixStream,
    deadline: Instant,
    within: Duration,
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
) -> Result<(String, u64), Failure> {
    let lost = |e: io::Error| failed(ErrorCode::IoFailed, format!("the hub: {e}"));
    let late = || {
        failed(
            ErrorCode::IoFailed,
            format!(
                "the hub did not answer status in {} s",
                within.as_secs_f64()
            ),
        )
    };
    let request = format!("{{\"id\":\"{STATUS_ID}\",\"command\":\"status\"}}\n");
    stream.write_all(request.as_bytes()).map_err(lost)?;
    let mut pending = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let left = deadline
            .checked_duration_since(clock.now())
            .filter(|left| !left.is_zero())
            .ok_or_else(late)?;
        set_read_timeout(stream, Some(left)).map_err(lost)?;
        before_read();
        let read = match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(failed(
                    ErrorCode::IoFailed,
                    "the hub closed the connection before answering status",
                ));
            }
            Ok(read) => read,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(late());
            }
            Err(e) => return Err(lost(e)),
        };
        pending.extend_from_slice(chunk.get(..read).unwrap_or_default());
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if let Some(answer) = answer(&line)? {
                return Ok(answer);
            }
        }
    }
}

/// The version and client count from the hub's answer to `status`; `None`
/// for any other line.
fn answer(line: &[u8]) -> Result<Option<(String, u64)>, Failure> {
    let Ok(line) = serde_json::from_slice::<HubLine>(line) else {
        return Ok(None);
    };
    let field = |key: &str| line.payload.get(key).and_then(Value::as_str);
    if field("command_id") != Some(STATUS_ID) {
        return Ok(None);
    }
    match line.kind.as_str() {
        "command_accepted" => {
            let result = line.payload.get("result");
            let version = result
                .and_then(|result| result.get("fiber_version"))
                .and_then(Value::as_str);
            let clients = result
                .and_then(|result| result.get("clients"))
                .and_then(Value::as_u64);
            match (version, clients) {
                (Some(version), Some(clients)) => Ok(Some((version.to_owned(), clients))),
                (Some(_) | None, Some(_) | None) => Err(failed(
                    ErrorCode::IoFailed,
                    "the hub's answer to status has no fiber_version or clients",
                )),
            }
        }
        "command_rejected" => {
            let code = field("code")
                .and_then(|code| serde_json::from_value(Value::String(code.to_owned())).ok())
                .unwrap_or(ErrorCode::IoFailed);
            let message = field("message").unwrap_or("the hub refused status");
            Err(failed(code, message))
        }
        _ => Ok(None),
    }
}

/// Sets `stream`'s read timeout. `InvalidInput` is ignored: macOS refuses
/// the option once the peer has closed, while its buffered bytes stay
/// readable and a read of the closed socket does not block.
fn set_read_timeout(stream: &UnixStream, timeout: Option<Duration>) -> io::Result<()> {
    match stream.set_read_timeout(timeout) {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    }
}

/// One `key: value` line per field.
pub(crate) fn render_text(status: &Status) -> String {
    let yes = |flag: bool| if flag { "yes" } else { "no" };
    let devices = if status.devices.is_empty() {
        "none".to_owned()
    } else {
        status.devices.join(", ")
    };
    format!(
        "running: {}\nversion: {}\nport: {}\nclients: {}\ndevices: {devices}\ninstalled: {}",
        yes(status.running),
        status.version.as_deref().unwrap_or("none"),
        status
            .port
            .map_or_else(|| "none".to_owned(), |port| port.to_string()),
        status.clients,
        yes(status.installed),
    )
}

/// One JSON line.
pub(crate) fn render_json(status: &Status) -> Result<String, Failure> {
    serde_json::to_string(status)
        .map_err(|e| failed(ErrorCode::IoFailed, format!("the hub's status: {e}")))
}

#[cfg(test)]
#[path = "hub_status_tests.rs"]
mod tests;
