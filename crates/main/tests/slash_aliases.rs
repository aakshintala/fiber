//! Binary-level tests of the slash aliases (see #3442): the built
//! `fiber` on a pty, typing each alias and asserting the same screen or
//! action as its canonical command, with the alias on the same
//! slash-list row.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use serde_json::json;
use support::pty::Run;
use support::{Setup, write_json};

/// The terminal on home: an offline model keeps boot free of the
/// network, and the hub exits at once after its last client leaves, so
/// `wait` never outlasts the test's deadline.
fn terminal(setup: &Setup) -> Run {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "hub": {"idle_exit_ms": 0}}),
    );
    Run::spawn(
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

/// A setup whose `/rules` view lists one standing rule.
fn rules_setup() -> Setup {
    let setup = Setup::new();
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "git status",
                "added": 1000, "session_id": "s_aaa"})
        ),
    )
    .unwrap();
    setup
}

/// Quits from home after Ctrl+C clears any remaining input, then checks
/// the restored terminal and a clean exit.
fn quit(mut run: Run, command: &str) {
    // The caller's complete post-command frame is the input-ready signal.
    // Ctrl+C then `/quit` is one ordered cleanup sequence.
    run.write(b"\x03");
    run.write(b"/quit\r");
    run.wait_screen("the restored terminal", |grid| {
        !grid.alternate_screen && !grid.hide_cursor
    });
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0), "{command}");
}

/// The first grid row containing `needle`.
fn row_with(grid: &support::pty::Grid, needle: &str, command: &str) -> String {
    grid.rows
        .iter()
        .find(|row| row.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} for {command} in\n{}", grid.contents))
        .clone()
}

/// Types `prefix`, waits for the slash-list row naming the canonical
/// command beside its alias, types `rest`, runs, and returns the grid.
fn run_slash(run: &mut Run, prefix: &str, row: &str, rest: &str, command: &str) {
    run.write(prefix.as_bytes());
    let what = format!("the slash-list row for {command}");
    run.wait_screen(&what, |grid| grid.contents.contains(row));
    // This whole frame shows the prefix was handled and the input loop
    // is ready for `rest`.
    run.write(rest.as_bytes());
}

/// The rebinding screen's footer line after running `command` through
/// `prefix` plus `rest`, for the row `row`.
fn rebinding_footer(command: &str, prefix: &str, row: &str, rest: &str) -> String {
    let setup = Setup::new();
    let mut run = terminal(&setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run_slash(&mut run, prefix, row, rest, command);
    let grid = run.wait_screen("the rebinding screen", |grid| {
        grid.contents.contains("Enter rebind")
    });
    let footer = row_with(&grid, "Enter rebind", command);
    // This whole frame shows the rebinding screen is ready for Esc.
    run.write(b"\x1b[27u"); // Kitty-protocol Esc
    // Confirm Esc closed the view before sending the cleanup command.
    run.wait_screen("the closed rebinding screen", |grid| {
        !grid.contents.contains("Enter rebind")
    });
    quit(run, command);
    footer
}

#[test]
fn keymap_opens_the_rebinding_screen_like_keys() {
    // The row shows `/keybindings` before `/keymap` (`docs/tui.md:1937,1957`).
    let keys = rebinding_footer("/keys", "/keys", "keys /keybindings /keymap", "\r");
    let keymap = rebinding_footer("/keymap", "/keymap", "keys /keybindings /keymap", "\r");
    assert_eq!(keymap, keys);
}

/// The standing-rules row after running `command` through `prefix`
/// plus `rest`, for the row `row`.
fn rules_row(command: &str, prefix: &str, row: &str, rest: &str) -> String {
    let setup = rules_setup();
    let mut run = terminal(&setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run_slash(&mut run, prefix, row, rest, command);
    let grid = run.wait_screen("the rules rows", |grid| {
        grid.contents.contains("allow shell:git status")
    });
    let found = row_with(&grid, "allow shell:git status", command);
    // This whole frame shows the rules screen is ready for Esc.
    run.write(b"\x1b[27u"); // Kitty-protocol Esc
    // Confirm Esc closed the view before sending the cleanup command.
    run.wait_screen("the closed rules view", |grid| {
        !grid.contents.contains("allow shell:git status")
    });
    quit(run, command);
    found
}

#[test]
fn permissions_opens_the_standing_rules_like_rules() {
    // The table and alias rule set this row order (`docs/tui.md:1933,1957`).
    let rules = rules_row("/rules", "/rules", "rules /permissions", "\r");
    let permissions = rules_row("/permissions", "/permissions", "rules /permissions", "\r");
    assert_eq!(permissions, rules);
}

/// The home notice after running `command` through `prefix` plus
/// `rest`, for the row `row`.
fn home_notice(command: &str, prefix: &str, row: &str, rest: &str, notice: &str) -> String {
    let setup = Setup::new();
    let mut run = terminal(&setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run_slash(&mut run, prefix, row, rest, command);
    let grid = run.wait_screen("the home notice", |grid| grid.contents.contains(notice));
    let found = row_with(&grid, notice, command);
    quit(run, command);
    found
}

#[test]
fn compact_starts_a_handoff_like_handoff() {
    // The table and alias rule set this row order (`docs/tui.md:1940,1957`).
    let row = "handoff /compact";
    let handoff = home_notice(
        "/handoff",
        "/handof",
        row,
        "f take over\r",
        "No session on screen.",
    );
    let compact = home_notice(
        "/compact",
        "/compa",
        row,
        "ct take over\r",
        "No session on screen.",
    );
    assert_eq!(compact, handoff);
}

#[test]
fn stop_asks_to_close_all_like_close() {
    // The table and alias rule set this row order (`docs/tui.md:1947,1957`).
    let row = "close /stop";
    let close = home_notice(
        "/close all",
        "/close",
        row,
        " all\r",
        "No sessions to close.",
    );
    let stop = home_notice("/stop all", "/sto", row, "p all\r", "No sessions to close.");
    assert_eq!(stop, close);
}
