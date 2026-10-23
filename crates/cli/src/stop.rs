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

use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use contract::clock::Clock;
use contract::commands::hub::{HubCommand, HubCommandLine, StopArgs};
use contract::shapes::Failure;
use contract::{CommandId, HubLine};
use rustix::event::PollFlags;
use serde_json::Value;

use crate::hub_service::{
    ManagerRun, current_exe, manager, read_unit, stop_login_service, system_run_bounded,
};
use crate::hub_unit::Service;
use crate::{CliError, finish, write_line};

/// The stop command's id, sent with the request and matched on the answer.
const STOP_ID: &str = "c_stop";

/// What the stop prints when nothing was running before it.
const NOTHING_RUNNING: &str = "Nothing running.";

/// The second line whenever the unit file was present: the login service
/// stays installed, so the hub comes back through it, not through the
/// socket just closed.
const INSTALLED_AGAIN: &str = "The hub starts again at the next login or on fiber hub install.";

/// One deadline for the whole stop: the probe, a client start, the
/// manager's calls and the stop exchange all fit inside it. The connect
/// and the status deadlines plus ten seconds keep a stop behind a status
/// on a loaded machine but inside every login manager's grace.
pub(crate) const STOP_WITHIN: Duration =
    Duration::from_secs(doors::hub::CONNECT_DEADLINE.as_secs() + 10);

/// What one answered stop exchange reports: the hub answered, so the
/// service step runs and the close is awaited after it. Only a transport
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
) -> Result<(), CliError> {
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
            let (stream, _) = doors::hub::connect_until(home, start, clock, deadline, &mut || {})
                .map_err(|error| CliError::Hub(doors::Error::Local(error)))?;
            with_answer(
                out, &stream, deadline, clock, &service, run, installed, true,
            )
        }
        doors::hub::Probe::Failed(error) | doors::hub::Probe::Connected(Err(error)) => {
            Err(CliError::Hub(doors::Error::Local(error)))
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
) -> Result<(), CliError> {
    let (stream, _) = connect().map_err(CliError::Hub)?;
    // No service step remotely: the close still follows the answer, and
    // a stalled one never replaces a survivor rejection.
    let deadline = clock.now() + STOP_WITHIN;
    match exchange_stop(&stream, deadline, clock)? {
        Answered::Stopped(stopped) => {
            wait_for_close(&stream, deadline, clock)?;
            report(
                out,
                &format!("Stopped {stopped} sessions and the hub."),
                false,
            )
        }
        Answered::Rejected(rejection) => {
            // A stalled close never replaces the survivor rejection.
            #[allow(
                clippy::let_underscore_must_use,
                reason = "a stalled close never replaces the survivor rejection"
            )]
            let _ = wait_for_close(&stream, deadline, clock);
            Err(rejection.into())
        }
    }
}

#[cfg(test)]
thread_local! {
    /// The stop case arms it, and `with_answer` runs it right after the
    /// exchange collects the answer, before the service step below. The
    /// log it appends to proves the answer came first, on this same
    /// thread: the exchange and the service step never leave it.
    static STOP_ANSWERED: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
}

/// Arms the one-shot answer hook before the next answered stop.
/// Test only: production has no seam there.
#[cfg(test)]
pub(crate) fn on_stop_answered(hook: impl FnOnce() + 'static) {
    STOP_ANSWERED.with(|slot| slot.set(Some(Box::new(hook))));
}

/// The stop after the exchange answered: the installed service stops when
/// the unit file is present, then the close is awaited. A service failure
/// never replaces the answer: it is reported after the tail below, and a
/// stalled close never replaces either. A started hub that stopped nothing
/// reports nothing running, like the empty path.
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
) -> Result<(), CliError> {
    // The answer first: either answer runs the service step below.
    // Only a transport failure, where no answer arrives, returns
    // before it.
    let answered = exchange_stop(stream, deadline, clock)?;
    // Tests only: the ordering cases log the collected answer ahead of
    // the service step below, on this same thread.
    #[cfg(test)]
    STOP_ANSWERED.with(|slot| {
        if let Some(hook) = slot.take() {
            hook();
        }
    });
    // Every answer runs the same tail: the service step, then the close
    // wait, then what the mark says. Only a transport failure, where no
    // answer arrives, returns before it.
    enum AfterAnswer {
        /// Report the line; a close failure fails the stop.
        Report(String),
        /// Keep the rejection; a stalled close never replaces it.
        Keep(Failure),
    }
    let marked = match answered {
        Answered::Stopped(0) if started => AfterAnswer::Report(NOTHING_RUNNING.to_owned()),
        Answered::Stopped(stopped) => {
            AfterAnswer::Report(format!("Stopped {stopped} sessions and the hub."))
        }
        Answered::Rejected(rejection) => AfterAnswer::Keep(rejection),
    };
    // A partial stop finishes every step it can: the service error is
    // kept aside and the tail below always runs, so a manager failure
    // never swallows the survivor rejection and never skips the close.
    let service_error = if installed {
        stop_login_service(service, run, clock).err()
    } else {
        None
    };
    match marked {
        AfterAnswer::Report(first) => {
            wait_for_close(stream, deadline, clock)?;
            report(out, &first, installed)?;
            if let Some(error) = service_error {
                return Err(error);
            }
            Ok(())
        }
        AfterAnswer::Keep(rejection) => {
            // A stalled close never replaces the survivor rejection.
            #[allow(
                clippy::let_underscore_must_use,
                reason = "a stalled close never replaces the survivor rejection"
            )]
            let _ = wait_for_close(stream, deadline, clock);
            if let Some(error) = service_error {
                // The command's result is the rejection below; the
                // manager failure is reported additionally, the way
                // every failure prints.
                writeln!(io::stderr(), "fiber: {error}").unwrap_or(());
            }
            Err(rejection.into())
        }
    }
}

/// Prints the stop's outcome: its first line, then the installed line
/// whenever the unit file was present.
fn report(out: &mut dyn Write, first: &str, installed: bool) -> Result<(), CliError> {
    write_line(out, first)?;
    if installed {
        write_line(out, INSTALLED_AGAIN)?;
    }
    Ok(())
}

/// The session ids with a socket entry under `home`'s `run/`, with no
/// connect: a missing `run/` is no entries.
fn session_entries(home: &Path) -> Result<Vec<contract::SessionId>, CliError> {
    doors::session_entries(home).map_err(|error| CliError::Hub(doors::Error::Local(error)))
}

/// Sends `stop` with `sessions` and waits for the answer through the
/// shared exchange: the answer says the hub accepted or refused the stop.
/// The close after it is awaited separately, after the service step, so a
/// stalled close never skips it. A rejection is kept as the answer, so the
/// caller still runs the service step and the close wait before reporting
/// it.
fn exchange_stop(
    stream: &UnixStream,
    deadline: Instant,
    clock: &dyn Clock,
) -> Result<Answered, CliError> {
    let mut request = serde_json::to_vec(&HubCommandLine {
        id: CommandId(STOP_ID.to_owned()),
        command: HubCommand::Stop(StopArgs {
            sessions: Some(true),
        }),
    })
    .map_err(CliError::HubRequestEncode)?;
    request.push(b'\n');
    // The refusal already carries the stop's failure: the exchange maps
    // only the transport. The close is not awaited here: it follows the
    // service step in the caller.
    let result = crate::hub_env::exchange(
        stream,
        &request,
        deadline,
        clock,
        &mut || {},
        &mut || {},
        || CliError::HubClosedBeforeAnswer,
        late,
        lost,
        |line: &HubLine| -> Result<Option<Answered>, CliError> {
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
        (Ok(_), Err(error)) => Err(CliError::Hub(doors::Error::Local(error))),
        (Ok(answered), Ok(())) => Ok(answered),
    }
}

/// Waits for the hub's close after the stop answer: the end after the
/// answer is the hub's exit, which is the stop. Anything still arriving
/// drains; the bound ends a stalled close.
fn wait_for_close(
    mut stream: &UnixStream,
    deadline: Instant,
    clock: &dyn Clock,
) -> Result<(), CliError> {
    stream.set_nonblocking(true).map_err(|error| lost(&error))?;
    let result = loop {
        if clock.now() >= deadline {
            break Err(late());
        }
        let mut chunk = [0u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => break Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if clock.now() >= deadline {
                    break Err(late());
                }
                crate::hub_env::wait_clock(clock, deadline, stream.as_fd(), PollFlags::IN);
            }
            Err(error) => break Err(lost(&error)),
        }
    };
    let restore = crate::hub_env::restore_blocking(stream);
    match (result, restore) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(CliError::Hub(doors::Error::Local(error))),
        (Ok(()), Ok(())) => Ok(()),
    }
}

/// The confirmed-closed count in an accepted stop answer.
fn sessions_stopped(result: &Value) -> Result<u64, CliError> {
    result
        .get("sessions_stopped")
        .and_then(Value::as_u64)
        .ok_or(CliError::StopCountMissing)
}

/// The stop exchange's deadline failure.
fn late() -> CliError {
    CliError::HubStopLate {
        secs: STOP_WITHIN.as_secs(),
    }
}

/// An I/O failure on the hub's socket. The exchange lends the error, so it
/// is rebuilt with the same kind and text.
fn lost(error: &io::Error) -> CliError {
    CliError::Hub(doors::Error::Local(io::Error::new(
        error.kind(),
        error.to_string(),
    )))
}

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
