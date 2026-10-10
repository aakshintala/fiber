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

/// Whether the cells starting at `x`, `y` spell `word`: one cell per
/// byte, each a single-char cell holding that byte.
fn spells(screen: &Screen, x: u16, y: u16, word: &[u8]) -> bool {
    word.iter().enumerate().all(|(dx, byte)| {
        let cell = screen.cell(x + u16::try_from(dx).unwrap(), y);
        cell.symbol.len() == 1 && cell.symbol.as_bytes()[0] == *byte
    })
}

/// Whether any row holds `word` in consecutive cells.
fn shows(screen: &Screen, word: &[u8]) -> bool {
    (0..ROWS).any(|y| {
        (0..COLS - u16::try_from(word.len()).unwrap() + 1).any(|x| spells(screen, x, y, word))
    })
}

/// Whether the screen shows the input box's prompt row: `>` with a
/// space before it and a stripe or a space before that, the
/// prompt-row shape look.rs reads.
fn prompt(screen: &Screen) -> bool {
    (0..ROWS).any(|y| {
        (2..COLS).any(|x| {
            screen.cell(x, y).symbol.as_str() == ">"
                && screen.cell(x - 1, y).symbol.as_str() == " "
                && matches!(screen.cell(x - 2, y).symbol.as_str(), "\u{258c}" | " ")
        })
    })
}

/// Whether the screen shows the reply's first fragment: `Hel` in
/// consecutive cells on one row.
fn reply(screen: &Screen) -> bool {
    shows(screen, b"Hel")
}

/// Whether the screen shows the turn's `completed` status: nine
/// consecutive cells spelling it on one row.
fn completed(screen: &Screen) -> bool {
    shows(screen, b"completed")
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
    run.screen_until(COLS, ROWS, "the prompt", prompt);
    run.write(b"say hi\r");
    run.screen_until(COLS, ROWS, "the reply", reply);
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
