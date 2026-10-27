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

/// The terminal emits this synchronized-output marker after each frame
/// (`docs/tui.md`, "Performance").
const FRAME_END: &[u8] = b"\x1b[?2026l";

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
    let input = run.output().len();
    run.write(b"\x03");
    run.wait_bytes(input, FRAME_END, "the cleared home-input frame");
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

/// Types `prefix`, waits for the slash-list row and its frame to finish,
/// types `rest`, and returns the output offset before `rest`.
fn run_slash(run: &mut Run, prefix: &str, row: &str, rest: &str, command: &str) -> usize {
    let prefix_input = run.output().len();
    run.write(prefix.as_bytes());
    let what = format!("the slash-list row for {command}");
    run.wait_screen(&what, |grid| grid.contents.contains(row));
    run.wait_bytes(prefix_input, FRAME_END, "the slash-list frame");
    let rest_input = run.output().len();
    run.write(rest.as_bytes());
    rest_input
}

/// Opens a command's view, returns the row containing `marker`, then
/// closes the view and quits.
fn view_row(
    setup: &Setup,
    command: &str,
    prefix: &str,
    row: &str,
    rest: &str,
    marker: &str,
) -> String {
    let mut run = terminal(setup);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    let rest_input = run_slash(&mut run, prefix, row, rest, command);
    let view = run.wait_screen("the command view", |grid| grid.contents.contains(marker));
    run.wait_bytes(rest_input, FRAME_END, "the command view frame");
    let found = row_with(&view, marker, command);

    let close_input = run.output().len();
    run.write(b"\x1b[27u"); // Kitty-protocol Esc
    run.wait_screen("the closed command view", |grid| {
        !grid.contents.contains(marker)
    });
    run.wait_bytes(close_input, FRAME_END, "the closed command view frame");
    quit(run, command);
    found
}

/// The rebinding screen's footer line after running `command` through
/// `prefix` plus `rest`, for the row `row`.
fn rebinding_footer(command: &str, prefix: &str, row: &str, rest: &str) -> String {
    view_row(&Setup::new(), command, prefix, row, rest, "Enter rebind")
}

#[test]
fn keymap_opens_the_rebinding_screen_like_keys() {
    // The row shows `/keybindings` before `/keymap` (`docs/tui.md:1944,1965`).
    let keys = rebinding_footer("/keys", "/keys", "keys /keybindings /keymap", "\r");
    let keymap = rebinding_footer("/keymap", "/keymap", "keys /keybindings /keymap", "\r");
    assert_eq!(keymap, keys);
}

/// The standing-rules row after running `command` through `prefix`
/// plus `rest`, for the row `row`.
fn rules_row(command: &str, prefix: &str, row: &str, rest: &str) -> String {
    view_row(
        &rules_setup(),
        command,
        prefix,
        row,
        rest,
        "allow shell:git status",
    )
}

#[test]
fn permissions_opens_the_standing_rules_like_rules() {
    // The table and alias rule set this row order (`docs/tui.md:1940,1965`).
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
    let rest_input = run_slash(&mut run, prefix, row, rest, command);
    let grid = run.wait_screen("the home notice", |grid| grid.contents.contains(notice));
    run.wait_bytes(rest_input, FRAME_END, "the home notice frame");
    let found = row_with(&grid, notice, command);
    quit(run, command);
    found
}

#[test]
fn compact_starts_a_handoff_like_handoff() {
    // The table and alias rule set this row order (`docs/tui.md:1947,1965`).
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
    // The table and alias rule set this row order (`docs/tui.md:1954,1965`).
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
