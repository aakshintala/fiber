//! Serves an installed hub on a picked port, retrying a taken one with a
//! fresh port, as `hub_remote_terminal.rs` already does: a port another
//! test process took between the pick and the hub's bind fails that hub
//! at once, so the next attempt serves on a fresh port.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::net::TcpListener;
use std::process::{Command, ExitStatus};
use std::time::Duration;

use contract::clock::Clock;
use fakes::MUST_SUCCEED_WITHIN;

use crate::support::{Deadline, HubProc, Setup, SystemClock, bounded};

/// Hub start attempts `serve_installed` makes, each on a fresh port.
/// What a hello wait leaves of the deadline for its report and cleanup.
pub(crate) const ATTEMPTS: usize = 5;
const REPORT_MARGIN: Duration = Duration::from_millis(250);
/// The `hub.log` lines a failure report shows.
const LOG_TAIL: usize = 20;

/// A port nothing holds right now: bound on port 0, read, released.
pub(crate) fn free_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// Whether `log` holds the hub's failure to bind `port`: the line its start
/// error writes before the hub exits (`127.0.0.1:<port>: <why>`).
fn port_taken(log: &str, port: u16) -> bool {
    log.contains(&format!("127.0.0.1:{port}: "))
}

/// What a hub that did not say hello left behind: its exit `status`
/// (`None` for a hub still running when the wait gave up) and the state
/// of `hub.log`. A missing file reports distinctly from an empty one: the
/// sightings share an alive hub with no lines, and only the distinction
/// keeps that observation provable.
fn report(setup: &Setup, status: Option<ExitStatus>) -> String {
    let path = setup.home().join("logs").join("hub.log");
    let (detail, tail) = match fs::read(&path) {
        Err(_) => ("hub.log missing".to_owned(), String::new()),
        Ok(bytes) if bytes.is_empty() => ("hub.log empty (0 bytes)".to_owned(), String::new()),
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            (
                format!("hub.log {} bytes", bytes.len()),
                lines[lines.len().saturating_sub(LOG_TAIL)..].join("\n"),
            )
        }
    };
    let state = match status {
        Some(status) => format!("exited ({status})"),
        None => "was still running".to_owned(),
    };
    format!("{state}; {detail}; hub.log tail (last {LOG_TAIL} lines):\n{tail}")
}

/// Stops waiting on `process`: snapshots its status, reaps it (killing a
/// hub still running), and panics with `why` and the report.
fn give_up(setup: &Setup, mut process: HubProc, why: &str) -> ! {
    let status = process.child.try_wait().unwrap();
    match status {
        Some(_) => drop(process.wait()),
        None => drop(process.kill_and_wait()),
    }
    panic!("{why}: the hub {}", report(setup, status));
}

/// The bounds a hello failure observed, without inferring a cause: the
/// wait's entry bounds (`entry`, made when the wait started), the root
/// and started-wait bounds remaining at the failure, and each slice's
/// `until` offset from its start (`now+`). The sightings share a loaded
/// hub and an exhausted guard; these values say which wait ran out.
/// The slice `until` offsets compactly: consecutive equal bounds collapse
/// to one `bound xcount`, so the thousands of zero-length slices behind
/// an exhausted wait stay a short line under `flake_report`'s cap.
fn slices_shown(slices: &[Duration]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut rest = slices.iter().peekable();
    while let Some(first) = rest.next() {
        let mut count = 1usize;
        while rest.peek().is_some_and(|next| *next == first) {
            rest.next();
            count += 1;
        }
        if count == 1 {
            parts.push(format!("{first:?}"));
        } else {
            parts.push(format!("{first:?} x{count}"));
        }
    }
    format!("[{}]", parts.join(", "))
}

fn hello_account(
    entry: &str,
    failing_left: Duration,
    failing_started_left: Duration,
    slices: &[Duration],
) -> String {
    format!(
        "{entry}, failing root left {failing_left:?}, failing started-wait left {failing_started_left:?} after slices until now+{}",
        slices_shown(slices)
    )
}

/// A caught panic's text, when it carries one: `within` panics with a
/// `String`, like the outer timeout the diagnostic path drives.
fn hello_panic_text(payload: &Box<dyn std::any::Any + Send>) -> Option<String> {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
}

/// Whether `text` is the outer [`bounded`] wait's own timeout for this
/// hello: `within` panics `waited {left:?} for {what}` on expiry, while a
/// panic inside the work reads `the wait for {what} panicked: ...` and a
/// thread that never started reads `could not start a thread for {what}`.
/// Only the timeout takes the enriched `give_up` path; anything else
/// resumes unchanged. The middle names the bound the slice died with,
/// such as `0ns`, and carries no spaces.
fn is_hello_timeout(text: &str) -> bool {
    text.strip_prefix("waited ")
        .and_then(|rest| rest.strip_suffix(" for the installed hub's hello"))
        .is_some_and(|middle| !middle.is_empty() && !middle.contains(' '))
}

/// One hello attempt through the started wait `wait`: the far end gets
/// `bound` past now to answer, while [`bounded`] caps the attempt through
/// the same wait. The outer wait's own timeout is caught and feeds
/// `give_up` with its text, so the hub is snapshotted and reaped before
/// the enriched panic; any other panic resumes unchanged, and any
/// returned error goes back with the hub.
pub(crate) fn hello_slice(
    setup: &Setup,
    process: HubProc,
    wait: Deadline,
    bound: Duration,
    entry: &str,
    slices: &[Duration],
) -> (std::io::Result<doors::hub::Hub>, HubProc) {
    let home = setup.home();
    let until = SystemClock.now() + bound;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bounded(wait, "the installed hub's hello", move || {
            doors::hub::connect_until(&home, &mut || Ok(()), &SystemClock, until, &mut || {})
        })
    }));
    match outcome {
        Ok(slice) => (slice, process),
        Err(payload) => {
            if let Some(text) = hello_panic_text(&payload)
                && is_hello_timeout(&text)
            {
                give_up(
                    setup,
                    process,
                    &format!(
                        "{text} ({})",
                        hello_account(entry, setup.deadline.left(), wait.left(), slices),
                    ),
                );
            }
            std::panic::resume_unwind(payload);
        }
    }
}

/// Waits for the hub's `hub_hello` in slices of
/// [`MUST_SUCCEED_WITHIN`], looking at the hub's exit between them. `Ok`
/// is the hello and the live hub; `Err` is the status of a hub that
/// exited, already reaped. The wait starts at entry, never renewed: a
/// slice ends [`REPORT_MARGIN`] before it, while the give-up keeps its
/// margin against the test's guard, so the report and the cleanup run
/// first. Every failure names the bounds it observed and what the hub had
/// written, and leaves the cause open.
fn await_hello(
    setup: &Setup,
    mut process: HubProc,
    helper_left: Duration,
) -> Result<(doors::hub::Hub, HubProc), ExitStatus> {
    let entry_left = setup.deadline.left();
    let wait = setup.deadline.start_wait();
    let entry = format!(
        "hello wait entered with helper root left {helper_left:?}, root left {entry_left:?} and started-wait left {:?}",
        wait.left()
    );
    let mut slices: Vec<Duration> = Vec::new();
    loop {
        let failing_left = setup.deadline.left();
        if failing_left <= REPORT_MARGIN {
            give_up(
                setup,
                process,
                &format!(
                    "waited for the installed hub's hello ({})",
                    hello_account(&entry, failing_left, wait.left(), &slices),
                ),
            );
        }
        let bound = wait
            .left()
            .saturating_sub(REPORT_MARGIN)
            .min(MUST_SUCCEED_WITHIN);
        slices.push(bound);
        let (slice, back) = hello_slice(setup, process, wait, bound, &entry, &slices);
        process = back;
        let error = match slice {
            Ok(hub) => return Ok((hub, process)),
            Err(error) => error,
        };
        if process.child.try_wait().unwrap().is_some() {
            return Err(process.wait());
        }
        // Only a slice's own end loops; any other error would repeat at once.
        if !matches!(
            error.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::UnexpectedEof
        ) {
            give_up(
                setup,
                process,
                &format!(
                    "the installed hub's hello failed with {error} ({})",
                    hello_account(&entry, setup.deadline.left(), wait.left(), &slices),
                ),
            );
        }
    }
}

/// A hub that said hello, and the live hub behind it.
pub(crate) struct Served {
    pub(crate) port: u16,
    pub(crate) hub: doors::hub::Hub,
    pub(crate) process: HubProc,
}

/// Serves the installed hub, drawing each attempt's port from `ports` and
/// building its command with `serve` (which writes `config.json` for that
/// port and returns the `hub serve --installed` command). A hub that exits
/// on a port its log names as taken is retried on a fresh port, up to
/// `attempts` times; any other failure panics naming the port and the log.
pub(crate) fn serve_installed(
    setup: &Setup,
    attempts: usize,
    ports: &mut dyn FnMut() -> u16,
    serve: &mut dyn FnMut(u16) -> Command,
) -> Served {
    let helper_left = setup.deadline.left();
    let mut attempt = 1;
    loop {
        let port = ports();
        let logged = setup.hub_log().len();
        let mut command = serve(port);
        let process = HubProc::spawn_command(setup.deadline, &mut command);
        match await_hello(setup, process, helper_left) {
            Ok((hub, process)) => return Served { port, hub, process },
            Err(status) => {
                let log = setup.hub_log();
                let added = log.get(logged..).unwrap_or_default();
                if attempt < attempts && port_taken(added, port) {
                    attempt += 1;
                    continue;
                }
                panic!(
                    "the installed hub's hello on 127.0.0.1:{port}, attempt {attempt} of {attempts} with helper root left {helper_left:?} and failing root left {:?}: the hub {}",
                    setup.deadline.left(),
                    report(setup, Some(status))
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{is_hello_timeout, slices_shown};

    #[test]
    fn slices_shown_collapses_runs_and_keeps_singles() {
        assert_eq!(
            slices_shown(&[
                Duration::from_secs(10),
                Duration::from_secs(10),
                Duration::ZERO,
                Duration::ZERO,
                Duration::ZERO,
            ]),
            "[10s x2, 0ns x3]"
        );
        assert_eq!(slices_shown(&[]), "[]");
    }

    #[test]
    fn hello_timeout_names_the_bound_the_slice_died_with() {
        assert!(is_hello_timeout("waited 0ns for the installed hub's hello"));
        assert!(is_hello_timeout(
            "waited 39.75s for the installed hub's hello"
        ));
    }

    #[test]
    fn hello_timeout_rejects_panics_from_inside_the_work() {
        assert!(!is_hello_timeout(
            "the wait for the installed hub's hello panicked: boom"
        ));
        assert!(!is_hello_timeout(
            "could not start a thread for the installed hub's hello: boom"
        ));
    }

    #[test]
    fn hello_timeout_rejects_the_guard_text() {
        assert!(!is_hello_timeout("waited for the installed hub's hello"));
        assert!(!is_hello_timeout(
            "waited for the installed hub's hello (hello wait entered with helper root left 40s)"
        ));
        assert!(!is_hello_timeout("waited  for the installed hub's hello"));
        assert!(!is_hello_timeout(
            "waited a while for the installed hub's hello"
        ));
    }
}
