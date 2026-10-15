//! Binary-level test of the handoff band (`docs/tui.md`, "Handoff"): the
//! real binary on a 160x48 pty in truecolour, with the scripted provider
//! running a read past the 400k automatic handoff, writing the note, then
//! answering. The band is one row on the `handoff` tint with half-block
//! edges, reading " ⇄ handoff · automatic at 400k" with the sizes right
//! ("402k → 32k") and the time dim; the card ends at the band, a second
//! card continues the turn, and its ▣ line closes it.

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

/// The dark theme's secondary and handoff colours in truecolour
/// (`docs/tui.md`, "Handoff").
const SECONDARY: Colour = Colour::Rgb(179, 157, 219);
const HANDOFF: Colour = Colour::Rgb(31, 26, 46);
/// A turn card's background in truecolour.
const TURN: Colour = Colour::Rgb(16, 16, 23);

/// The scripted turn: a read past the handoff, the note, then done with
/// the usage that sizes the band after.
fn script(setup: &Setup) {
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "read", "arguments": {"path": "a.txt"}}],
             "usage": {"input": 402000, "output": 10}},
            {"text": ["The note."]},
            {"text": ["Done."], "usage": {"input": 32000, "output": 5}},
        ]}),
    );
}

/// The sized band's row, its content columns and its sizes column:
/// the row holds "402k → 32k", the sizes cells read bold and the row
/// reads on the handoff tint. Every property the test asserts is
/// checked here, so a partial redraw cannot satisfy the wait.
fn band_cells(grid: &Grid) -> Option<(u16, usize, usize, usize)> {
    let band = find_row(&grid.rows, "402k → 32k")?;
    // The content columns from the edge run above.
    let above = row_text(grid, band.checked_sub(1)?, 160);
    let first = above.find('▄')?;
    let left = above[..first].chars().count();
    let last = above.rfind('▄')?;
    let right = above[..last].chars().count();
    let text = row_text(grid, band, 160);
    let at = text.find("402k → 32k")?;
    let sizes_at = text[..at].chars().count();
    // The sizes read bold on the handoff tint.
    let bold = grid.cell(sizes_at as u16, band).bold;
    let tint = (left..=right).all(|col| grid.cell(col as u16, band).bg == HANDOFF);
    (bold && tint).then_some((band, left, right, sizes_at))
}

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
}

/// The row's cells as text, indexed by cell column.
fn row_text(grid: &Grid, y: u16, width: u16) -> String {
    (0..width).map(|x| grid.cell(x, y).symbol.clone()).collect()
}

/// Whether `text` reads as a clock: two digits, a colon, two digits.
fn is_clock(text: &str) -> bool {
    let cells: Vec<char> = text.chars().collect();
    cells.len() == 5
        && cells[0].is_ascii_digit()
        && cells[1].is_ascii_digit()
        && cells[2] == ':'
        && cells[3].is_ascii_digit()
        && cells[4].is_ascii_digit()
}

#[test]
fn the_handoff_band_matches_the_doc() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("a.txt"), "hello\n").unwrap();
    script(&setup);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "handoff": {"tokens": 400000},
            "hub": {"idle_exit_ms": 1000}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    // The band once the request after the handoff reports its size.
    let grid = run.wait_screen("the sized band", |grid| band_cells(grid).is_some());
    let (band, left, right, sizes_at) = band_cells(&grid).expect("the sized band");
    let width = right - left + 1;
    let (left, right) = (left as u16, right as u16);
    // The lower edge matches the upper one.
    let below = row_text(&grid, band + 1, 160);
    let first = below.find('▀').expect("the lower edge");
    assert_eq!(below[..first].chars().count(), left as usize);
    let text = row_text(&grid, band, 160);
    let content: String = text.chars().skip(left as usize).take(width).collect();
    // Left to right: " ⇄ ", "handoff", then the trigger after " · ".
    let head = " ⇄ handoff · automatic at 400k";
    assert!(content.starts_with(head), "{content:?}");
    assert_eq!(content.chars().count(), width, "{content:?}");
    // "⇄" and "handoff" in secondary, "handoff" bold; the trigger dim.
    for col in left..left + 3 {
        assert_eq!(grid.cell(col, band).fg, SECONDARY, "col {col}");
    }
    for col in left + 3..left + 10 {
        assert_eq!(grid.cell(col, band).fg, SECONDARY, "col {col}");
        assert!(grid.cell(col, band).bold, "col {col}");
    }
    for col in left + 10..left + 30 {
        assert!(grid.cell(col, band).dim, "col {col}");
    }
    // Right-aligned, the sizes bold and the clock dim; the clock reads
    // digits and a colon only, never compared with a clock, so a minute
    // boundary cannot flake the test.
    let sizes = "402k → 32k";
    let tail = &content[content.find(sizes).expect("the sizes") + sizes.len()..];
    assert!(tail.starts_with(" · "), "{tail:?}");
    let clock: String = tail.chars().skip(3).take(5).collect();
    assert!(is_clock(&clock), "{tail:?}");
    assert!(tail.ends_with(' '), "{tail:?}");
    for col in sizes_at..sizes_at + 10 {
        assert!(grid.cell(col as u16, band).bold, "col {col}");
    }
    for col in sizes_at + 10..sizes_at + 19 {
        assert!(grid.cell(col as u16, band).dim, "col {col}");
    }
    // The clock's last digit ends at the band's last text column; the
    // final column is the band's blank right padding.
    assert_eq!(sizes_at + 10 + 3 + 5, right as usize, "{content:?}");
    assert_eq!(grid.cell(right, band).symbol.as_str(), " ");
    assert_eq!(grid.cell(right, band).bg, HANDOFF);
    // The content row sits on the handoff tint across its width.
    for col in left..=right {
        assert_eq!(grid.cell(col, band).bg, HANDOFF, "col {col}");
    }
    // Half-block edges over the default background, above and below.
    for (row, want) in [(band - 1, "▄"), (band + 1, "▀")] {
        for col in left..=right {
            let cell = grid.cell(col, row);
            assert_eq!(cell.symbol.as_str(), want, "col {col}");
            assert_eq!(cell.fg, HANDOFF, "col {col}");
            assert_eq!(cell.bg, Colour::Default, "col {col}");
        }
    }
    // The card above the band and a second card below it both sit on
    // the turn tint; the ▣ line closes the turn once, below the band.
    // The close lands after the sized band, so it gets its own wait.
    let grid = run.wait_screen("the closed turn", |grid| {
        band_cells(grid).is_some() && grid.rows.iter().any(|row| row.contains('▣'))
    });
    let (band, left, _right, _) = band_cells(&grid).expect("the sized band");
    let left = left as u16;
    let closings: Vec<u16> = (0..48)
        .filter(|y| row_text(&grid, *y, 160).contains('▣'))
        .collect();
    assert_eq!(closings.len(), 1, "{closings:?}");
    assert!(closings[0] > band, "{closings:?}");
    assert!(
        (0..band).all(|y| !row_text(&grid, y, 160).contains('▣')),
        "no ▣ above the band"
    );
    assert!(
        (0..band).any(|y| grid.cell(left + 1, y).bg == TURN),
        "the card above sits on the turn tint"
    );
    assert_eq!(grid.cell(left + 1, closings[0]).bg, TURN);
}
