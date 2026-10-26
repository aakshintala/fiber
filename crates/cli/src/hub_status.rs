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
use contract::commands::hub::{HubCommand, HubCommandLine, NoArgs};
use contract::events::Variables;
use contract::shapes::Failure;
use contract::{CommandId, HubLine};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fd::{AsFd, BorrowedFd};
use serde::Serialize;
use serde_json::Value;

use crate::hub_service::manager;
use crate::hub_unit::{Manager, name, unit_path};
use crate::{CliError, finish, write_line};

/// The `id` of the one command `fiber hub status` sends.
const STATUS_ID: &str = "c_status";

/// How long `fiber hub status` waits for the answer: the connect deadline
/// plus the login-shell capture's 10-second limit, since a hub in its
/// first seconds answers `status` only after its first capture
/// (`docs/invocation.md`, "A session's environment"). `cli` declares it
/// because `cli` may not depend on `hub`.
pub(crate) const STATUS_WITHIN: Duration =
    Duration::from_secs(doors::hub::CONNECT_DEADLINE.as_secs() + 10);

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
        .map_err(CliError::Config)
        .and_then(|home| {
            let installed = installed(manager(), &home, &|key| std::env::var_os(key))?;
            let status = probe(&home, clock, STATUS_WITHIN, installed)?;
            let text = if json {
                render_json(&status)?
            } else {
                render_text(&status)
            };
            write_line(&mut io::stdout(), &text).map_err(CliError::Forwarded)
        });
    finish(ran)
}

/// Whether `home`'s login service has its unit file.
pub(crate) fn installed(
    manager: Manager,
    home: &Path,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<bool, CliError> {
    let unit = unit_path(manager, &name(home)?, var)?;
    unit.try_exists().map_err(|e| CliError::HubDevicesRead {
        path: unit.clone(),
        source: e,
    })
}

/// Asks `home`'s hub for its status, all within `answer_within` on `clock`;
/// a hub that is absent or refuses is not running.
pub(crate) fn probe(
    home: &Path,
    clock: &dyn Clock,
    answer_within: Duration,
    installed: bool,
) -> Result<Status, CliError> {
    probe_with(
        home,
        clock,
        answer_within,
        installed,
        &mut || {},
        &mut || {},
    )
}

/// Asks `home`'s hub for its status, all within `answer_within` on `clock`;
/// a hub that is absent or refuses is not running. The connect, the
/// handshake and the answer share one absolute deadline, so an EOF retry
/// gets only what remains. `before_read` runs just before each answer
/// read, and `before_wait` after each wait's deadline check, just before
/// [`Clock::wait_until`].
fn probe_with(
    home: &Path,
    clock: &dyn Clock,
    answer_within: Duration,
    installed: bool,
    before_read: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Result<Status, CliError> {
    let port = port(home)?;
    let devices = devices(home)?;
    let reply = match diagnose_with(home, clock, answer_within, before_read, before_wait) {
        Diagnostic::Absent => {
            return Ok(Status {
                running: false,
                version: None,
                port,
                clients: 0,
                devices,
                installed,
            });
        }
        Diagnostic::Connected(reply) => reply?,
        Diagnostic::Failed(failure) => return Err(CliError::Forwarded(failure)),
    };
    Ok(Status {
        running: true,
        version: Some(reply.version),
        port,
        // The hub counts the asker too.
        clients: reply.clients.saturating_sub(1),
        devices,
        installed,
    })
}

/// What one exchange with `home`'s hub found, without starting one.
/// `port`/`devices` validation stays with the status probe, not here: an
/// unreadable device list must not make a live hub look absent.
pub(crate) enum Diagnostic {
    /// No socket, or nobody listens: no hub runs.
    Absent,
    /// A connection was accepted: the status exchange's result, whose
    /// variables may still be missing or malformed.
    Connected(Result<Reply, Failure>),
    /// Anything before a connection: the error.
    Failed(Failure),
}

/// The hub's answer to `status`: its version, its client count including
/// the asker, and its environment without values. A rejection or a lost
/// connection is the exchange's `Err`, never this struct.
pub(crate) struct Reply {
    pub(crate) version: String,
    pub(crate) clients: u64,
    pub(crate) variables: Result<Variables, String>,
}

/// Whether `home`'s hub answers `status`, over one connection within
/// `answer_within` on `clock`, starting nothing: an absent socket is
/// [`Diagnostic::Absent`]. The connect, the handshake and the answer
/// share one absolute deadline, so an EOF retry gets only what remains.
pub(crate) fn diagnose(home: &Path, clock: &dyn Clock, answer_within: Duration) -> Diagnostic {
    diagnose_with(home, clock, answer_within, &mut || {}, &mut || {})
}

/// [`diagnose`] with hooks for tests: `before_read` runs just before
/// each answer read, and `before_wait` after each wait's deadline check,
/// just before [`Clock::wait_until`].
fn diagnose_with(
    home: &Path,
    clock: &dyn Clock,
    answer_within: Duration,
    before_read: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Diagnostic {
    // A `FIBER_HOME` too long for the hub socket names its cause and fix
    // instead of the connect's raw operating-system error.
    if let Err(failure) = doors::check_socket_path(home, "hub").map_err(Failure::from) {
        return Diagnostic::Failed(CliError::Forwarded(failure).into());
    }
    let deadline = clock.now() + answer_within;
    let connected = doors::hub::probe_until(home, clock, deadline);
    let stream = match connected {
        doors::hub::Probe::Absent => return Diagnostic::Absent,
        doors::hub::Probe::Failed(error) => {
            return Diagnostic::Failed(CliError::Hub(doors::Error::Local(error)).into());
        }
        doors::hub::Probe::Connected(Err(error)) => {
            return Diagnostic::Connected(Err(CliError::Hub(doors::Error::Local(error)).into()));
        }
        doors::hub::Probe::Connected(Ok((stream, _))) => stream,
    };
    match ask(
        &stream,
        deadline,
        answer_within,
        clock,
        before_read,
        before_wait,
    ) {
        Ok(reply) => Diagnostic::Connected(Ok(reply)),
        Err(error) => Diagnostic::Connected(Err(Failure::from(error))),
    }
}

/// The global `hub.port`; a project's value is never read.
fn port(home: &Path) -> Result<Option<u16>, CliError> {
    let Some(value) = config::get_global(home, "hub.port").map_err(CliError::Config)? else {
        return Ok(None);
    };
    value
        .as_u64()
        .and_then(|port| u16::try_from(port).ok())
        .map(Some)
        .ok_or_else(|| CliError::HubPortInvalid {
            value: value.clone(),
            path: config::global_config_file(home),
        })
}

/// The sorted names of the regular files in `hub/devices/`; none when it is
/// absent.
fn devices(home: &Path) -> Result<Vec<String>, CliError> {
    let dir = home.join("hub/devices");
    let lost = |e: io::Error| CliError::HubDevicesRead {
        path: dir.clone(),
        source: e,
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
/// `deadline` passes on `clock`: the write and every read are
/// non-blocking under the same deadline, so unrelated lines, trickled
/// bytes and silence all end there. `before_read` runs just before each
/// answer read; `before_wait` runs after each wait's deadline check, just
/// before [`Clock::wait_until`]. The stream comes back blocking with no
/// read timeout, as it arrived.
// Exempted from mutation testing: the InvalidInput arm is macOS-only (Linux never refuses the restore) and mutants run on Linux, so no test can reach its guard mutants; the macOS job still runs an_answer_followed_by_a_close_is_read.
#[cfg_attr(false, mutants::skip)]
fn ask(
    stream: &UnixStream,
    deadline: Instant,
    within: Duration,
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Result<Reply, CliError> {
    let result = exchange(stream, deadline, within, clock, before_read, before_wait);
    // macOS refuses the option once the peer has closed, while its
    // buffered bytes stay readable; ignoring that refusal keeps a
    // close-after-answer a reply.
    let restore = match stream.set_nonblocking(false) {
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => Ok(()),
        other => other,
    };
    match result {
        // The exchange's own error wins over a restore failure.
        Err(exchange) => Err(exchange),
        Ok(reply) => restore
            .map(|()| reply)
            .map_err(|error| CliError::Hub(doors::Error::Local(error))),
    }
}

/// The exchange `ask` wraps: it sends `status` and reads lines until its
/// answer, failing once `deadline` passes on `clock`. It leaves the
/// stream's mode alone; `ask` restores blocking after.
fn exchange(
    mut stream: &UnixStream,
    deadline: Instant,
    within: Duration,
    clock: &dyn Clock,
    before_read: &mut dyn FnMut(),
    before_wait: &mut dyn FnMut(),
) -> Result<Reply, CliError> {
    let lost = |e: io::Error| CliError::Hub(doors::Error::Local(e));
    let late = || CliError::HubStatusLate {
        seconds: within.as_secs_f64(),
    };
    stream.set_nonblocking(true).map_err(lost)?;
    let mut request = serde_json::to_vec(&HubCommandLine {
        id: CommandId(STATUS_ID.to_owned()),
        command: HubCommand::Status(NoArgs {}),
    })
    .map_err(CliError::HubRequestEncode)?;
    request.push(b'\n');
    let mut sent = 0;
    while sent < request.len() {
        match stream.write(request.get(sent..).unwrap_or_default()) {
            Ok(0) => {
                return Err(CliError::HubStatusClosed);
            }
            Ok(wrote) => sent += wrote,
            Err(error) => {
                // `!=`, not a `WouldBlock` match guard: the guard's
                // always-wait mutant no test can reach on a connected
                // socket, while this form's flipped comparison fails the
                // send-buffer test below.
                if error.kind() != io::ErrorKind::WouldBlock {
                    return Err(lost(error));
                }
                if clock.now() >= deadline {
                    return Err(late());
                }
                before_wait();
                wait_clock(clock, deadline, stream.as_fd(), PollFlags::OUT);
            }
        }
    }
    let mut pending = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if clock.now() >= deadline {
            return Err(late());
        }
        before_read();
        match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(CliError::HubStatusClosed);
            }
            Ok(read) => {
                pending.extend_from_slice(chunk.get(..read).unwrap_or_default());
            }
            Err(error) => {
                // `!=`, not a `WouldBlock` match guard: the guard's
                // always-wait mutant no test can reach on a connected
                // socket, while this form's flipped comparison fails the
                // silent-hub deadline tests.
                if error.kind() != io::ErrorKind::WouldBlock {
                    return Err(lost(error));
                }
                if clock.now() >= deadline {
                    return Err(late());
                }
                before_wait();
                wait_clock(clock, deadline, stream.as_fd(), PollFlags::IN);
            }
        }
        while let Some(end) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=end).collect();
            if let Some(reply) = answer(&line)? {
                return Ok(reply);
            }
        }
    }
}

/// Parks `clock` until `until`, waiting on `fd` for real while parked:
/// the full bound on a real clock, a short slice on a fake one, which the
/// caller re-checks against the fake deadline after.
fn wait_clock(clock: &dyn Clock, until: Instant, fd: BorrowedFd, events: PollFlags) {
    clock.wait_until(Some(until), &mut |bound| {
        let wait = bound.or(Some(Duration::from_millis(10)));
        let spec = wait.map(|wait| Timespec {
            tv_sec: wait.as_secs().min(i64::MAX as u64).cast_signed(),
            tv_nsec: wait.subsec_nanos() as _,
        });
        let mut fds = [PollFd::new(&fd, events)];
        // The park itself is the wait: a signal delivery or a woken fd
        // both end it, so the count is discarded.
        #[allow(
            clippy::let_underscore_must_use,
            reason = "a woken fd and an expired wait both end the park: the count is unused"
        )]
        let _ = poll(&mut fds, spec.as_ref());
    });
}

/// The version, client count and environment from the hub's answer to
/// `status`; `None` for any other line.
fn answer(line: &[u8]) -> Result<Option<Reply>, CliError> {
    let Ok(line) = serde_json::from_slice::<HubLine>(line) else {
        return Ok(None);
    };
    let Some(result) = crate::sessions::answer_of(&line, STATUS_ID, "status") else {
        return Ok(None);
    };
    let result = result.map_err(CliError::Forwarded)?;
    let version = result.get("fiber_version").and_then(Value::as_str);
    let clients = result.get("clients").and_then(Value::as_u64);
    let variables = match result.get("variables") {
        None => Err("the hub's answer to status has no variables".to_owned()),
        Some(variables) => serde_json::from_value::<Variables>(variables.clone())
            .map_err(|_| "the hub's answer to status has malformed variables".to_owned()),
    };
    match (version, clients) {
        (Some(version), Some(clients)) => Ok(Some(Reply {
            version: version.to_owned(),
            clients,
            variables,
        })),
        (Some(_) | None, Some(_) | None) => Err(CliError::HubStatusAnswerMissing),
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
pub(crate) fn render_json(status: &Status) -> Result<String, CliError> {
    serde_json::to_string(status).map_err(CliError::HubStatusJson)
}

#[cfg(test)]
#[path = "hub_status_tests.rs"]
mod tests;
