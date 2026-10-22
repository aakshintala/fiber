//! Top-level `fiber stop`: shuts down every running session through
//! its own shutdown, then the hub exits (`docs/invocation.md`, "Stopping
//! everything").
//!
//! The local path never starts a hub to learn there is nothing to stop:
//! it probes with `doors` (no starter) and reuses the probed connection
//! for the stop exchange. When no hub answers, it lists `run/` entries
//! only, with no connect; session-id entries start a hub through the
//! passed-in client starter, and the started hub's own shutdown loop
//! probes each socket. A listed `--hub` connects through `connect_target`,
//! which never starts one either. The exchange always runs first: the
//! installed login service stops only after the hub answered, so the
//! manager never kills a hub with its sessions still running.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use contract::ErrorCode;
use contract::clock::Clock;
use contract::commands::hub::{HubCommand, HubCommandLine, StopArgs};
use contract::shapes::Failure;
use contract::{CommandId, HubLine};
use serde_json::Value;

use crate::hub_service::{
    ManagerRun, current_exe, manager, read_unit, stop_login_service, system_run_bounded,
};
use crate::hub_unit::Service;
use crate::{failed, finish, write_line};

/// The stop command's id, sent with the request and matched on the answer.
const STOP_ID: &str = "c_stop";

/// What the stop prints when nothing was running before it.
const NOTHING_RUNNING: &str = "Nothing running.";

/// The second line whenever the unit file was present: the login service
/// stays installed, so the hub comes back through it, not through the
/// socket just closed.
const INSTALLED_AGAIN: &str = "The hub starts again at the next login.";

/// One deadline for the whole stop: the probe, a client start, the
/// manager's calls and the stop exchange all fit inside it. The connect
/// and the status deadlines plus ten seconds keep a stop behind a status
/// on a loaded machine but inside every login manager's grace.
pub(crate) const STOP_WITHIN: Duration =
    Duration::from_secs(doors::hub::CONNECT_DEADLINE.as_secs() + 10);

/// What one answered stop exchange reports: the hub answered, so the
/// service step runs and the close is awaited either way. Only a transport
/// failure, where no answer arrives at all, skips the service step.
enum Answered {
    /// The hub accepted with its confirmed-closed count.
    Stopped(u64),
    /// The hub refused: the service still stops and the close is awaited,
    /// then this failure is what the command reports.
    Rejected(Failure),
}

/// `fiber stop` for this client's own hub: probes first without starting
/// one, starts one through `start` only when session sockets wait, then
/// stops the sessions and the hub over the connection. Never connects to a
/// session socket.
pub fn stop_local(
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
) -> i32 {
    // Only the stop path bounds its manager calls: install, uninstall
    // and restart keep `system_run` bit-identical.
    let run = |program: &str, args: &[String]| system_run_bounded(program, args, clock);
    finish(run_local(&mut io::stdout(), home, start, clock, &run))
}

/// `fiber stop --hub <name>`: stops the sessions and the hub process over
/// the given connection and waits for its close. No installed line: the
/// protocol has no remote manager access, so only the observed close is
/// reported.
pub fn stop_remote(
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
    clock: &dyn Clock,
) -> i32 {
    finish(run_remote(&mut io::stdout(), connect, clock))
}

/// The local stop over `out`: probes first and retains whether a hub
/// answered, then lists `run/` entries with no connect when none did, and
/// starts a hub through `start` only when session sockets wait. The stop
/// exchange always runs before the installed service stops.
pub(crate) fn run_local(
    out: &mut dyn Write,
    home: &Path,
    start: &mut dyn FnMut() -> io::Result<()>,
    clock: &dyn Clock,
    run: &ManagerRun<'_>,
) -> Result<(), Failure> {
    let service = Service::locate(manager(), home, &current_exe()?, &|key| {
        std::env::var_os(key)
    })?;
    let installed = read_unit(&service.unit)?.is_some();
    let deadline = clock.now() + STOP_WITHIN;
    match doors::hub::probe_until(home, clock, deadline) {
        doors::hub::Probe::Absent => {
            if session_entries(home)?.is_empty() {
                if installed {
                    stop_login_service(&service, run, clock)?;
                }
                return report(out, NOTHING_RUNNING, installed);
            }
            let (stream, _) =
                doors::hub::connect_until(home, start, clock, deadline, &mut || {})
                    .map_err(|error| crate::sessions::hub_failure(doors::Error::Local(error)))?;
            with_answer(
                out, &stream, deadline, clock, &service, run, installed, true,
            )
        }
        doors::hub::Probe::Failed(error) => {
            Err(failed(ErrorCode::IoFailed, format!("the hub: {error}")))
        }
        doors::hub::Probe::Connected(Err(error)) => {
            Err(failed(ErrorCode::IoFailed, format!("the hub: {error}")))
        }
        doors::hub::Probe::Connected(Ok((stream, _))) => with_answer(
            out, &stream, deadline, clock, &service, run, installed, false,
        ),
    }
}

/// The remote stop over `out`: sends `stop` with `sessions` over the given
/// connection and waits for its close.
pub(crate) fn run_remote(
    out: &mut dyn Write,
    connect: &mut dyn FnMut() -> Result<doors::hub::Hub, doors::Error>,
    clock: &dyn Clock,
) -> Result<(), Failure> {
    let (stream, _) = connect().map_err(crate::sessions::hub_failure)?;
    match exchange_stop(&stream, clock.now() + STOP_WITHIN, clock)? {
        Answered::Stopped(stopped) => report(
            out,
            &format!("Stopped {stopped} sessions and the hub."),
            false,
        ),
        Answered::Rejected(rejection) => Err(rejection),
    }
}

/// The stop after the exchange answered: the installed service stops when
/// the unit file is present, then the close the exchange already awaited
/// is reported. A started hub that stopped nothing reports nothing
/// running, like the empty path.
#[allow(
    clippy::too_many_arguments,
    reason = "one call of the answered stop: output, connection, deadline, clock, service, stop, install and start"
)]
fn with_answer(
    out: &mut dyn Write,
    stream: &UnixStream,
    deadline: Instant,
    clock: &dyn Clock,
    service: &Service,
    run: &ManagerRun<'_>,
    installed: bool,
    started: bool,
) -> Result<(), Failure> {
    match exchange_stop(stream, deadline, clock)? {
        Answered::Stopped(0) if started => {
            if installed {
                stop_login_service(service, run, clock)?;
            }
            report(out, NOTHING_RUNNING, installed)
        }
        Answered::Stopped(stopped) => {
            if installed {
                stop_login_service(service, run, clock)?;
            }
            report(
                out,
                &format!("Stopped {stopped} sessions and the hub."),
                installed,
            )
        }
        Answered::Rejected(rejection) => {
            if installed {
                stop_login_service(service, run, clock)?;
            }
            Err(rejection)
        }
    }
}

/// Prints the stop's outcome: its first line, then the installed line
/// whenever the unit file was present.
fn report(out: &mut dyn Write, first: &str, installed: bool) -> Result<(), Failure> {
    write_line(out, first)?;
    if installed {
        write_line(out, INSTALLED_AGAIN)?;
    }
    Ok(())
}

/// The session ids with a socket entry under `home`'s `run/`, with no
/// connect: a missing `run/` is no entries.
fn session_entries(home: &Path) -> Result<Vec<contract::SessionId>, Failure> {
    doors::session_entries(home)
        .map_err(|error| crate::sessions::hub_failure(doors::Error::Local(error)))
}

/// Sends `stop` with `sessions` and waits for the answer and the close
/// through the shared exchange: the answer says the hub accepted the stop,
/// the end after it is the hub's exit, which is the stop. A rejection is
/// kept as the answer, so the caller still runs the service step and the
/// close wait before reporting it.
fn exchange_stop(
    stream: &UnixStream,
    deadline: Instant,
    clock: &dyn Clock,
) -> Result<Answered, Failure> {
    let mut request = serde_json::to_vec(&HubCommandLine {
        id: CommandId(STOP_ID.to_owned()),
        command: HubCommand::Stop(StopArgs {
            sessions: Some(true),
        }),
    })
    .map_err(|error| failed(ErrorCode::IoFailed, format!("the hub: {error}")))?;
    request.push(b'\n');
    // The refusal already carries the stop's failure: the exchange maps
    // only the transport.
    let result = crate::hub_env::exchange(
        stream,
        &request,
        deadline,
        clock,
        true,
        &mut || {},
        &mut || {},
        || {
            failed(
                ErrorCode::IoFailed,
                "the hub closed the connection before answering stop",
            )
        },
        late,
        lost,
        |line: &HubLine| -> Result<Option<Answered>, Failure> {
            let Some(answered) = crate::sessions::answer_of(line, STOP_ID, "stop") else {
                return Ok(None);
            };
            match answered {
                Ok(result) => Ok(Some(Answered::Stopped(sessions_stopped(&result)?))),
                Err(rejection) => Ok(Some(Answered::Rejected(rejection))),
            }
        },
    );
    let restore = crate::hub_env::restore_blocking(stream);
    match (result, restore) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(failed(ErrorCode::IoFailed, format!("the hub: {error}"))),
        (Ok(answered), Ok(())) => Ok(answered),
    }
}

/// The confirmed-closed count in an accepted stop answer.
fn sessions_stopped(result: &Value) -> Result<u64, Failure> {
    result
        .get("sessions_stopped")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            failed(
                ErrorCode::IoFailed,
                "the hub answered stop without a session count",
            )
        })
}

/// The stop exchange's deadline failure.
fn late() -> Failure {
    failed(
        ErrorCode::IoFailed,
        format!("the hub did not stop in {} s", STOP_WITHIN.as_secs()),
    )
}

/// An I/O failure on the hub's socket.
fn lost(error: &io::Error) -> Failure {
    failed(ErrorCode::IoFailed, format!("the hub: {error}"))
}

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
