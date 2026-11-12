//! The real terminal's normal quit remains silent and returns its status.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

#[test]
fn tui_normal_exit_keeps_its_status_and_leaves_no_failure_message() {
    let setup = support::Setup::new();
    let mut run = support::pty::Run::spawn(&setup, 120, 32, &[], &[]);
    run.wait_screen("the terminal's first frame", |grid| {
        grid.contents.contains("No sessions yet.")
    });
    run.write(b"\x03\x03\r");
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(0));
    assert!(
        !String::from_utf8_lossy(&exited.terminal).contains("fiber: terminal could not continue"),
        "normal TUI exit must not print a failure reason"
    );
}
