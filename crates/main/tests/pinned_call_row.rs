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
use support::pty::{Colour, Grid, Run};
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

/// The binary started and prompted, with the turn finished: the
/// finished title at its output offset plus the completed paint, so
/// later wheel steps cannot race the turn's own frames.
fn started(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    let from = run.ready();
    run.write(b"go\r");
    loop {
        let grid = run.wait_screen("approval or done", |grid| {
            grid.contents.contains("Allow once") || grid.contents.contains("Done.")
        });
        if grid.contents.contains("Done.") {
            break;
        }
        run.write(b"\r");
    }
    run.turn_finished(from);
    run
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

/// Whether `next` shows the viewport scrolled one wheel step from
/// `prev`: an anchor row sitting alone in each sits further along the
/// step, down the screen for a step up and up it for a step down. A
/// repaint leaves every anchor where it was and an earlier step's frame
/// leaves them behind, so neither passes: only a frame with this step's
/// scroll moves an anchor along the step. Rows the step covered or
/// uncovered, rows it leaves still, and rows repeating on screen prove
/// nothing either way.
fn scrolled(prev: &[String], next: &[String], up: bool) -> bool {
    for (i, anchor) in prev.iter().enumerate() {
        if prev.iter().filter(|row| *row == anchor).count() != 1 {
            continue;
        }
        let mut at = next.iter().enumerate().filter(|(_, row)| *row == anchor);
        let (Some((j, _)), None) = (at.next(), at.next()) else {
            continue;
        };
        if j != i && (j > i) == up {
            return true;
        }
    }
    false
}

#[test]
fn scrolled_rejects_a_repaint() {
    let grid = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    assert!(!scrolled(&grid, &grid.clone(), true));
    assert!(!scrolled(&grid, &grid.clone(), false));
}

#[test]
fn scrolled_up_needs_movement_down_the_screen() {
    let prev = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let next = vec!["n".to_string(), "a".to_string(), "b".to_string()];
    assert!(scrolled(&prev, &next, true));
    assert!(!scrolled(&prev, &next, false));
}

#[test]
fn scrolled_down_needs_movement_up_the_screen() {
    let prev = vec!["a".to_string(), "b".to_string(), "c".to_string()];
    let next = vec!["b".to_string(), "c".to_string(), "n".to_string()];
    assert!(scrolled(&prev, &next, false));
    assert!(!scrolled(&prev, &next, true));
}

#[test]
fn scrolled_ignores_repeated_missing_and_still_anchors() {
    // `a` repeats, `gone` never arrives, `b` sits still: none proves a step.
    let prev = vec!["a".to_string(), "a".to_string(), "b".to_string()];
    let next = vec!["a".to_string(), "a".to_string(), "b".to_string()];
    assert!(!scrolled(&prev, &next, true));
    let prev = vec!["gone".to_string(), "b".to_string()];
    let next = vec!["new".to_string(), "b".to_string()];
    assert!(!scrolled(&prev, &next, true));
    assert!(!scrolled(&prev, &next, false));
}

/// One wheel step up at 160x48.
const UP: &[u8] = b"\x1b[<64;21;11M";
/// One wheel step down at 160x48.
const DOWN: &[u8] = b"\x1b[<65;21;40M";

/// Wheels up one step at a time until the pin shows: the step count
/// stays below the clamp, so every step moves the top row. Each wait
/// proves its own step moved the rows along the step, never any row
/// change at all, or the pin it was wheeling towards.
fn wheel_to_pin(run: &mut Run) {
    let mut prev = run.screen().rows.clone();
    for _ in 0..12 {
        run.write(UP);
        let grid = run.wait_screen("one step up", |next| {
            scrolled(&prev, &next.rows, true) || pinned(next)
        });
        if pinned(&grid) {
            return;
        }
        prev = grid.rows.clone();
    }
}

/// Whether the grid shows the followed bottom: no pin over it, with
/// the tail's last row on screen.
fn followed_bottom(grid: &Grid) -> bool {
    pin_row(grid).is_none() && grid.rows.iter().any(|row| row.contains("T0060"))
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

/// The first row holding `needle` whose next row `proves` the top sits
/// past the header: the pin candidate.
fn pin_row_for(grid: &Grid, needle: &str, proves: fn(&str) -> bool) -> Option<usize> {
    (0..10).find(|i| {
        grid.rows.get(*i).is_some_and(|row| row.contains(needle))
            && grid.rows.get(i + 1).is_some_and(|row| proves(row))
    })
}

/// The pinned row holding `needle`: its row holds the needle, the row
/// below proves the top sits inside opened content, and every text cell
/// shares the card's tint. Returns the pin's row.
fn assert_pinned(grid: &Grid, needle: &str) -> u16 {
    let pin =
        u16::try_from(pin_row_for(grid, needle, covered_proves_pin).expect("the pin row")).unwrap();
    assert!(
        row_text(grid, pin).contains(needle),
        "the pin row holds the call row"
    );
    assert!(
        covered_proves_pin(&row_text(grid, pin + 1)),
        "the next row is the covered content row"
    );
    assert_pinned_tint(grid, pin);
    pin
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
    let _pin = assert_pinned(&grid, "notes.txt");
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
        run.wait_screen("one step down", |next| {
            scrolled(&grid.rows, &next.rows, false) || pinned(next)
        });
    }
    let grid = run.wait_screen("the pinned call row", pinned);
    let _pin = assert_pinned(&grid, "notes.txt");
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
    let pin = assert_pinned(&grid, "notes.txt");
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
        if followed_bottom(&grid) {
            break;
        }
        run.write(DOWN);
        run.wait_screen("one step down", |next| {
            scrolled(&grid.rows, &next.rows, false) || followed_bottom(next)
        });
    }
    let grid = run.wait_screen("the followed bottom", followed_bottom);
    assert!(
        grid.rows.iter().all(|row| !row.contains("notes.txt")),
        "the call row scrolled off with no pin over it"
    );
    // Wheeling back up past the call pins it again.
    wheel_to_pin(&mut run);
    let grid = run.wait_screen("the pinned call row", pinned);
    let _pin = assert_pinned(&grid, "notes.txt");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The current match's background.
const CURRENT: Colour = Colour::Rgb(255, 159, 67);

/// The scripted turn: thirty `read` calls, so the open ledger is tall
/// enough that wheeling up from the bottom passes through a pinned group
/// line before the call rows scroll off.
fn script_group(setup: &Setup) {
    for n in 0..30 {
        fs::write(setup.workspace().join(format!("f{n:02}.txt")), "one\ntwo\n").unwrap();
    }
    let mut calls = Vec::new();
    for n in 0..30 {
        calls.push(json!({"name": "read", "arguments": {"path": format!("f{n:02}.txt")}}));
    }
    let tail = (1..=60)
        .map(|n| format!("- T{n:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"reasoning": "**Doing**", "tool_calls": calls},
            {"text": [tail, "Done."]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"rail": {"shown": false}}}),
    );
}

/// The scripted turn: two `read` calls on different files, so two call
/// rows can open in one ledger.
fn script_two(setup: &Setup) {
    for name in ["alpha.txt", "bravo.txt"] {
        fs::write(
            setup.workspace().join(name),
            (1..=300).map(|n| format!("L{n:04}\n")).collect::<String>(),
        )
        .unwrap();
    }
    let tail = (1..=60)
        .map(|n| format!("- T{n:04}"))
        .collect::<Vec<_>>()
        .join("\n");
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"reasoning": "**Doing**",
             "tool_calls": [
                {"name": "read", "arguments": {"path": "alpha.txt"}},
                {"name": "read", "arguments": {"path": "bravo.txt"}},
            ]},
            {"text": [tail, "Done."]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"rail": {"shown": false}}}),
    );
}

/// Ctrl+O opens the ledger, then End follows the bottom: the starting
/// point for wheeling up into a pin.
fn opened_ledger(run: &mut Run) -> Grid {
    run.write(b"\x0f");
    // The tail holds the bottom: wheel to the top so the ledger rows
    // draw on screen.
    run.write("\x1b[<64;21;11M".repeat(30).as_bytes());
    run.wait_screen("the open ledger", |grid| {
        grid.rows.iter().any(|row| row.contains("+ Thought: Doing"))
    });
    run.write(b"\x1b[F");
    run.wait_screen("the followed bottom", |grid| {
        grid.rows.iter().any(|row| row.contains("T0060"))
    })
}

/// Wheels up one step at a time until `pred` holds; every step must move.
/// Each wait proves its own step moved the rows along the step, or the
/// goal it was wheeling towards.
fn wheel_until(run: &mut Run, what: &str, pred: impl Fn(&Grid) -> bool) {
    let mut prev = run.screen().rows.clone();
    for _ in 0..80 {
        run.write(UP);
        let grid = run.wait_screen("one step up", |next| {
            scrolled(&prev, &next.rows, true) || pred(next)
        });
        if pred(&grid) {
            return;
        }
        prev = grid.rows.clone();
    }
    panic!("{what} never showed");
}

/// The pinned group summary: row 0 names the ledger with its open toggle,
/// and the row below is a ledger row, which proves the top sits past the
/// summary. Returns the pin's row.
fn assert_group_pinned(grid: &Grid, summary: &str) -> u16 {
    let pin =
        u16::try_from(pin_row_for(grid, summary, |row| row.contains('✓')).expect("the group pin"))
            .unwrap();
    assert!(
        row_text(grid, pin).contains('▾'),
        "the pin is the open ledger's group line"
    );
    assert_pinned_tint(grid, pin);
    pin
}

#[test]
fn the_group_line_pins_while_no_call_is_opened() {
    let setup = Setup::new();
    script_group(&setup);
    let mut run = started(&setup);
    opened_ledger(&mut run);
    // Wheel up until the open ledger's group line pins over a ledger row.
    wheel_until(&mut run, "the pinned group line", |grid| {
        pin_row_for(grid, "Read 30 files", |row| row.contains('✓')).is_some()
    });
    let grid = run.wait_screen("the pinned group line", |grid| {
        pin_row_for(grid, "Read 30 files", |row| row.contains('✓')).is_some()
    });
    let pin = assert_group_pinned(&grid, "Read 30 files");
    // The pin persists as the top moves deeper into the ledger.
    for _ in 0..3 {
        let prev = run.screen().rows.clone();
        run.write(UP);
        // The pin persists through every frame here, so only the step's
        // own scroll proves the wait: a pin check would pass on any frame.
        run.wait_screen("one step up", |next| scrolled(&prev, &next.rows, true));
    }
    let grid = run.screen();
    assert_group_pinned(&grid, "Read 30 files");
    // A click on the pin closes the ledger: its rows are gone and the
    // summary shows closed.
    let col = row_text(&grid, pin)
        .find("Read 30 files")
        .expect("the pin row");
    click(&mut run, u16::try_from(col).unwrap(), pin);
    let grid = run.wait_screen("the closed ledger", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("Read 30 files") && row.contains('▸'))
    });
    assert!(
        !grid.contents.contains("f00.txt"),
        "the closed ledger hides its rows"
    );
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn the_call_row_wins_over_the_group_line_inside_an_open_ledger() {
    let setup = Setup::new();
    script(&setup);
    let mut run = started(&setup);
    opened(&mut run);
    // The pin is the call's row, not the open ledger's group line.
    wheel_to_pin(&mut run);
    let grid = run.wait_screen("the pinned call row", pinned);
    let pin = assert_pinned(&grid, "notes.txt");
    assert!(
        !row_text(&grid, pin).contains('▾') && !row_text(&grid, pin).contains('▸'),
        "the innermost opened row pins: {:?}",
        row_text(&grid, pin)
    );
    // Clicking the pin closes only the call: the ledger stays open,
    // with its summary pinning the closed call's row.
    let col = row_text(&grid, pin).find("notes.txt").expect("the pin row");
    click(&mut run, u16::try_from(col).unwrap(), pin);
    let grid = run.wait_screen("the closed call", |grid| {
        !grid.contents.contains("L00")
            && grid
                .rows
                .get(usize::from(pin))
                .is_some_and(|row| row.contains('▾'))
    });
    // The ledger stays open around the closed call: its summary still
    // names the ledger with the open toggle.
    assert!(
        row_text(&grid, pin).contains("Read 1 file"),
        "the summary pins with the ledger open: {:?}",
        row_text(&grid, pin)
    );
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn wheeling_up_pins_each_opened_call_in_turn() {
    let setup = Setup::new();
    script_two(&setup);
    let mut run = started(&setup);
    // Open the ledger and both call rows, then follow the bottom.
    run.write(b"\x0f");
    run.write("\x1b[<64;21;11M".repeat(30).as_bytes());
    let mut grid = run.wait_screen("the open ledger", |grid| {
        grid.rows.iter().any(|row| row.contains("+ Thought: Doing"))
            && grid.rows.iter().any(|row| row.contains("alpha.txt"))
            && grid.rows.iter().any(|row| row.contains("bravo.txt"))
    });
    for name in ["alpha.txt", "bravo.txt"] {
        let at = find_row(&grid.rows, name).expect("the call row");
        let row = row_text(&grid, at);
        click(
            &mut run,
            u16::try_from(row.find(name).unwrap()).unwrap(),
            at,
        );
        grid = run.wait_screen("the opened call", |grid| {
            grid.contents.contains("more lines")
        });
    }
    run.write(b"\x1b[F");
    run.wait_screen("the followed bottom", |grid| {
        grid.rows.iter().any(|row| row.contains("T0060"))
    });
    // Wheeling up pins the later call first, then the earlier one: each
    // pin is its own call's row over its own covered output.
    wheel_until(&mut run, "the bravo.txt pin", |grid| {
        pin_row_for(grid, "bravo.txt", covered_proves_pin).is_some()
    });
    let grid = run.wait_screen("the bravo.txt pin", |grid| {
        pin_row_for(grid, "bravo.txt", covered_proves_pin).is_some()
    });
    assert_pinned(&grid, "bravo.txt");
    wheel_until(&mut run, "the alpha.txt pin", |grid| {
        pin_row_for(grid, "alpha.txt", covered_proves_pin).is_some()
    });
    let grid = run.wait_screen("the alpha.txt pin", |grid| {
        pin_row_for(grid, "alpha.txt", covered_proves_pin).is_some()
    });
    assert_pinned(&grid, "alpha.txt");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn search_lands_the_current_match_on_the_row_under_the_pin() {
    let setup = Setup::new();
    script(&setup);
    let mut run = started(&setup);
    opened(&mut run);
    wheel_to_pin(&mut run);
    let grid = run.wait_screen("the pinned call row", pinned);
    let _pin = assert_pinned(&grid, "notes.txt");
    // A search for a shown output line above the top lands its match
    // under the pin, with the pin staying.
    run.write(b"\x06L0005");
    let grid = run.wait_screen("the current match under the pin", |grid| {
        let Some(pin) = pin_row(grid) else {
            return false;
        };
        (pin + 1..48).any(|y| {
            let y = u16::try_from(y).unwrap();
            row_text(grid, y).contains("L0005") && (0..160).any(|x| grid.cell(x, y).bg == CURRENT)
        })
    });
    let pin = assert_pinned(&grid, "notes.txt");
    assert!(
        row_text(&grid, pin + 1).contains("L0005"),
        "the match lands under the pin: {:?}",
        row_text(&grid, pin + 1)
    );
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
