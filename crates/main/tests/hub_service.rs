//! `fiber hub install` and `uninstall` against the real service manager
//! (`docs/invocation.md`, "The hub"): systemd's user instance on Linux, run
//! by CI with `--run-ignored only`, and launchd on macOS, run by the owner.
//! Each test loads a unit for a temporary Fiber home under the real `HOME`,
//! because the user's manager reads only the real unit directory, and
//! removes it again on every path out.

#![cfg(any(target_os = "linux", target_os = "macos"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use fakes::ProviderServer;
use serde_json::json;
use std::ffi::OsString;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use support::*;

/// The variables `systemctl --user` and `launchctl` need to reach the
/// user's manager, which `Setup::fiber` clears.
const MANAGER_VARS: [&str; 4] = [
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "XDG_CONFIG_HOME",
    "USER",
];

/// `fiber <args>` for the test's Fiber home, with the real `HOME` and the
/// manager's variables.
fn fiber(setup: &Setup, args: &[&str]) -> Command {
    let mut command = setup.fiber(args);
    command.env("HOME", std::env::var_os("HOME").expect("HOME is set"));
    for key in MANAGER_VARS {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    command
}

/// `program <args>` in its own process group, inheriting the test's
/// environment, stdout piped.
fn host(program: &str, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    command
}

/// Runs `command` to its exit and returns its stdout, asserting exit 0.
fn succeeds(setup: &Setup, what: &str, command: Command) -> String {
    let output = run_to_exit(setup.deadline, what, command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{what}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// Calls `look` until it gives a value, failing at the test's deadline
/// naming `what`. Each `look` already waits on a signal: a manager call
/// through [`run_to_exit`] or a socket `connect`, so the test never sleeps
/// itself; it retries at once with what remains of the one deadline.
fn wait_for<T>(setup: &Setup, what: &str, mut look: impl FnMut() -> Option<T>) -> T {
    loop {
        if let Some(found) = look() {
            return found;
        }
        assert!(
            !setup.deadline.left().is_zero(),
            "waited until the deadline for {what}"
        );
    }
}

/// A client of the hub once it speaks `hub_hello` on `run/hub`, waited on
/// through `doors::hub::connect`'s own retry on the process clock: the
/// retry sleeps in product code, the test only receives it with the one
/// deadline and never sleeps itself.
fn hub_client(setup: &Setup) -> Socket {
    let deadline = setup.deadline;
    let home = setup.home();
    let (stream, hello) = loop {
        let home = home.clone();
        match bounded(deadline, "run/hub to accept", move || {
            let mut start = || -> std::io::Result<()> { Ok(()) };
            doors::hub::connect(&home, &mut start, &SystemClock)
        }) {
            Ok(hub) => break hub,
            Err(_) => {
                assert!(
                    !deadline.left().is_zero(),
                    "waited until the deadline for run/hub to accept"
                );
            }
        }
    };
    assert_eq!(hello.kind, "hub_hello");
    Socket::from(deadline, stream)
}

/// The hub's pid as the manager reports it, once it reports one.
#[cfg(target_os = "linux")]
fn hub_pid(setup: &Setup, name: &str) -> u32 {
    let unit = format!("{name}.service");
    wait_for(setup, "systemd to report the hub's pid", || {
        let shown = succeeds(
            setup,
            "systemctl --user show",
            host(
                "systemctl",
                &["--user", "show", "-p", "MainPID", "--value", &unit],
            ),
        );
        shown.trim().parse::<u32>().ok().filter(|pid| *pid != 0)
    })
}

#[cfg(target_os = "macos")]
fn hub_pid(setup: &Setup, name: &str) -> u32 {
    let target = format!("{}/{name}", domain(setup));
    wait_for(setup, "launchd to report the hub's pid", || {
        let printed = succeeds(
            setup,
            "launchctl print",
            host("launchctl", &["print", &target]),
        );
        printed
            .lines()
            .find_map(|line| line.trim().strip_prefix("pid = "))
            .and_then(|pid| pid.trim().parse::<u32>().ok())
    })
}

#[cfg(target_os = "macos")]
fn domain(setup: &Setup) -> String {
    format!(
        "gui/{}",
        succeeds(setup, "id -u", host("id", &["-u"])).trim()
    )
}

/// Restarts the hub through the manager, as `fiber update` does.
#[cfg(target_os = "linux")]
fn restart(setup: &Setup, name: &str) {
    succeeds(
        setup,
        "systemctl --user restart",
        host(
            "systemctl",
            &["--user", "restart", &format!("{name}.service")],
        ),
    );
}

#[cfg(target_os = "macos")]
fn restart(setup: &Setup, name: &str) {
    let target = format!("{}/{name}", domain(setup));
    succeeds(
        setup,
        "launchctl kickstart",
        host("launchctl", &["kickstart", "-k", &target]),
    );
}

/// Runs `fiber hub uninstall` when dropped, so a failing test leaves no
/// unit loaded. It waits at most the cleanup deadline and never panics.
struct Uninstall {
    command: Option<Command>,
    deadline: Deadline,
}

impl Drop for Uninstall {
    fn drop(&mut self) {
        let Some(mut command) = self.command.take() else {
            return;
        };
        let (done, finished) = mpsc::channel();
        let spawned = thread::Builder::new().spawn(move || {
            let output = command.output();
            match done.send(output) {
                Ok(()) | Err(_) => {}
            }
        });
        if spawned.is_ok() {
            match finished.recv_timeout(self.deadline.cleanup()) {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

/// Install, idempotency, a restart and uninstall, with a session that
/// outlives the restart and the uninstall.
fn the_login_service_round_trip() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    // The installed hub runs under the service manager's bare environment:
    // only `FIBER_HOME` reaches it, so the test env var the fake provider
    // declares never arrives. The stored credential is read first, so store
    // the fake key the way `fiber login fake` writes it.
    config::store_credential(
        &setup.home(),
        "fake",
        "default",
        &contract::Secret::new("sk-test".to_owned()),
    )
    .unwrap();
    let _uninstall = Uninstall {
        command: Some(fiber(&setup, &["hub", "uninstall"])),
        deadline: setup.deadline,
    };

    let installed = succeeds(
        &setup,
        "fiber hub install",
        fiber(&setup, &["hub", "install"]),
    );
    let unit = PathBuf::from(
        installed
            .lines()
            .find_map(|line| line.strip_prefix("Installed the hub's login service: "))
            .unwrap_or_else(|| panic!("install names the unit file: {installed}")),
    );
    assert!(unit.is_file(), "{}", unit.display());
    let name = unit
        .file_stem()
        .map(OsString::from)
        .and_then(|stem| stem.into_string().ok())
        .expect("the unit file is named after the service");
    drop(hub_client(&setup));
    let pid = hub_pid(&setup, &name);

    succeeds(
        &setup,
        "the second fiber hub install",
        fiber(&setup, &["hub", "install"]),
    );
    assert_eq!(
        hub_pid(&setup, &name),
        pid,
        "a second install restarts nothing"
    );

    let workspace = setup.workspace().to_string_lossy().into_owned();
    let guard = SessionGuard::arm(setup.deadline, &workspace);
    let client = hub_client(&setup);
    client.send(
        &json!({"id": "c_start", "command": "start", "args": {"workspace": workspace}}).to_string(),
    );
    let ack = recv_reply(&client, "the start acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let session = ack["payload"]["result"]["session_id"]
        .as_str()
        .expect("the start answers with a session id")
        .to_owned();
    let session_socket = setup.session_socket(&session);
    let accepting = || UnixStream::connect(&session_socket).ok().map(drop);
    wait_for(&setup, "the session's socket to accept", accepting);
    drop(client);

    restart(&setup, &name);
    wait_for(&setup, "the restarted hub's new pid", || {
        Some(hub_pid(&setup, &name)).filter(|now| *now != pid)
    });
    drop(hub_client(&setup));
    assert!(
        accepting().is_some(),
        "the session outlived the hub's restart"
    );

    let status = succeeds(
        &setup,
        "fiber hub status --json",
        fiber(&setup, &["hub", "status", "--json"]),
    );
    let status: serde_json::Value = serde_json::from_str(&status).unwrap();
    assert_eq!(status["running"], true, "{status}");
    assert_eq!(status["installed"], true, "{status}");

    succeeds(
        &setup,
        "fiber hub uninstall",
        fiber(&setup, &["hub", "uninstall"]),
    );
    assert!(!unit.exists(), "uninstall removed {}", unit.display());
    let hub_socket = setup.hub_socket();
    wait_for(&setup, "run/hub to stop accepting", || {
        UnixStream::connect(&hub_socket).is_err().then_some(())
    });
    let direct = Socket::connect(setup.deadline, &session_socket);
    close_session(&direct);
    drop(direct);
    guard.wait_gone();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "loads a systemd user unit; CI runs it with --run-ignored"]
fn systemd_installs_restarts_and_removes_the_hub_and_sessions_carry_on() {
    the_login_service_round_trip();
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "loads a launchd agent; the owner runs it"]
fn launchd_installs_restarts_and_removes_the_hub_and_sessions_carry_on() {
    the_login_service_round_trip();
}
