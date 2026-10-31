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
use std::path::Path;
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
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ("hub.log missing".to_owned(), String::new())
        }
        Err(error) => (format!("hub.log unreadable ({error})"), String::new()),
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

/// The hello durations attempted before a failure. Consecutive equal
/// attempts collapse to one `bound xcount`, keeping failure reports short.
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
    failing_wait_left: Duration,
    attempts: &[Duration],
) -> String {
    format!(
        "{entry}, failing root left {failing_left:?}, failing started-wait left {failing_wait_left:?} after hello attempts+{}",
        slices_shown(attempts)
    )
}

/// A hello duration that leaves room for one connect race and the report
/// before the test's root guard expires.
fn hello_bound(root_left: Duration) -> Option<Duration> {
    let hello_left = root_left.saturating_sub(doors::hub::CONNECT_DEADLINE + REPORT_MARGIN);
    (!hello_left.is_zero()).then_some(hello_left.min(MUST_SUCCEED_WITHIN))
}

type ConnectAttemptFn = fn(
    &Path,
    &mut dyn FnMut() -> std::io::Result<()>,
    &dyn Clock,
    Duration,
) -> std::io::Result<doors::hub::Hub>;

/// Runs one injected action/receive attempt. Production uses
/// `connect_within`, which starts the hello bound at acceptance, after its
/// connect race, rather than before both waits.
fn connect_attempt_with(
    attempt: ConnectAttemptFn,
    home: &Path,
    start: &mut dyn FnMut() -> std::io::Result<()>,
    clock: &dyn Clock,
    hello_within: Duration,
) -> std::io::Result<doors::hub::Hub> {
    attempt(home, start, clock, hello_within)
}

fn connect_attempt(
    home: &Path,
    start: &mut dyn FnMut() -> std::io::Result<()>,
    clock: &dyn Clock,
    hello_within: Duration,
) -> std::io::Result<doors::hub::Hub> {
    connect_attempt_with(doors::hub::connect_within, home, start, clock, hello_within)
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

/// One hello attempt, bounded by the test's root guard. The connector's
/// own connect race and hello receive each have their own bound. The outer
/// timeout is caught and feeds `give_up` with its text; other panics resume
/// unchanged, and a returned error goes back with the hub.
pub(crate) fn hello_slice(
    setup: &Setup,
    process: HubProc,
    wait: Deadline,
    bound: Duration,
    entry: &str,
    attempts: &[Duration],
) -> (std::io::Result<doors::hub::Hub>, HubProc) {
    let home = setup.home();
    let root = wait;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bounded(wait, "the installed hub's hello", move || {
            let current_bound = hello_bound(root.left()).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the test root leaves no room for another hello attempt",
                )
            })?;
            connect_attempt(
                &home,
                &mut || Ok(()),
                &SystemClock,
                bound.min(current_bound),
            )
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
                        hello_account(entry, setup.deadline.left(), wait.left(), attempts),
                    ),
                );
            }
            std::panic::resume_unwind(payload);
        }
    }
}

/// Waits for the hub's `hub_hello`, looking at the hub's exit between
/// bounded attempts. `connect_within` owns the connect race, then starts a
/// fresh hello bound at acceptance. The root deadline only caps the
/// total test work and reserves room for a race, the hello and the report.
fn await_hello(
    setup: &Setup,
    mut process: HubProc,
    helper_left: Duration,
) -> Result<(doors::hub::Hub, HubProc), (ExitStatus, String)> {
    let entry_left = setup.deadline.left();
    let entry = format!(
        "hello wait entered with helper root left {helper_left:?}, root left {entry_left:?}"
    );
    let mut attempts: Vec<Duration> = Vec::new();
    loop {
        if process.child.try_wait().unwrap().is_some() {
            let account = hello_account(
                &entry,
                setup.deadline.left(),
                setup.deadline.left(),
                &attempts,
            );
            return Err((process.wait(), account));
        }
        let failing_left = setup.deadline.left();
        let Some(bound) = hello_bound(failing_left) else {
            give_up(
                setup,
                process,
                &format!(
                    "waited for the installed hub's hello ({})",
                    hello_account(&entry, failing_left, failing_left, &attempts),
                ),
            );
        };
        attempts.push(bound);
        let (slice, back) = hello_slice(setup, process, setup.deadline, bound, &entry, &attempts);
        process = back;
        let error = match slice {
            Ok(hub) => return Ok((hub, process)),
            Err(error) => error,
        };
        if process.child.try_wait().unwrap().is_some() {
            let status = process.wait();
            let account = hello_account(
                &entry,
                setup.deadline.left(),
                setup.deadline.left(),
                &attempts,
            );
            return Err((status, account));
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
                    hello_account(
                        &entry,
                        setup.deadline.left(),
                        setup.deadline.left(),
                        &attempts,
                    ),
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
            Err((status, account)) => {
                let log = setup.hub_log();
                let added = log.get(logged..).unwrap_or_default();
                if attempt < attempts && port_taken(added, port) {
                    attempt += 1;
                    continue;
                }
                panic!(
                    "the installed hub's hello on 127.0.0.1:{port}, attempt {attempt} of {attempts} with helper root left {helper_left:?} and failing root left {:?} ({account}): the hub {}",
                    setup.deadline.left(),
                    report(setup, Some(status))
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::os::unix::net::UnixListener;
    use std::process::Command;
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::Duration;

    use contract::clock::Clock;
    use fakes::clock::{FakeClock, Miss};
    use support::clock::Sleep;

    use super::{
        connect_attempt, hello_bound, is_hello_timeout, report, serve_installed, slices_shown,
    };
    use crate::support::{Deadline, Setup};

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
    fn hello_bound_reserves_action_and_report_margin() {
        assert_eq!(
            hello_bound(Duration::from_secs(30)),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            hello_bound(doors::hub::CONNECT_DEADLINE + Duration::from_millis(250)),
            None
        );
        assert_eq!(
            hello_bound(
                doors::hub::CONNECT_DEADLINE + Duration::from_millis(250) + Duration::from_secs(3)
            ),
            Some(Duration::from_secs(3))
        );
    }

    /// A fake-clock action that consumes nearly the full connect race does
    /// not consume the hello receive's bound, which starts when the socket
    /// accepts. The worker is named and the clock advances only at its
    /// observed parks.
    #[test]
    fn connect_attempt_starts_hello_bound_after_the_action() {
        let setup = Setup::new();
        let home = setup.home().to_path_buf();
        fs::create_dir_all(home.join("run")).unwrap();
        let clock = FakeClock::with_sleep(Sleep::Waits);
        let origin = clock.origin();
        let hello_within = Duration::from_secs(10);
        let (id_tx, id_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let thread_clock = Arc::clone(&clock);
        let thread_home = home.clone();
        let worker = thread::Builder::new()
            .name("hub-start-hello-bound".to_owned())
            .spawn(move || {
                id_tx.send(thread::current().id()).unwrap();
                let result = connect_attempt(
                    &thread_home,
                    &mut || Ok(()),
                    thread_clock.as_ref(),
                    hello_within,
                );
                done_tx.send(result).unwrap();
            })
            .unwrap();
        let worker_id = Deadline::after(Duration::from_secs(5))
            .recv(&id_rx)
            .expect("the connector reports its worker id");
        let poll = Duration::from_millis(10);
        let action_deadline = Deadline::after(Duration::from_secs(30));
        let mut next_poll = origin + poll;
        for _ in 0..497 {
            clock.advance_when_parked(Some(next_poll), Some(worker_id), poll, &action_deadline);
            next_poll += poll;
        }
        assert!(
            clock
                .mark_parked(next_poll, Deadline::after(Duration::from_secs(5)))
                .is_some(),
            "the connector parks just before the race deadline"
        );

        let listener = UnixListener::bind(home.join("run").join("hub")).unwrap();
        listener.set_nonblocking(true).unwrap();
        let listener = Arc::new(listener);
        let accepted = Arc::new(Mutex::new(None));
        let poll_listener = Arc::clone(&listener);
        let poll_accepted = Arc::clone(&accepted);
        clock.advance_when_parked(Some(next_poll), Some(worker_id), poll, &action_deadline);
        fakes::wait_until(
            "the connector to reach the hub listener",
            Deadline::after(Duration::from_secs(5)),
            move || match poll_listener.accept() {
                Ok((stream, _)) => {
                    *poll_accepted.lock().unwrap() = Some(stream);
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(error) => panic!("accepting the connector: {error}"),
            },
        );
        // The accept happened at the fake clock's current instant. This is
        // the receive deadline expected from connect_within.
        let accepted_at = clock.now();
        let expected = accepted_at + hello_within;
        let observed = clock.try_advance_when_parked(
            Some(expected),
            Some(worker_id),
            Duration::from_secs(6),
            Duration::from_secs(5),
        );
        let (matched, actual) = match observed {
            Ok(_) => (true, None),
            Err(Miss::Mismatch {
                watched: (_, Some(until)),
                ..
            }) => (false, Some(until)),
            Err(Miss::Mismatch { .. } | Miss::NoPark) => (false, None),
        };
        if !matched {
            // Let the old shared deadline expire so the worker and socket
            // are cleaned up before asserting the regression failure.
            let until = actual.unwrap_or(expected);
            clock.advance_when_parked(
                Some(until),
                Some(worker_id),
                until.saturating_duration_since(clock.now()),
                &Deadline::after(Duration::from_secs(5)),
            );
        }
        let mut server = accepted.lock().unwrap().take().unwrap();
        if matched {
            server
                .write_all(
                    br#"{"kind":"hub_hello","ts":1,"schema_version":1,"payload":{"fiber_version":"0.0.0"}}"#,
                )
                .unwrap();
            server.write_all(b"\n").unwrap();
        }
        drop(server);
        let result = Deadline::after(Duration::from_secs(5))
            .recv_or_fail(&done_rx, "the connector's hello result");
        worker.join().unwrap();
        assert!(matched, "hello parks at {expected:?}, found {actual:?}");
        let (_, line) = result.expect("the connected hub answers its hello");
        assert_eq!(line.kind, "hub_hello");
    }

    /// A missing `hub.log` reports missing, while an unreadable one
    /// reports the error instead of misdiagnosing it as missing: a
    /// directory at the log path fails the read with a non-`NotFound`
    /// error on every platform.
    #[test]
    fn report_distinguishes_missing_from_unreadable_hub_log() {
        let setup = Setup::new();
        let missing = report(&setup, None);
        assert!(
            missing.contains("hub.log missing"),
            "a fresh home holds no hub.log: {missing}"
        );
        assert!(
            !missing.contains("unreadable"),
            "absence is not an error: {missing}"
        );
        fs::create_dir_all(setup.home().join("logs").join("hub.log")).unwrap();
        let unreadable = report(&setup, None);
        assert!(
            unreadable.contains("hub.log unreadable"),
            "a directory at the log path is not missing: {unreadable}"
        );
        assert!(
            !unreadable.contains("hub.log missing"),
            "an error reports distinctly from absence: {unreadable}"
        );
    }

    /// A hub that exits before saying hello reports root and per-attempt
    /// hello bounds alongside the exit, without a shared started-wait.
    #[test]
    fn serve_installed_retry_exhausted_reports_wait_accounting() {
        let setup = Setup::new();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            serve_installed(&setup, 1, &mut || 1, &mut |_| Command::new("true"))
        }));
        let message = outcome
            .err()
            .and_then(|payload| payload.downcast::<String>().ok().map(|boxed| *boxed))
            .expect("the exited hub exhausts its one attempt");
        assert!(
            message.contains("hello wait entered"),
            "the panic names the hello wait's entry bounds: {message}"
        );
        assert!(
            message.contains("helper root left"),
            "the panic names the helper root bound at entry: {message}"
        );
        assert!(
            message.contains("failing root left"),
            "the panic names the root bound at the failure: {message}"
        );
        assert!(
            message.contains("hello attempts+"),
            "the panic names the per-attempt hello bounds: {message}"
        );
        assert!(
            message.contains("exited"),
            "the panic names the hub's exit: {message}"
        );
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
