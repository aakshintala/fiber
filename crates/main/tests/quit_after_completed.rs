//! Binary-level regression test for see #1739: quitting right after a
//! turn completes exits and restores the terminal.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use support::Setup;
use support::pty::Run;

/// Whether `haystack` holds `needle` as bytes.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[test]
fn quitting_right_after_completed_exits_and_restores_the_terminal() {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Hel", "lo."]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    let mut run = Run::spawn(
        &setup,
        160,
        48,
        &[
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "ghostty"),
        ],
    );
    run.read_until(">");
    run.write(b"say hi\r");
    run.read_until("Hel");
    run.read_until("completed");
    let stalled_at = run.output().len();
    run.stall();
    run.write(b"\x03\x03\r");
    let exited = run.wait();
    assert_eq!(exited.status.code(), Some(0));
    assert!(
        contains(&exited.terminal[stalled_at..], b"\x1b[?1049l\x1b[?25h"),
        "the terminal was restored after the stall"
    );
}
