//! Binary-level test of the pinned call row (`docs/tui.md`, "Pinned
//! call"): the real binary on a 160x48 pty in truecolour, with a
//! scripted provider. Each scenario opens the ledger and a `read` call
//! with a long tail below it, scrolls the call row off the top, and
//! asserts the pin's text, tint and click on the real frames.

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
use support::pty::{Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| u16::try_from(y).expect("the screen fits in a u16"))
}

/// The row's cells as text, indexed by cell column.
fn row_text(grid: &Grid, y: u16) -> String {
    (0..160).map(|x| grid.cell(x, y).symbol.clone()).collect()
}

/// A click at cell (`x`, `y`).
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// The scripted turn: a `read` of 300 lines with a 60-item tail below
/// it, so the scrolled conversation holds the call row off the top.
fn script(setup: &Setup) {
    fs::write(
        setup.workspace().join("notes.txt"),
        (1..=300).map(|n| format!("L{n:04}\n")).collect::<String>(),
    )
    .unwrap();
    let tail = (1..=60)
        .map(|n| format!("- T{n:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"reasoning": "**Doing**",
             "tool_calls": [{"name": "read", "arguments": {"path": "notes.txt"}}]},
            {"text": [tail, "Done."]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"rail": {"shown": false}}}),
    );
}

/// The binary started and prompted, with the turn done.
fn started(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    loop {
        let grid = run.wait_screen("approval or done", |grid| {
            grid.contents.contains("Allow once") || grid.contents.contains("Done.")
        });
        if grid.contents.contains("Done.") {
            return run;
        }
        run.write(b"\r");
    }
}

/// The ledger opened and the `read` call clicked open: the grid with
/// the more row.
fn opened(run: &mut Run) -> Grid {
    run.write(b"\x0f");
    // The tail holds the bottom: wheel to the top so the ledger rows
    // draw on screen.
    run.write("\x1b[<64;21;11M".repeat(30).as_bytes());
    let grid = run.wait_screen("the open ledger", |grid| {
        grid.rows.iter().any(|row| row.contains("+ Thought: Doing"))
    });
    let row = find_row(&grid.rows, "notes.txt").expect("the ledger row");
    let col = grid.rows[usize::from(row)]
        .find("notes.txt")
        .expect("the ledger row");
    click(run, u16::try_from(col).unwrap(), row);
    run.wait_screen("the opened call", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains(" 280 more lines (Enter to view)"))
    });
    // Back to the bottom: the pin tests scroll up from following.
    run.write(b"\x1b[F");
    run.wait_screen("the followed bottom", |grid| {
        grid.rows.iter().any(|row| row.contains("T0060"))
    })
}

/// Whether the covered row proves the pin: a genuinely placed call
/// row always shows the first output line below it, so the second line
/// or later, or the more row, means the top sits inside the opened
/// output with the call row pinned over it.
fn covered_proves_pin(row: &str) -> bool {
    if row.contains("more lines") {
        return true;
    }
    row.split_whitespace()
        .find_map(|token| token.strip_prefix('L'))
        .is_some_and(|digits| digits.parse::<u32>().is_ok_and(|line| line >= 2))
}

/// The pin row: the call row over a covered row that proves the top
/// sits inside the opened output. Scrolled up from the bottom, the
/// first such pair is the pin: a genuinely placed call row shows the
/// first output line below it.
fn pin_row(grid: &Grid) -> Option<usize> {
    (0..10).find(|i| {
        grid.rows
            .get(*i)
            .is_some_and(|row| row.contains("notes.txt"))
            && grid
                .rows
                .get(i + 1)
                .is_some_and(|row| covered_proves_pin(row))
    })
}

/// Whether the grid holds the pin.
fn pinned(grid: &Grid) -> bool {
    pin_row(grid).is_some()
}

/// One wheel step up at 160x48.
const UP: &[u8] = b"\x1b[<64;21;11M";
/// One wheel step down at 160x48.
const DOWN: &[u8] = b"\x1b[<65;21;40M";

/// Wheels up one step at a time until the pin shows: the step count
/// stays below the clamp, so every step moves the top row.
fn wheel_to_pin(run: &mut Run) {
    let mut prev = run.screen().rows.clone();
    for _ in 0..12 {
        run.write(UP);
        let grid = run.wait_screen("one step up", |next| next.rows != prev);
        if pinned(&grid) {
            return;
        }
        prev = grid.rows.clone();
    }
}

/// Every text cell of the pin row shares one background: the pin keeps
/// the call card's tint over the covered row.
fn assert_pinned_tint(grid: &Grid, y: u16) {
    let mut bg = None;
    let mut text = 0;
    for x in 1..100 {
        let cell = grid.cell(x, y);
        if cell.symbol.as_str() != " " {
            let bg = *bg.get_or_insert(cell.bg);
            assert_eq!(cell.bg, bg, "the pin tints cell {x}");
            text += 1;
        }
    }
    assert!(text > 10, "the pin row holds the call row");
}

#[test]
fn the_pin_stays_over_the_opened_content_while_scrolling() {
    let setup = Setup::new();
    script(&setup);
    let mut run = started(&setup);
    opened(&mut run);
    // Twelve wheel-ups move the top into the opened output: the call
    // row scrolls off, and its pin stays on row 0.
    wheel_to_pin(&mut run);
    let grid = run.wait_screen("the pinned call row", pinned);
    let pin = u16::try_from(pin_row(&grid).expect("the pin row")).unwrap();
    assert!(
        row_text(&grid, pin).contains("notes.txt"),
        "the pin row holds the call row"
    );
    assert!(
        covered_proves_pin(&row_text(&grid, pin + 1)),
        "the next row is the covered content row"
    );
    assert_pinned_tint(&grid, pin);
    // The input box keeps its row: the pin never overlaps it.
    assert!(grid.contents.contains("›"), "the input box draws");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn paging_up_pins_the_same_lead_row_with_its_tint() {
    let setup = Setup::new();
    script(&setup);
    let mut run = started(&setup);
    opened(&mut run);
    // A different path to the same wheel assertion: one page up lands
    // near the opened output, then wheeling down settles the top
    // inside it.
    run.write(b"\x1b[5~");
    for _ in 0..10 {
        let grid = run.screen();
        if pinned(&grid) {
            break;
        }
        run.write(DOWN);
        run.wait_screen("one step down", |next| next.rows != grid.rows);
    }
    let grid = run.wait_screen("the pinned call row", pinned);
    let pin = u16::try_from(pin_row(&grid).expect("the pin row")).unwrap();
    assert!(
        row_text(&grid, pin).contains("notes.txt"),
        "the pin row holds the call row"
    );
    assert_pinned_tint(&grid, pin);
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn clicking_the_pin_closes_the_call_and_lands_its_row_on_top() {
    let setup = Setup::new();
    script(&setup);
    let mut run = started(&setup);
    opened(&mut run);
    wheel_to_pin(&mut run);
    let grid = run.wait_screen("the pinned call row", pinned);
    // A click on the pin's cells closes the call: its row lands on the
    // top row under the ledger's summary pin.
    let pin = u16::try_from(pin_row(&grid).expect("the pin row")).unwrap();
    let col = row_text(&grid, pin).find("notes.txt").expect("the pin row");
    click(&mut run, u16::try_from(col).unwrap(), pin);
    let grid = run.wait_screen("the closed call row", |grid| {
        !grid.contents.contains("L00")
            && grid
                .rows
                .get(usize::from(pin))
                .is_some_and(|row| row.contains('▾'))
    });
    // The landed call row hides under the summary pin, so the round
    // trip proves it: closing the ledger and reopening it shows the
    // thought and the still-closed call row at the ledger's top.
    let col = row_text(&grid, pin)
        .find("Read 1 file")
        .expect("the summary pin");
    click(&mut run, u16::try_from(col).unwrap(), pin);
    let grid = run.wait_screen("the closed ledger", |grid| {
        grid.rows.iter().any(|row| {
            row.contains("Read 1 file") && row.contains('\u{25b8}') && !row.contains("+ Thought:")
        })
    });
    let shut = find_row(&grid.rows, "Read 1 file").expect("the closed summary");
    let col = row_text(&grid, shut)
        .find("Read 1 file")
        .expect("the closed summary");
    click(&mut run, u16::try_from(col).unwrap(), shut);
    let grid = run.wait_screen("the reopened ledger", |grid| {
        grid.contents.contains("+ Thought: Doing")
            && grid.contents.contains("notes.txt")
            && !grid.contents.contains("L00")
    });
    assert!(
        grid.contents.contains("notes.txt"),
        "the landed call row draws at the ledger's top"
    );
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn wheeling_past_the_call_clears_the_pin_and_wheeling_back_restores_it() {
    let setup = Setup::new();
    script(&setup);
    let mut run = started(&setup);
    opened(&mut run);
    wheel_to_pin(&mut run);
    run.wait_screen("the pinned call row", pinned);
    // Wheeling to the bottom follows again: the call row draws once,
    // on its own row, and row 0 is the window's own first row.
    for _ in 0..20 {
        let grid = run.screen();
        if pin_row(&grid).is_none() && grid.rows.iter().any(|row| row.contains("T0060")) {
            break;
        }
        run.write(DOWN);
        run.wait_screen("one step down", |next| next.rows != grid.rows);
    }
    let grid = run.wait_screen("the followed bottom", |grid| {
        pin_row(grid).is_none() && grid.rows.iter().any(|row| row.contains("T0060"))
    });
    assert!(
        grid.rows.iter().all(|row| !row.contains("notes.txt")),
        "the call row scrolled off with no pin over it"
    );
    // Wheeling back up past the call pins it again.
    wheel_to_pin(&mut run);
    let grid = run.wait_screen("the pinned call row", pinned);
    let pin = u16::try_from(pin_row(&grid).expect("the pin row")).unwrap();
    assert_pinned_tint(&grid, pin);
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
