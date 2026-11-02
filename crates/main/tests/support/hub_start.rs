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

use crate::support::{CapturedStderr, Deadline, HubProc, Setup, SystemClock, bounded};

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

/// Chars one new capture section may add to a give-up report: the flake
/// report stays under its cap while the head keeps its entry bounds.
const CAPTURE_CHARS: usize = 1500;

/// Cuts `text` to [`CAPTURE_CHARS`] chars, marking the cut with `...`, so
/// a long capture never pushes the report's head out.
fn truncate_section(text: &str) -> String {
    if text.chars().count() <= CAPTURE_CHARS {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(CAPTURE_CHARS).collect();
    cut.push_str("...");
    cut
}

/// The hub child's captured stderr, or the marker for nothing captured
/// yet: a hub still running when the wait gave up may simply not have
/// flushed, while an exited hub's pipes drained at EOF. Absence reads as
/// its own marker, never as empty output.
fn stderr_section(snapshot: &str, status: Option<ExitStatus>) -> String {
    if snapshot.is_empty() {
        return match status {
            None => format!(
                "hub stderr (live snapshot, last {LOG_TAIL} lines):\n(still running: no stderr captured yet)"
            ),
            Some(_) => {
                format!("hub stderr (live snapshot, last {LOG_TAIL} lines):\n(no stderr captured)")
            }
        };
    }
    let lines: Vec<&str> = snapshot.lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_TAIL)..].join("\n");
    format!(
        "hub stderr (live snapshot, last {LOG_TAIL} lines):\n{}",
        truncate_section(&tail)
    )
}

/// The home's resolved state and log directories: paths only, never file
/// contents, so no secret or token can ride along. Capped like every other
/// capture section, so a long valid home cannot push the report's head out.
fn dirs_section(home: &Path) -> String {
    truncate_section(&format!(
        "state dir {}, log dir {}",
        home.join("run").display(),
        home.join("logs").display()
    ))
}

/// Per-thread park state for `pid` from `/proc/<pid>/task`: each thread's
/// `wchan` and `stack`, which hold kernel symbols only. Every read error
/// becomes text, never a panic: a missing task directory means the pid is
/// gone, and a thread vanishing mid-read names itself. An exited but
/// unreaped hub keeps its thread listed with a `0` wchan, so the zombie
/// reads from `/proc/<pid>/stat` instead: only a stat read observes it,
/// since `try_wait` itself reaps.
#[cfg(target_os = "linux")]
fn hub_threads(pid: u32) -> String {
    let mut tids: Vec<u32> = Vec::new();
    match fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Ok(tid) = entry.file_name().to_string_lossy().parse::<u32>() {
                    tids.push(tid);
                }
            }
        }
        Err(error) => return format!("hub threads for pid {pid}:\n(pid gone: {error})"),
    }
    tids.sort_unstable();
    let zombie = fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .is_some_and(|stat| {
            stat.rsplit_once(')')
                .is_some_and(|(_, tail)| tail.split_whitespace().next() == Some("Z"))
        });
    if zombie {
        return format!(
            "hub threads for pid {pid}:\n(no live threads: zombie, exited but unreaped)"
        );
    }
    let mut section = format!("hub threads for pid {pid}:");
    for tid in tids {
        let wchan = match fs::read_to_string(format!("/proc/{pid}/task/{tid}/wchan")) {
            Ok(wchan) => wchan.trim().to_owned(),
            Err(error) => format!("unreadable ({error})"),
        };
        let stack = match fs::read_to_string(format!("/proc/{pid}/task/{tid}/stack")) {
            Ok(stack) => stack,
            Err(error) => format!("(stack unreadable: {error})"),
        };
        section.push_str(&format!("\ntask {tid} wchan {wchan}\n{stack}"));
    }
    truncate_section(&section)
}

/// The hub's park state from `sample` on `pid`, bounded under
/// [`MUST_SUCCEED_WITHIN`](fakes::MUST_SUCCEED_WITHIN) through
/// [`fakes::wait_output`](fakes::wait_output), which kills the sample
/// child's own process group and reaps it on every path. A missed deadline
/// or output error prints as text and never replaces the timeout panic.
#[cfg(target_os = "macos")]
fn hub_threads(pid: u32) -> String {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;
    let mut command = Command::new("sample");
    command
        .arg(format!("{pid}"))
        .arg("1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return format!("hub sample for pid {pid}:\n(sample not started: {error})");
        }
    };
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        fakes::wait_output(child, "sample the hub")
    }));
    match outcome {
        Ok(output) => {
            let text = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = text.lines().collect();
            let tail = lines[lines.len().saturating_sub(LOG_TAIL)..].join("\n");
            if tail.trim().is_empty() {
                format!("hub sample for pid {pid}:\n(sample printed nothing)")
            } else {
                format!("hub sample for pid {pid}:\n{}", truncate_section(&tail))
            }
        }
        Err(payload) => format!(
            "hub sample for pid {pid}:\n(sample failed: {})",
            hello_panic_text(&payload).unwrap_or_else(|| "a non-string panic".to_owned())
        ),
    }
}

/// Thread capture where neither `/proc` nor `sample` applies: absence
/// reads as its own marker, never as empty output.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn hub_threads(pid: u32) -> String {
    format!("hub threads for pid {pid}:\n(thread capture unsupported on this platform)")
}

/// Thread section for a hub that already exited and was reaped before any
/// capture could read it: absence reads as its own marker, never as empty
/// output.
const EXITED_THREADS: &str = "(the hub had exited before thread capture: no live thread state)";

/// What a hub that did not say hello left behind: its exit `status`
/// (`None` for a hub still running when the wait gave up) and the state
/// of `hub.log`. A missing file reports distinctly from an empty one: the
/// sightings share an alive hub with no lines, and only the distinction
/// keeps that observation provable. The hub child's captured `stderr`, the
/// resolved directories, and the live `threads` capture follow, each
/// truncated, so the report's head is never pushed out.
fn report(setup: &Setup, status: Option<ExitStatus>, stderr: &str, threads: &str) -> String {
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
    format!(
        "{state}; {detail}; hub.log tail (last {LOG_TAIL} lines):\n{tail}\n{}\n{}\n{threads}",
        stderr_section(stderr, status),
        dirs_section(&setup.home())
    )
}

/// Stops waiting on `process`: captures the hub's live thread state by
/// its spawn-known pid first, snapshots its status, reaps it (killing a
/// hub still running), and panics with `why` and the report. The capture
/// reads the live pid before any wait on the child; a zombie, a vanished
/// pid, or a failed read prints its marker as text. Kill-and-reap still
/// precedes the panic exactly as before, and the stderr text waits for the
/// drain's EOF under its own bound before cloning, so a slow drain cannot
/// omit what the hub wrote.
pub(crate) fn give_up(
    setup: &Setup,
    mut process: HubProc,
    capture: &CapturedStderr,
    why: &str,
) -> ! {
    let pid = process.child.id();
    let threads = hub_threads(pid);
    let status = process.child.try_wait().unwrap();
    match status {
        Some(_) => drop(process.wait()),
        None => drop(process.kill_and_wait()),
    }
    panic!(
        "{why}: the hub {}",
        report(
            setup,
            status,
            &capture.text(Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
            &threads
        )
    );
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
    capture: &CapturedStderr,
    wait: Deadline,
    bound: Duration,
    entry: &str,
    attempts: &[Duration],
) -> (std::io::Result<doors::hub::Hub>, HubProc) {
    hello_slice_with_budgets(
        setup,
        process,
        capture,
        (wait, wait),
        bound,
        entry,
        attempts,
    )
}

/// Runs one hello slice with separate outer and root budgets. A spent root
/// can be tested with a live outer wait, so the no-room error returns before
/// the outer timeout races it.
pub(crate) fn hello_slice_with_budgets(
    setup: &Setup,
    process: HubProc,
    capture: &CapturedStderr,
    waits: (Deadline, Deadline),
    bound: Duration,
    entry: &str,
    attempts: &[Duration],
) -> (std::io::Result<doors::hub::Hub>, HubProc) {
    let (wait, root) = waits;
    let home = setup.home();
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
                    capture,
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
    capture: &CapturedStderr,
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
                capture,
                &format!(
                    "waited for the installed hub's hello ({})",
                    hello_account(&entry, failing_left, failing_left, &attempts),
                ),
            );
        };
        attempts.push(bound);
        let (slice, back) = hello_slice(
            setup,
            process,
            capture,
            setup.deadline,
            bound,
            &entry,
            &attempts,
        );
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
                capture,
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
        let (process, capture) = HubProc::spawn_command_captured(setup.deadline, &mut command);
        match await_hello(setup, process, &capture, helper_left) {
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
                    report(
                        setup,
                        Some(status),
                        &capture.text(Deadline::after(fakes::MUST_SUCCEED_WITHIN)),
                        EXITED_THREADS
                    )
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
    use std::os::unix::process::CommandExt as _;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::Duration;

    use contract::clock::Clock;
    use fakes::clock::{FakeClock, Miss};
    use support::clock::Sleep;

    use super::{
        CAPTURE_CHARS, MUST_SUCCEED_WITHIN, connect_attempt, give_up, hello_bound, hub_threads,
        is_hello_timeout, report, serve_installed, slices_shown, truncate_section,
    };
    use crate::support::{CapturedStderr, Deadline, HubProc, Setup};

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
        let worker_id = Deadline::after(MUST_SUCCEED_WITHIN)
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
                .mark_parked(next_poll, Deadline::after(MUST_SUCCEED_WITHIN))
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
            Deadline::after(MUST_SUCCEED_WITHIN),
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
            MUST_SUCCEED_WITHIN,
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
                &Deadline::after(MUST_SUCCEED_WITHIN),
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
        let result = Deadline::after(MUST_SUCCEED_WITHIN)
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
        let missing = report(&setup, None, "", "");
        assert!(
            missing.contains("hub.log missing"),
            "a fresh home holds no hub.log: {missing}"
        );
        assert!(
            !missing.contains("unreadable"),
            "absence is not an error: {missing}"
        );
        fs::create_dir_all(setup.home().join("logs").join("hub.log")).unwrap();
        let unreadable = report(&setup, None, "", "");
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

    #[test]
    fn truncate_section_marks_only_a_cut() {
        let below: String = "x".repeat(CAPTURE_CHARS - 1);
        assert_eq!(truncate_section(&below), below);
        let at: String = "y".repeat(CAPTURE_CHARS);
        assert_eq!(truncate_section(&at), at);
        let above: String = "z".repeat(CAPTURE_CHARS + 1);
        assert_eq!(
            truncate_section(&above),
            format!("{}...", "z".repeat(CAPTURE_CHARS))
        );
        let wide: String = "é".repeat(CAPTURE_CHARS + 1);
        let cut = truncate_section(&wide);
        assert_eq!(cut.chars().count(), CAPTURE_CHARS + 3);
        assert!(cut.ends_with("..."), "a cut ends in its mark");
    }

    /// Spawns a stub child in its own process group that parks opening
    /// `fifo` for write: the open blocks until a reader arrives, and no
    /// reader arrives during the probe, so the child stays hung regardless
    /// of thread scheduling.
    fn spawn_fifo_stub(deadline: Deadline, fifo: &Path) -> HubProc {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("exec > \"$1\"")
            .arg("fifo-stub")
            .arg(fifo)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        HubProc::spawn_command(deadline, &mut command)
    }

    /// Reads the `give_up` panic's text: only the enriched panic can follow
    /// it, so its text also proves the live child was reaped.
    fn give_up_text(
        setup: &Setup,
        process: HubProc,
        capture: &CapturedStderr,
        why: &str,
    ) -> String {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            give_up(setup, process, capture, why);
        }));
        outcome
            .err()
            .and_then(|payload| payload.downcast::<String>().ok().map(|boxed| *boxed))
            .expect("give_up panics")
    }

    /// The enriched give-up names every capture section: the caller's entry
    /// line, the live child's state, the hub.log state, the stderr
    /// snapshot, the resolved directories, and the live thread state of the
    /// FIFO-hung stub. The probe drives no hello_slice bound: the ticket's
    /// subject is the capture report, so it tests the reporter, not the
    /// scheduling that reaches it. The kill in `give_up` reaps the stub
    /// before the panic, and the home's TempDir removes the FIFO, so a
    /// failing probe never wedges later tests.
    #[test]
    fn give_up_reports_stderr_dirs_and_thread_state() {
        let setup = Setup::new();
        let fifo = setup.home().join("stub.fifo");
        fakes::fifo(&fifo);
        let process = spawn_fifo_stub(setup.deadline, &fifo);
        let pid = process.child.id();
        let capture =
            CapturedStderr::drained_text("hub stderr line 1\nhub stderr line 2\n".to_owned());
        let message = give_up_text(
            &setup,
            process,
            &capture,
            "waited for the installed hub's hello (test)",
        );
        assert!(
            message.contains("waited for the installed hub's hello (test)"),
            "the panic keeps the caller's entry line: {message}"
        );
        assert!(
            message.contains("was still running"),
            "the panic names the live child's state: {message}"
        );
        assert!(
            message.contains("hub.log missing"),
            "a fresh home holds no hub.log: {message}"
        );
        assert!(
            message.contains("hub stderr (live snapshot, last 20 lines):"),
            "the panic carries the stderr section: {message}"
        );
        assert!(
            message.contains("hub stderr line 2"),
            "the panic carries the snapshot's tail: {message}"
        );
        assert!(
            message.contains(&format!("state dir {}", setup.home().join("run").display())),
            "the panic names the resolved state directory: {message}"
        );
        assert!(
            message.contains(&format!("log dir {}", setup.home().join("logs").display())),
            "the panic names the resolved log directory: {message}"
        );
        assert!(
            message.contains(&format!("pid {pid}")),
            "the panic names the captured pid: {message}"
        );
        #[cfg(target_os = "linux")]
        assert!(
            message.contains("wchan"),
            "the panic names the stub's park state: {message}"
        );
        fs::remove_file(&fifo).unwrap_or(());
    }

    /// An empty snapshot on a running hub reports that nothing was captured
    /// yet, never as a clean drain; an empty hub.log reports distinctly
    /// from a missing one.
    #[test]
    fn give_up_marks_empty_stderr_and_empty_log() {
        let setup = Setup::new();
        fs::create_dir_all(setup.home().join("logs")).unwrap();
        fs::write(setup.home().join("logs").join("hub.log"), "").unwrap();
        let fifo = setup.home().join("stub.fifo");
        fakes::fifo(&fifo);
        let process = spawn_fifo_stub(setup.deadline, &fifo);
        let capture = CapturedStderr::drained_text(String::new());
        let message = give_up_text(
            &setup,
            process,
            &capture,
            "waited for the installed hub's hello (test)",
        );
        assert!(
            message.contains("(still running: no stderr captured yet)"),
            "an empty snapshot on a running hub is not a clean drain: {message}"
        );
        assert!(
            message.contains("hub.log empty (0 bytes)"),
            "an empty hub.log reports distinctly from a missing one: {message}"
        );
        fs::remove_file(&fifo).unwrap_or(());
    }

    /// A long valid home truncates with the cut mark instead of pushing
    /// the report's head out; a short one passes through whole.
    #[test]
    fn dirs_section_truncates_a_long_home() {
        let long = Path::new("/h").join("x".repeat(CAPTURE_CHARS + 1));
        let cut = super::dirs_section(&long);
        assert!(
            cut.starts_with("state dir "),
            "the truncation keeps the head: {cut}"
        );
        assert!(cut.ends_with("..."), "a cut ends in its mark");
        assert_eq!(
            super::dirs_section(Path::new("/h")),
            "state dir /h/run, log dir /h/logs"
        );
    }

    /// A pid with no task directory reads as its own marker, never as
    /// empty output and never as a panic.
    #[test]
    fn hub_threads_names_a_gone_pid() {
        let pid = u32::MAX;
        let threads = hub_threads(pid);
        assert!(
            threads.contains(&format!("pid {pid}")),
            "the capture names the pid it read: {threads}"
        );
        #[cfg(target_os = "linux")]
        assert!(
            threads.contains("pid gone"),
            "a vanished pid prints its marker as text: {threads}"
        );
    }

    /// An exited but unreaped child has no live threads: the capture names
    /// the zombie as text, and the give-up still reports the exit with an
    /// empty snapshot as nothing captured, never as a clean drain.
    #[cfg(target_os = "linux")]
    #[test]
    fn hub_threads_names_a_zombie_as_text() {
        let setup = Setup::new();
        let mut command = Command::new("true");
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let process = HubProc::spawn_command(setup.deadline, &mut command);
        let pid = process.child.id();
        // A reap-free exit probe: `waitid` with `WNOWAIT` reports the exit
        // while leaving the child a zombie for `hub_threads` to name, which
        // `try_wait` itself would reap away. Blocking code runs on a thread
        // and the test receives under its own bound, which fails naming the
        // wait instead of hanging the runner.
        let (exited_tx, exited_rx) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                let waited = rustix::process::waitid(
                    rustix::process::WaitId::Pid(rustix::process::Pid::from_child(&process.child)),
                    rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOWAIT,
                );
                exited_tx.send(waited.map(|_| ())).unwrap_or(());
            });
            // The scope joins the probe afterwards; the stub exits at once,
            // so even the failure path joins promptly instead of hanging.
            Deadline::after(MUST_SUCCEED_WITHIN)
                .recv_or_fail(&exited_rx, "the true stub to exit")
                .unwrap_or_else(|error| panic!("reap-free wait for the true stub: {error}"));
        });
        let threads = hub_threads(pid);
        assert!(
            threads.contains("no live threads"),
            "an exited unreaped child names its zombie state: {threads}"
        );
        let capture = CapturedStderr::drained_text(String::new());
        let message = give_up_text(&setup, process, &capture, "waited (test)");
        assert!(
            message.contains("exited"),
            "the panic names the hub's exit: {message}"
        );
        assert!(
            message.contains("(no stderr captured)"),
            "an empty snapshot on an exited hub is not a clean drain: {message}"
        );
    }
}
