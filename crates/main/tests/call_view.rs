//! A call's swapped view end to end (`docs/tui.md`, "Swapped views"):
//! the real binary on a 160x48 pty in truecolour, with a scripted
//! provider. A shell call cut at both ends opens its view, which pages
//! to the end through `artifact_page` with both markers. The SGR pins
//! assert on the grid itself.

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

use insta::assert_snapshot;
use serde_json::json;
use support::journey::grid_text;
use support::pty::{Colour, Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// A turn card's background in truecolour.
const TURN: Colour = Colour::Rgb(16, 16, 23);

/// The scripted turn: `steps`, then done.
fn script(setup: &Setup, steps: serde_json::Value) {
    write_json(&setup.workspace().join("s.json"), &json!({"steps": steps}));
}

/// The scripted provider as the model, the rail hidden. The hub stays
/// past the ledger browsing: the view pages its artifact through it.
fn config(setup: &Setup) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"rail": {"shown": false}}}),
    );
}

/// Standing rules allowing every shell command and edit, so no approval
/// panel stops the turns.
fn allow(setup: &Setup) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": ""}),
            json!({"decision": "allow", "tool": "edit", "prefix": ""}),
        ),
    )
    .unwrap();
}

/// The binary started and prompted, returning the run and the output
/// length the turn's end is waited on.
fn started(setup: &Setup) -> (Run, usize) {
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    let from = run.output().len();
    (run, from)
}

/// Answers any approval, then waits for the reply `done`.
fn finish(run: &mut Run, done: &str) {
    loop {
        let grid = run.wait_screen("approval or done", |grid| {
            grid.contents.contains("Allow once") || grid.contents.contains(done)
        });
        if grid.contents.contains(done) {
            return;
        }
        run.write(b"\r");
    }
}

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
}

/// The smallest numbered output row visible in `grid`.
fn first_output_number(grid: &Grid) -> Option<u16> {
    grid.rows
        .iter()
        .filter_map(|row| row.trim_start().get(..3)?.parse().ok())
        .min()
}

/// The row's cells as text, indexed by cell column.
fn row_text(grid: &Grid, y: u16) -> String {
    (0..160).map(|x| grid.cell(x, y).symbol.clone()).collect()
}

/// A click at cell (`x`, `y`).
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// The ledger row whose kind cell reads `kind` followed by `target`.
fn ledger_row(grid: &Grid, gutter: u16, kind: &str, target: &str) -> u16 {
    let padded = format!("{kind:<9}");
    (0..48)
        .find(|y| {
            let row = row_text(grid, *y);
            let chars: Vec<char> = row.chars().collect();
            chars.get(gutter as usize + 8..gutter as usize + 17)
                == Some(&padded.chars().collect::<Vec<char>>()[..])
                && chars
                    .get(gutter as usize + 17..)
                    .map(|tail| tail.iter().collect::<String>().starts_with(target))
                    .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("the {kind} {target} row"))
}

/// Every scripted step thinks "**Doing**", so its thought row marks
/// the open ledger.
const REASONING: &str = "**Doing**";

/// The turn card's edges on row `y`: the maximal run of card background
/// around the inner column, with blank padding cells at both ends. The
/// edges are derived from the grid, never hard-coded.
fn card_edges(grid: &Grid, y: u16, inner: u16) -> (u16, u16) {
    let mut left = inner;
    while left > 0 && grid.cell(left - 1, y).bg == TURN {
        left -= 1;
    }
    let mut right = inner;
    while grid.cell(right + 1, y).bg == TURN {
        right += 1;
    }
    assert_eq!(grid.cell(left, y).symbol.as_str(), " ");
    assert_eq!(grid.cell(right, y).symbol.as_str(), " ");
    (left, right)
}

/// Opens the ledger and returns the grid with its gutter, derived from
/// the group row.
fn open_ledger(run: &mut Run) -> (Grid, u16) {
    run.write(b"\x0f");
    let grid = run.wait_screen("the open ledger", |grid| {
        grid.rows.iter().any(|row| row.contains("+ Thought: Doing"))
    });
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, _right) = card_edges(&grid, y, 2);
    (grid, left + 1)
}

/// Clicks the ledger row (`kind`, `target`) and waits for `marker`.
fn open_call(run: &mut Run, gutter: u16, kind: &str, target: &str, marker: &str) -> Grid {
    let grid = run.wait_screen("the open ledger", |grid| {
        grid.rows.iter().any(|row| row.contains("+ Thought: Doing"))
    });
    let row = ledger_row(&grid, gutter, kind, target);
    click(run, gutter + 10, row);
    run.wait_screen("the opened call", |grid| {
        grid.rows.iter().any(|row| row.contains(marker))
    })
}

/// Clicks the more row of the opened call and waits for the view's end
/// marker: following pages to the end through `artifact_page`.
fn open_view_by_click(run: &mut Run) -> Grid {
    let grid = run.wait_screen("the opened call", |grid| {
        grid.rows.iter().any(|row| row.contains("more lines"))
    });
    let row = find_row(&grid.rows, "more lines").expect("the more row");
    let text = row_text(&grid, row);
    let x = text.find(|c: char| !c.is_whitespace()).unwrap_or(20) as u16 + 1;
    click(run, x, row);
    // Following pages to the end: the resume marker draws at the tail.
    // (The head page's end marker pages out of the three-page window, as
    // scrolling back up shows.)
    run.wait_screen("the call's view", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("── the model's view resumes ──"))
    })
}

/// The top row names the capitalised kind bold and its target dim.
fn assert_top_row(grid: &Grid, kind: &str, target: &str) {
    let row = row_text(grid, 0);
    let kind_at = row
        .find(kind)
        .unwrap_or_else(|| panic!("the {kind} title in {row:?}"));
    for (i, _) in kind.char_indices() {
        assert!(
            grid.cell(kind_at as u16 + i as u16, 0).bold,
            "the kind draws bold"
        );
    }
    let target_at = row
        .find(target)
        .unwrap_or_else(|| panic!("the {target} target in {row:?}"));
    for (i, _) in target.char_indices() {
        assert!(
            grid.cell(target_at as u16 + i as u16, 0).dim,
            "the target draws dim"
        );
    }
}

#[test]
fn a_cut_shell_call_pages_to_the_end_with_both_markers() {
    let setup = Setup::new();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "shell", "arguments": {"command": "awk 'BEGIN { for (i = 1; i < 500; i++) print sprintf(\"%03d %220s\", i, \"x\"); print \"TAIL\" }'"}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    allow(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    open_call(&mut run, gutter, "shell", "awk", "exit 0");
    // The more row opens the view with one click; following pages to the
    // end through `artifact_page`, where the resume marker draws.
    let grid = open_view_by_click(&mut run);
    let last = "TAIL";
    assert!(
        grid.rows.iter().any(|row| row.contains(last)),
        "the tail paged to the end"
    );
    assert!(
        grid.rows
            .iter()
            .any(|row| row.contains("── the model's view resumes ──")),
        "the resume marker draws"
    );
    assert!(
        grid.rows
            .iter()
            .any(|row| row.contains("y copies the whole call")),
        "the footer names the copy key"
    );
    assert_top_row(&grid, "Shell", "awk");
    let temp_id = setup
        .workspace()
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_snapshot!(
        "call_view_open",
        grid_text(&grid.rows, grid.cursor, grid.hide_cursor, &[temp_id])
    );
    let mut grid = grid;
    let mut first = first_output_number(&grid).unwrap_or(u16::MAX);
    for _ in 0..40 {
        if grid
            .rows
            .iter()
            .any(|row| row.contains("── end of the model's view ──"))
        {
            break;
        }
        // The first PageUp moves selection to the top of the visible
        // window; the second scrolls that window upward.
        run.write(b"\x1b[5~");
        run.write(b"\x1b[5~");
        grid = run.wait_screen("the scrolled view", |grid| {
            grid.rows
                .iter()
                .any(|row| row.contains("── end of the model's view ──"))
                || first_output_number(grid).is_some_and(|at| at < first)
        });
        first = first_output_number(&grid).unwrap_or(first);
    }
    assert!(
        grid.rows
            .iter()
            .any(|row| row.contains("── end of the model's view ──")),
        "the end marker draws on the head page"
    );
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
