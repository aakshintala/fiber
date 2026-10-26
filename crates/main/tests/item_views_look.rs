//! Binary-level tests of the delegate view and a job's pty view
//! (`docs/tui.md`, "Swapped views"): the real binary on a 160x48 pty in
//! truecolour, with the scripted provider answering, draws the doc's
//! swapped-view rows (top row, bold breadcrumb, `surface` status card,
//! live-screen line, `turn` slab, input hints).

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// `error` in the dark theme.
const ERROR: Colour = Colour::Rgb(255, 93, 115);
/// `surface` in the dark theme.
const SURFACE: Colour = Colour::Rgb(26, 26, 34);
/// `turn` in the dark theme.
const TURN: Colour = Colour::Rgb(16, 16, 23);

/// A held delegate and a held tty job on one FIFO: the scripted `shell`
/// (with `tty: true`) and the scripted delegate's blocking step both run
/// `head -n 1 <hold>`, parking until the test drops its open FIFO. The
/// watchdog reaps the process group on drop: armed first, before any
/// fallible step.
fn held_setup() -> (Setup, fs::File, fakes::Watchdog) {
    let setup = Setup::new();
    let hold = setup.workspace().join("hold");
    fakes::fifo(&hold);
    // Held open read+write, as the running setup does, so both parked
    // `head` calls stay alive through every view, hint and steer
    // assertion until the test drops it.
    let release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&hold)
        .unwrap();
    let guard = fakes::Watchdog::matching(hold.to_str().unwrap());
    let command = format!("head -n 1 {}", hold.display());
    write_json(
        &setup.workspace().join("d.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": command}}]},
            {"text": ["Reviewed."]},
        ]}),
    );
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "delegate_spawn", "arguments": {
                "description": "review the parser", "prompt": "go",
                "model": "fiber:scripted/d.json"}}],
             "usage": {"input": 1200}},
            {"tool_calls": [{"name": "shell", "arguments": {
                "command": command, "tty": true, "run_in_background": true}}]},
            {"text": ["Done."]},
        ]}),
    );
    write_json(
        &setup.workspace().join("r.json"),
        &json!({"steps": [
            {"text": ["allow"]}, {"text": ["allow"]},
            {"text": ["allow"]}, {"text": ["allow"]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json", "reviewer": {"model": "scripted/r.json"},
            "roles": {"delegate": "fiber:scripted/d.json"},
            "hub": {"idle_exit_ms": 1000}
        }),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
            json!({"decision": "allow", "tool": "shell", "prefix": command}),
        ),
    )
    .unwrap();
    (setup, release, guard)
}

/// The binary started at 160x48 and prompted, so the delegate spawns
/// and the tty job starts.
fn start_run(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    run
}

/// The first row containing `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    grid.rows
        .iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
        .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents))
}

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing.
fn col_at_or(grid: &Grid, y: u16, needle: &str) -> Option<u16> {
    let want: Vec<char> = needle.chars().collect();
    if want.is_empty() {
        return None;
    }
    let (width, _) = grid.size();
    (0..width).find(|x| {
        want.iter().enumerate().all(|(dx, ch)| {
            let xi = x + dx as u16;
            xi < width && grid.cell(xi, y).symbol == ch.to_string()
        })
    })
}

/// The column where `needle` starts on `y`, or a panic.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    col_at_or(grid, y, needle).unwrap_or_else(|| panic!("{needle:?} on row {y}"))
}

/// Every cell of `needle` on `y` is bold, or a panic naming it.
fn assert_bold(grid: &Grid, y: u16, needle: &str) {
    let x = col_at(grid, y, needle);
    for (dx, ch) in needle.chars().enumerate() {
        let cell = grid.cell(x + dx as u16, y);
        assert_eq!(cell.symbol, ch.to_string(), "cell {}", x + dx as u16);
        assert!(cell.bold, "bold {} on row {y}", x + dx as u16);
    }
}

/// Every non-blank cell of `needle` on `y` is dim, or a panic naming it.
fn assert_dim(grid: &Grid, y: u16, needle: &str) {
    let x = col_at(grid, y, needle);
    for (dx, ch) in needle.chars().enumerate() {
        let cell = grid.cell(x + dx as u16, y);
        assert_eq!(cell.symbol, ch.to_string(), "cell {}", x + dx as u16);
        if ch != ' ' {
            assert!(cell.dim, "dim {} on row {y}", x + dx as u16);
        }
    }
}
