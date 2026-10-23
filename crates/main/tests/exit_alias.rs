//! Binary-level test that `/exit` quits exactly as `/quit` does (see
//! #2832): idle, and with a session working.

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
use support::pty::Grid;

/// Installs the fake provider and spawns the terminal at 120 by 32.
/// The hub exits a second after its last client leaves, so
/// `wait` never outlasts the test's deadline.
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

/// Whether the grid shows a working line with its elapsed count:
/// "Working" plus a digit right after its space.
fn working_elapsed(contents: &str) -> bool {
    contents.match_indices("Working ").any(|(at, _)| {
        contents[at + "Working ".len()..]
            .chars()
            .next()
            .is_some_and(|next| next.is_ascii_digit())
    })
}

/// Whether the row belongs to the quit question overlay.
fn overlay_row(row: &str) -> bool {
    row.contains("Quit")
        || row.contains("session working")
        || row.contains("leave them running")
        || row.contains("Close all")
        || row.contains("they keep running")
}

/// Quits from a screen with no live session with `command` and waits
/// for a clean exit: the primary screen is back and the process ends 0.
fn idle_quits(command: &str) {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    let mut run = terminal(&setup, &server);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"\x03");
    run.write(format!("{command}\r").as_bytes());
    run.wait_screen("the restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0), "{command}");
}

#[test]
fn exit_quits_an_idle_session_like_quit() {
    idle_quits("/quit");
    idle_quits("/exit");
}

/// Opens the quit question with `command` while a turn runs, leaves
/// the session running with Enter, and returns the overlay's rows.
fn working_overlay(command: &str) -> Vec<String> {
    let setup = support::Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    server.hold();
    let mut run = terminal(&setup, &server);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"say hi\r");
    assert!(
        server.await_requests(1, setup.deadline),
        "the first request to reach the server"
    );
    run.wait_screen("the running turn", |grid| working_elapsed(&grid.contents));
    // The Working Ns line fires on turn_started, before the feed row's
    // State catches up: sending the command here exercises the window
    // where the row still reads idle, and the quit question still opens
    // with 1 session working.
    run.write(format!("{command}\r").as_bytes());
    let grid: Grid = run.wait_screen("the quit question", |grid| {
        grid.contents.contains("Quit") && grid.contents.contains("1 session working")
    });
    let overlay: Vec<String> = grid
        .rows
        .iter()
        .filter(|row| overlay_row(row))
        .cloned()
        .collect();
    assert!(!overlay.is_empty(), "{command}");
    // Enter leaves the working session running: the terminal is
    // restored, with one resume line per live session on it.
    run.write(b"\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    // The turn is still held open: letting its answer through lets the
    // hub go idle and exit.
    server.release_one();
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0), "{command}");
    overlay
}

#[test]
fn exit_asks_while_working_like_quit() {
    let quit = working_overlay("/quit");
    let exit = working_overlay("/exit");
    assert_eq!(exit, quit);
}
