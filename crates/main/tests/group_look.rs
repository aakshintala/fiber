//! Binary-level test of tool groups and the ledger (`docs/tui.md`,
//! "Tool groups and the ledger"): the real binary on a 160x48 pty in
//! truecolour, with a scripted provider running two reads, a search, an
//! edit and a held shell call. Collapsed, the group is one row matching
//! the doc's summary mock in text and order; opened, its ledger rows
//! match the doc's ledger mock column for column; an opened read shows
//! 20 lines, then a more row.

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

/// The dark theme's attention, info, success, added and removed colours
/// in truecolour (`crates/tui/src/theme.rs`).
const ATTENTION: Colour = Colour::Rgb(255, 159, 67);
const INFO: Colour = Colour::Rgb(125, 211, 252);
const SUCCESS: Colour = Colour::Rgb(110, 170, 254);
const ADDED: Colour = Colour::Rgb(115, 201, 145);
const REMOVED: Colour = Colour::Rgb(255, 93, 115);
/// A turn card's background in truecolour.
const TURN: Colour = Colour::Rgb(16, 16, 23);

/// The working line's spinner frames (`docs/tui.md`, "The working line").
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The scripted turn: thinking between steps, two reads, a search, an
/// edit and a held shell call, then done twice.
fn script(setup: &Setup, hold: &str) {
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"reasoning": "**Reading**", "tool_calls": [
                {"name": "read", "arguments": {"path": "a.txt"}},
                {"name": "read", "arguments": {"path": "b.txt"}},
            ]},
            {"reasoning": "**Searching**", "tool_calls": [
                {"name": "shell", "arguments": {"command": "grep -n A0003 a.txt"}},
            ]},
            {"reasoning": "**Editing**", "tool_calls": [
                {"name": "edit", "arguments": {"path": "a.txt", "edits": [
                    {"old_text": "A0002\n", "new_text": "A0002\nA0002b\n"},
                ]}},
            ]},
            {"reasoning": "**Running**", "tool_calls": [
                {"name": "shell", "arguments": {"command": hold}},
            ]},
            {"text": ["Done."]},
            {"text": ["Done."]},
        ]}),
    );
}

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
}

/// The cell index of `needle` in `row`, counting cells.
fn locate(row: &str, needle: &str) -> Option<u16> {
    let chars: Vec<char> = row.chars().collect();
    let need: Vec<char> = needle.chars().collect();
    chars
        .windows(need.len().max(1))
        .position(|window| window == need.as_slice())
        .map(|at| at as u16)
}

/// The row's cells as text, indexed by cell column.
fn row_text(grid: &Grid, y: u16) -> String {
    (0..160).map(|x| grid.cell(x, y).symbol.clone()).collect()
}

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

/// The row's text between the card's padding cells, without the trailing
/// blanks.
fn content(grid: &Grid, y: u16, left: u16, right: u16) -> String {
    (left + 1..right)
        .map(|x| grid.cell(x, y).symbol.clone())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// Answers the approval on screen and waits for the held tool to run
/// with the approval panel gone.
fn run_held_tool(run: &mut Run) {
    run.wait_screen("the approval", |grid| {
        grid.rows.iter().any(|row| row.contains("Allow once"))
    });
    run.write(b"\r");
    run.wait_screen("the tool running", |grid| {
        !grid.rows.iter().any(|row| row.contains("Allow once"))
    });
}

/// Whether the cell is blank.
fn is_blank(grid: &Grid, x: u16, y: u16) -> bool {
    grid.cell(x, y).symbol.as_str() == " "
}

/// Every non-blank cell from `start` to `end` on row `y` is dim.
fn assert_dim(grid: &Grid, y: u16, start: u16, end: u16, what: &str) {
    for x in start..end {
        if !is_blank(grid, x, y) {
            assert!(grid.cell(x, y).dim, "{what} cell {x}");
        }
    }
}

/// The ledger row whose kind cell reads `kind`: the glyph, the kind in
/// info, and the target after it.
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

#[test]
fn the_group_line_and_the_ledger_match_the_doc() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    let lines = (1..=25).map(|n| format!("A{n:04}\n")).collect::<String>();
    fs::write(workspace.join("a.txt"), &lines).unwrap();
    fs::write(workspace.join("b.txt"), "B0001\nB0002\nB0003\n").unwrap();
    // The held shell call: the marker keeps it running until the test
    // has read the running rows.
    let hold = workspace.join("hold");
    fs::write(&hold, "").unwrap();
    let command = format!(
        "i=0; while [ -e '{}' ] && [ $i -lt 2400 ]; do sleep 0.05; i=$((i+1)); done # {}",
        hold.display(),
        // Longer than the 160-column row on any runner's temp path, so the
        // ledger must cut it.
        "x".repeat(120)
    );
    script(&setup, &command);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200},
            // Notes that are not empty: the reviewer-setup notice, which
            // takes the next key, does not follow the first escalation.
            "reviewer": {"context": "### Source control\n- acme is ours\n"},
            "tui": {"rail": {"shown": false}}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    run_held_tool(&mut run);

    // The collapsed group while the shell runs: one row matching the
    // doc's summary mock in text and order, cut with "…" before the
    // right part, which keeps its columns. The approval's resolve event
    // reaches the fold after the panel closes, and only then does the
    // asking call stop drawing the ledger, so the wait needs the ledger
    // closed, with ▸ keeping the last column.
    let grid = run.wait_screen("the running group", |grid| {
        grid.rows.iter().any(|row| {
            row.contains("Read 2 files") && row.contains("thought 4 times") && row.contains('…')
        }) && !grid.rows.iter().any(|row| row.contains("+ Thought:"))
    });
    let y = find_row(&grid.rows, "thought 4 times").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let gutter = left + 1;
    let text = content(&grid, y, left, right);
    // The dot spins in attention while the group runs.
    let dot = grid.cell(gutter, y);
    assert!(
        SPINNER.contains(&dot.symbol.as_str()) || dot.symbol == "●",
        "the dot spins, got {:?}",
        dot.symbol
    );
    assert_eq!(dot.fg, ATTENTION);
    // The kinds in their fixed order, then the in-flight shell.
    let after_dot: String = text.chars().skip(1).collect();
    assert!(
        after_dot.starts_with(" Read 2 files, searched 1 pattern, edited 1 file +"),
        "{text:?}"
    );
    // The edit's counts, as the tool reports them.
    let anchor = "edited 1 file +";
    let at = locate(&text, anchor).expect("the edit counts") as usize + anchor.chars().count();
    let chars: Vec<char> = text.chars().collect();
    let added: String = chars[at..]
        .iter()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    let rest: String = chars[at..].iter().collect();
    let removed: String = rest
        .split('−')
        .nth(1)
        .unwrap_or_default()
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    assert!(!added.is_empty() && !removed.is_empty(), "{text:?}");
    assert!(
        text.contains(&format!(
            "edited 1 file +{added} −{removed}, ran 1 command, thought 4 times"
        )),
        "{text:?}"
    );
    assert!(text.ends_with('▸'), "{text:?}");
    assert_eq!(grid.cell(right - 1, y).symbol.as_str(), "▸");

    // Ctrl+O opens the ledger: each step's first row is its thinking
    // row, with the step's digit in the gutter's fifth column.
    run.write(b"\x0f");
    let grid = run.wait_screen("the open ledger", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("+ Thought: Reading"))
    });
    let y = find_row(&grid.rows, "thought 4 times").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let gutter = left + 1;
    assert_eq!(grid.cell(right - 1, y).symbol.as_str(), "▾");
    for (step, heading) in [
        (1u16, "Reading"),
        (2u16, "Searching"),
        (3u16, "Editing"),
        (4u16, "Running"),
    ] {
        let row = find_row(&grid.rows, &format!("+ Thought: {heading}"))
            .unwrap_or_else(|| panic!("step {step}'s thinking row"));
        let digit = grid.cell(gutter + 4, row);
        assert_eq!(digit.symbol.as_str(), &step.to_string());
        assert!(digit.dim, "step {step}'s digit is dim");
    }
    // The ledger rows match the doc's mock column for column: the step
    // number right-aligned in five columns, the glyph, the kind in info
    // padded to nine, the target, the result.
    let check_row = |grid: &Grid, kind: &str, target: &str, result: &str, glyph: &str| -> u16 {
        let y = ledger_row(grid, gutter, kind, target);
        // A step's later rows carry a blank six-column gutter.
        for x in gutter..gutter + 6 {
            assert_eq!(grid.cell(x, y).symbol.as_str(), " ", "gutter ({x}, {y})");
        }
        let mark = grid.cell(gutter + 6, y);
        assert_eq!(mark.symbol.as_str(), glyph);
        for x in gutter + 8..gutter + 17 {
            assert_eq!(grid.cell(x, y).fg, INFO, "kind ({x}, {y})");
        }
        let row = row_text(grid, y);
        assert!(content(grid, y, left, right).ends_with(result), "{row}");
        y
    };
    // Both reads completed: their line counts are dim results.
    for (target, result) in [("a.txt", "25 lines"), ("b.txt", "3 lines")] {
        let y = check_row(&grid, "read", target, result, "✓");
        assert_eq!(grid.cell(gutter + 6, y).fg, SUCCESS);
        let row = row_text(&grid, y);
        let at = locate(&row, result).expect("the result");
        assert_dim(&grid, y, at, right - 1, "the read result");
    }
    // The tool reports how many lines matched.
    let search = ledger_row(&grid, gutter, "search", "grep -n A0003 a.txt");
    assert_eq!(grid.cell(gutter + 6, search).symbol.as_str(), "✓");
    assert_eq!(grid.cell(gutter + 6, search).fg, SUCCESS);
    for x in gutter + 8..gutter + 17 {
        assert_eq!(grid.cell(x, search).fg, INFO);
    }
    let row = row_text(&grid, search).trim_end().to_owned();
    // The tool reports how many lines matched.
    let result = if row.ends_with("1 match") {
        "1 match"
    } else {
        assert!(row.ends_with("matches"), "{row:?}");
        row.rsplit(' ').next().unwrap_or_default()
    };
    let at = locate(&row, result).expect("the result");
    assert_dim(&grid, search, at, right - 1, "the search result");
    // The edit's counts are in added and removed, where every other
    // result is dim.
    let edit = check_row(&grid, "edit", "a.txt", &format!("−{removed}"), "✓");
    assert_eq!(grid.cell(gutter + 6, edit).fg, SUCCESS);
    let row = row_text(&grid, edit);
    let plus = locate(&row, &format!("+{added}")).expect("the added count");
    let minus = locate(&row, &format!("−{removed}")).expect("the removed count");
    for x in plus..plus + added.chars().count() as u16 + 1 {
        assert_eq!(
            grid.cell(x, edit).fg,
            ADDED,
            "added ({x}, {edit}) in {row:?}"
        );
    }
    for x in minus..minus + removed.chars().count() as u16 + 1 {
        assert_eq!(
            grid.cell(x, edit).fg,
            REMOVED,
            "removed ({x}, {edit}) in {row:?}"
        );
    }
    // The running shell: ○ and "running" in attention, its long command
    // cut with "…" so the result keeps its columns.
    let shell = check_row(&grid, "shell", "i=0;", "running", "○");
    assert_eq!(grid.cell(gutter + 6, shell).fg, ATTENTION);
    let row = row_text(&grid, shell);
    assert!(row.contains('…'), "the long command is cut: {row:?}");
    let at = locate(&row, "running").expect("the result");
    for x in at..at + 7 {
        assert_eq!(grid.cell(x, shell).fg, ATTENTION, "running ({x}, {shell})");
    }

    // Removing the marker lets the shell finish; the click target below
    // is located on a screen taken after the turn completes, so the row
    // cannot be stale.
    let from = run.output().len();
    fs::remove_file(&hold).unwrap();
    run.wait_screen("the done reply", |grid| grid.contents.contains("Done."));
    run.turn_finished(from);

    // A click on the a.txt read row shows its output indented eight
    // columns with the line-number gutter: 20 lines, then
    // "… 5 more lines (Enter to view)" dim. The grid is fresh
    // from after the completion, and the card edges move with it.
    let grid = run.wait_screen("the open ledger", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("+ Thought: Reading"))
    });
    let y = find_row(&grid.rows, "thought 4 times").expect("the group row");
    let (left, _right) = card_edges(&grid, y, 2);
    let gutter = left + 1;
    let read = ledger_row(&grid, gutter, "read", "a.txt");
    run.write(
        format!(
            "\x1b[<0;{};{}M\x1b[<0;{};{}m",
            gutter + 10 + 1,
            read + 1,
            gutter + 10 + 1,
            read + 1
        )
        .as_bytes(),
    );
    let grid = run.wait_screen("the opened read", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("… 5 more lines (Enter to view)"))
    });
    let y = find_row(&grid.rows, "thought 4 times").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let gutter = left + 1;
    for n in 1..=20u16 {
        let needle = format!("{:>2} A{n:04}", n);
        assert!(
            grid.rows.iter().any(|row| {
                row.chars()
                    .skip((gutter + 8) as usize)
                    .collect::<String>()
                    .starts_with(&needle)
            }),
            "{needle} after the gutter"
        );
    }
    assert!(
        !grid.rows.iter().any(|row| row.contains("A0021")),
        "only twenty lines draw"
    );
    let more = find_row(&grid.rows, "… 5 more lines (Enter to view)").expect("the more row");
    assert_dim(&grid, more, gutter + 8, right - 1, "the more row");

    // Ctrl+O closes the ledger: the finished row reads as the running
    // one did, with its duration and ▸ at the same columns, every
    // non-blank cell dim.
    run.write(b"\x0f");
    let grid = run.wait_screen("the closed ledger", |grid| {
        grid.rows
            .iter()
            .any(|row| row.contains("thought 4 times") && !row.contains("+ Thought:"))
            && !grid.rows.iter().any(|row| row.contains("+ Thought:"))
    });
    let y = find_row(&grid.rows, "thought 4 times").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let gutter = left + 1;
    let text = content(&grid, y, left, right);
    assert!(
        text.starts_with(&format!(
            "● Read 2 files, searched 1 pattern, edited 1 file +{added} −{removed}, ran 1 command, thought 4 times"
        )),
        "{text:?}"
    );
    assert!(text.ends_with('▸'), "{text:?}");
    assert_eq!(grid.cell(right - 1, y).symbol.as_str(), "▸");
    // The duration, right-aligned: seconds ("Ns"), or minutes and
    // seconds ("Nm SSs").
    let mut trimmed = text.clone();
    trimmed.pop();
    let before_toggle = trimmed.trim_end().to_owned();
    let tokens: Vec<&str> = before_toggle.split(' ').collect();
    let is_numbered = |token: &str, unit: char| {
        token.len() > 1
            && token.ends_with(unit)
            && token[..token.len() - 1]
                .chars()
                .all(|ch| ch.is_ascii_digit())
    };
    let last = tokens.last().copied().unwrap_or_default();
    if tokens.len() >= 2 && is_numbered(tokens[tokens.len() - 2], 'm') {
        assert!(is_numbered(last, 's') && last.len() == 3, "{text:?}");
    } else {
        assert!(is_numbered(last, 's'), "{text:?}");
    }
    for x in gutter..right - 1 {
        if !is_blank(&grid, x, y) {
            assert!(grid.cell(x, y).dim, "finished ({x}, {y})");
        }
    }
    let dot = grid.cell(gutter, y);
    assert_eq!(dot.symbol.as_str(), "●");
    assert!(dot.dim);
    let row = row_text(&grid, y);
    let plus = locate(&row, &format!("+{added}")).expect("added");
    let minus = locate(&row, &format!("−{removed}")).expect("removed");
    for x in plus..plus + added.chars().count() as u16 + 1 {
        assert_eq!(grid.cell(x, y).fg, ADDED);
    }
    for x in minus..minus + removed.chars().count() as u16 + 1 {
        assert_eq!(grid.cell(x, y).fg, REMOVED);
    }

    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
