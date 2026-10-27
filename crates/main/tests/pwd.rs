//! Binary-level tests for `/pwd` and its `/cwd` alias (see #3444): on a
//! session the terminal shows the session's workspace as a notice, and on
//! home it shows the no-session notice.

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
use support::hello;

/// Spawns the terminal at 120 by 32 with the fake provider installed,
/// so `say hi` starts a session in the setup workspace. The hub exits a
/// second after its last client leaves, so `wait` never outlasts the
/// test's deadline.
fn terminal(setup: &support::Setup, server: &ProviderServer) -> support::pty::Run {
    setup.provider(server);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 1000}}),
    );
    support::pty::Run::spawn(
        setup,
        120,
        32,
        &[],
        &[
            ("FIBER_TEST_FAKE_KEY", "sk-test"),
            ("TERM", "xterm-256color"),
        ],
    )
}

/// Starts a session with `say hi` and waits for the reply's first
/// delta: the turn ran, so its `session_started` folded before it.
fn start_session(run: &mut support::pty::Run) {
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"say hi\r");
    run.wait_screen("the first delta", |grid| grid.contents.contains("Hel"));
}

/// Quits with the session left running and waits for a clean exit: the
/// primary screen is back, one resume line per live session is on it,
/// and the process ends 0.
fn quit_with_session(mut run: support::pty::Run) {
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// Types `command` on the session and waits for the workspace notice:
/// the recorded root, which is the launched workspace here.
fn workspace_notice(command: &str) {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    let workspace = std::fs::canonicalize(setup.workspace()).unwrap();
    let workspace = workspace.to_string_lossy().into_owned();
    let mut run = terminal(&setup, &server);
    start_session(&mut run);
    run.write(format!("{command}\r").as_bytes());
    run.wait_screen("the workspace notice", |grid| {
        grid.contents.contains(&workspace)
    });
    quit_with_session(run);
}

#[test]
fn pwd_shows_the_session_workspace() {
    workspace_notice("/pwd");
}

#[test]
fn cwd_shows_the_session_workspace() {
    workspace_notice("/cwd");
}

/// Types `/pwd` on home and waits for the no-session notice, then quits
/// to a clean exit with the terminal restored.
#[test]
fn pwd_on_home_shows_no_session() {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    let mut run = terminal(&setup, &server);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"/pwd\r");
    run.wait_screen("the no-session notice", |grid| {
        grid.contents.contains("No session on screen")
    });
    run.write(b"\x03\x03\r");
    run.wait_screen("the restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
