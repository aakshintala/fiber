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
use support::pty::{Run, Screen, contains};

/// The harness pins the terminal to 120 by 32 (`docs/testing.md`,
/// "Screens").
const COLS: u16 = 120;
const ROWS: u16 = 32;

/// Whether the screen shows the turn's `completed` status: nine
/// consecutive cells spelling it on one row.
fn completed(screen: &Screen) -> bool {
    const WORD: &[u8] = b"completed";
    (0..ROWS).any(|y| {
        (0..COLS - u16::try_from(WORD.len()).unwrap() + 1).any(|x| {
            WORD.iter().enumerate().all(|(dx, byte)| {
                let cell = screen.cell(x + u16::try_from(dx).unwrap(), y);
                cell.symbol.len() == 1 && cell.symbol.as_bytes()[0] == *byte
            })
        })
    })
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
        COLS,
        ROWS,
        &[
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "ghostty"),
        ],
    );
    run.read_until(">");
    run.write(b"say hi\r");
    run.read_until("Hel");
    run.screen_until(COLS, ROWS, "the completed turn", completed);
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
