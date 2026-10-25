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
use serde_json::json;

use crate::support::{HubProc, Setup, SystemClock, bounded, run_to_exit, write_json};

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
/// (`None` for a hub still running) and the tail of `hub.log`.
fn report(setup: &Setup, status: Option<ExitStatus>) -> String {
    let log = setup.hub_log();
    let lines: Vec<&str> = log.lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_TAIL)..].join("\n");
    let state = match status {
        Some(status) => format!("exited ({status})"),
        None => "was still running".to_owned(),
    };
    format!("{state}; hub.log tail (last {LOG_TAIL} lines):\n{tail}")
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

/// Waits for the hub's `hub_hello` in slices of
/// [`MUST_SUCCEED_WITHIN`], looking at the hub's exit between them. `Ok`
/// is the hello and the live hub; `Err` is the status of a hub that
/// exited, already reaped. The deadline is the test's own, never renewed:
/// a slice ends [`REPORT_MARGIN`] before it, so the report and the
/// cleanup run first.
fn await_hello(
    setup: &Setup,
    mut process: HubProc,
) -> Result<(doors::hub::Hub, HubProc), ExitStatus> {
    loop {
        let left = setup.deadline.left();
        if left <= REPORT_MARGIN {
            give_up(setup, process, "waited for the installed hub's hello");
        }
        let until = SystemClock.now() + left.saturating_sub(REPORT_MARGIN).min(MUST_SUCCEED_WITHIN);
        let home = setup.home();
        let slice = bounded(setup.deadline, "the installed hub's hello", move || {
            doors::hub::connect_until(&home, &mut || Ok(()), &SystemClock, until, &mut || {})
        });
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
                &format!("the installed hub's hello failed with {error}"),
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

/// Serves `a`'s hub on a test port and pairs a fresh client to it as
/// `box`: the client selects `box` by default when `default` holds.
/// The seeding scaffold `terminal.rs` and `hub_prune.rs` share: serve,
/// pair, then add, each awaited under the test's deadline.
pub(crate) fn paired_client(a: &Setup, default: bool) -> (Setup, Served) {
    let b = Setup::new();
    let served = serve_installed(a, ATTEMPTS, &mut free_port, &mut |port| {
        write_json(
            &a.home().join("config.json"),
            &json!({"hub": {"port": port}}),
        );
        a.fiber(&["hub", "serve", "--installed"])
    });
    assert_eq!(served.hub.1.kind, "hub_hello");
    let paired = run_to_exit(a.deadline, "hub pair", a.fiber(&["hub", "pair", "laptop"]));
    assert_eq!(paired.status.code(), Some(0));
    let code = String::from_utf8(paired.stdout).unwrap();
    let address = format!("ws://127.0.0.1:{}", served.port);
    let input = b.root.path().join("pairing-input");
    fs::write(&input, format!("{}\n", code.trim())).unwrap();
    let mut args = vec!["hub", "add", "box", &address];
    if default {
        args.push("--default");
    }
    let mut command = b.fiber(&args);
    command.stdin(fs::File::open(input).unwrap());
    let added = run_to_exit(b.deadline, "hub add with a pairing code", command);
    assert_eq!(added.status.code(), Some(0));
    assert!(added.stdout.is_empty());
    (b, served)
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
    let mut attempt = 1;
    loop {
        let port = ports();
        let logged = setup.hub_log().len();
        let mut command = serve(port);
        let process = HubProc::spawn_command(setup.deadline, &mut command);
        match await_hello(setup, process) {
            Ok((hub, process)) => return Served { port, hub, process },
            Err(status) => {
                let log = setup.hub_log();
                let added = log.get(logged..).unwrap_or_default();
                if attempt < attempts && port_taken(added, port) {
                    attempt += 1;
                    continue;
                }
                panic!(
                    "the installed hub's hello on 127.0.0.1:{port}, attempt {attempt} of {attempts}: the hub {}",
                    report(setup, Some(status))
                );
            }
        }
    }
}
