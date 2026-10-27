//! Binary-level test of opened tool calls (`docs/tui.md`, "Tool groups
//! and the ledger"): the real binary on a 160x48 pty in truecolour, with
//! a scripted provider. Each scenario opens the ledger, clicks a call's
//! row, and asserts the text and style of the opened rows.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use std::fs;

use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// The dark theme's added, removed and match colours in truecolour
/// (`crates/tui/src/theme.rs`).
const ADDED: Colour = Colour::Rgb(115, 201, 145);
const REMOVED: Colour = Colour::Rgb(255, 93, 115);
const MATCH: Colour = Colour::Rgb(90, 74, 26);
/// A turn card's background in truecolour.
const TURN: Colour = Colour::Rgb(16, 16, 23);

/// The scripted turn: `steps`, then done.
fn script(setup: &Setup, steps: serde_json::Value) {
    write_json(&setup.workspace().join("s.json"), &json!({"steps": steps}));
}

/// The scripted provider as the model, the rail hidden.
fn config(setup: &Setup) {
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"rail": {"shown": false}}}),
    );
}

/// A standing rule allowing every shell command, so no approval panel
/// stops the turn. An approval for any other tool is answered on screen.
fn allow_shell(setup: &Setup) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": ""})
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

/// The first row below `after` containing `needle`.
fn find_row_below(rows: &[String], after: u16, needle: &str) -> Option<u16> {
    rows.iter()
        .enumerate()
        .skip(after as usize + 1)
        .find(|(_, row)| row.contains(needle))
        .map(|(y, _)| y as u16)
}

/// The first row below `after` that draws exactly the indented `path`
/// with every path cell bold: a grouped path row. A substring search
/// would also match the command line naming both files, so the
/// grouping assertion reads only these rows.
fn find_path_row_below(grid: &Grid, left: u16, right: u16, after: u16, path: &str) -> Option<u16> {
    let want = format!("        {path}");
    (after + 1..48).find(|y| {
        let row = content(grid, *y, left, right);
        if row != want {
            return false;
        }
        let Some(at) = row.find(path) else {
            return false;
        };
        path.chars().enumerate().all(|(i, _)| {
            let x = left + 1 + at as u16 + i as u16;
            grid.cell(x, *y).bold
        })
    })
}

#[test]
fn an_opened_edit_shows_a_numbered_diff_with_paired_words() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("a.txt"), "call foo(a);\n").unwrap();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "edit", "arguments": {"path": "a.txt", "edits": [
                    {"old_text": "call foo(a);\n", "new_text": "call bar(a);\n"},
                ]}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    open_call(&mut run, gutter, "edit", "a.txt", "@@ -1 +1 @@");
    // The opened diff under the edit's row.
    let grid = run.wait_screen("the opened diff", |grid| {
        grid.rows.iter().any(|row| row.contains("+call bar(a);"))
    });
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let gutter = left + 1;
    let at = |row: &str| -> u16 { find_row(&grid.rows, row).unwrap_or_else(|| panic!("{row}")) };
    // The hunk row is dim.
    let hunk = at("@@ -1 +1 @@");
    assert_dim(&grid, hunk, gutter + 8, right - 1, "the hunk row");
    // The removed line numbers the old file in removed, the added line
    // the new file in added, both gutters dim.
    for (needle, colour, word) in [
        ("-call foo(a);", REMOVED, "foo(a);"),
        ("+call bar(a);", ADDED, "bar(a);"),
    ] {
        let y = at(needle);
        let row = content(&grid, y, left, right);
        assert!(row.starts_with("        1 "), "{row:?} gutter and indent");
        assert_dim(&grid, y, gutter + 8, gutter + 10, "the gutter");
        // Exactly the differing word draws reversed.
        let inverse: String = (gutter + 8..right - 1)
            .filter(|x| grid.cell(*x, y).inverse)
            .map(|x| grid.cell(x, y).symbol.clone())
            .collect();
        assert_eq!(inverse, word, "{needle}");
        for x in gutter + 10..right - 1 {
            let cell = grid.cell(x, y);
            if is_blank(&grid, x, y) {
                continue;
            }
            assert_eq!(cell.fg, colour, "body ({x}, {y})");
        }
    }
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn an_opened_read_at_an_offset_numbers_from_the_offset() {
    let setup = Setup::new();
    let lines = (1..=10).map(|n| format!("N{n:04}\n")).collect::<String>();
    fs::write(setup.workspace().join("notes.txt"), &lines).unwrap();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "read", "arguments": {"path": "notes.txt", "offset": 4}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    let grid = open_call(&mut run, gutter, "read", "notes.txt", "N0004");
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let gutter = left + 1;
    let row = find_row(&grid.rows, "N0004").expect("the first line read");
    let text = content(&grid, row, left, right);
    assert_eq!(text, "         4 N0004", "{text:?}");
    assert_dim(&grid, row, gutter + 8, gutter + 10, "the gutter");
    let row = find_row(&grid.rows, "N0007").expect("a later line");
    assert_eq!(content(&grid, row, left, right), "         7 N0007");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn an_opened_write_shows_its_content_numbered_from_one() {
    let setup = Setup::new();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "write", "arguments": {"path": "out.txt", "content": "w1\nw2\n"}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    let grid = open_call(&mut run, gutter, "write", "out.txt", "w1");
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let row = find_row(&grid.rows, "w1").expect("the written text");
    assert_eq!(content(&grid, row, left, right), "        1 w1");
    let row = find_row(&grid.rows, "w2").expect("the written text");
    assert_eq!(content(&grid, row, left, right), "        2 w2");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn an_opened_shell_grep_shows_matches_grouped_by_path() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("m1.txt"), "a\nneedle one\nb\n").unwrap();
    fs::write(setup.workspace().join("m2.txt"), "c\nneedle two\nd\n").unwrap();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "shell", "arguments": {"command": "grep -n -C 1 needle m1.txt m2.txt"}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    allow_shell(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    let grid = open_call(&mut run, gutter, "search", "grep", "needle one");
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    // The grouped path rows draw the path bold: holding the frame with
    // both bold rows never reads the command line naming both files.
    let grid = run.wait_screen("the grouped paths", |grid| {
        ["m1.txt", "m2.txt"]
            .iter()
            .all(|path| find_path_row_below(grid, left, right, 0, path).is_some())
    });
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    // One bold path row per file, in printed order.
    let call = ledger_row(&grid, left + 1, "search", "grep");
    let below: Vec<String> = grid.rows.iter().skip(call as usize + 1).cloned().collect();
    let first = find_path_row_below(&grid, left, right, call, "m1.txt")
        .unwrap_or_else(|| panic!("the first path in {below:?}"));
    let second = find_path_row_below(&grid, left, right, call, "m2.txt")
        .unwrap_or_else(|| panic!("the second path in {below:?}"));
    assert!(
        first < second,
        "printed order: {first} vs {second} in {below:?}"
    );
    for (path, y) in [("m1.txt", first), ("m2.txt", second)] {
        let row = content(&grid, y, left, right);
        assert_eq!(row, format!("        {path}"), "{row:?}");
        let at = row.find(path).expect("the path");
        for (i, _) in path.chars().enumerate() {
            let x = left + 1 + at as u16 + i as u16;
            assert!(grid.cell(x, y).bold, "path ({x}, {y})");
        }
    }
    // The matched text draws on the match tint; the context line does not.
    let hit = find_row(&grid.rows, "needle one").expect("the match");
    let row = row_text(&grid, hit);
    let at = row.find("needle").expect("the match");
    for (i, _) in "needle".chars().enumerate() {
        assert_eq!(grid.cell(at as u16 + i as u16, hit).bg, MATCH, "match cell");
    }
    let before: String = row.chars().take(at).collect();
    assert!(before.contains("2"), "{row:?} gutter");
    // The context lines around the match carry their line numbers.
    assert_eq!(
        content(&grid, hit.saturating_sub(1), left, right),
        "        1 a"
    );
    assert_eq!(content(&grid, hit + 1, left, right), "        3 b");
    // The context lines around the match carry no match tint.
    for y in [hit.saturating_sub(1), hit + 1] {
        for x in 0..160 {
            assert_ne!(grid.cell(x, y).bg, MATCH, "context cell ({x}, {y})");
        }
    }
    // One closing row.
    let exits: Vec<u16> = (0..48)
        .filter(|y| row_text(&grid, *y).contains("exit 0"))
        .collect();
    assert_eq!(exits.len(), 1, "one closing row");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn a_shell_rg_opens_as_shell() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("m1.txt"), "a\nneedle one\nb\n").unwrap();
    let command = if std::process::Command::new("rg")
        .arg("--version")
        .output()
        .is_ok_and(|out| out.status.success())
    {
        "rg -n needle m1.txt"
    } else {
        "command grep -n needle m1.txt"
    };
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "shell", "arguments": {"command": command}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    allow_shell(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    let kind = if command.starts_with("rg") {
        "search"
    } else {
        "shell"
    };
    let grid = open_call(&mut run, gutter, kind, command, "$ ");
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let row = find_row(&grid.rows, "$ ").expect("the command row");
    let text = content(&grid, row, left, right);
    assert!(text.starts_with("        $ "), "{text:?}");
    assert!(text.contains("needle"), "{text:?}");
    // Output, then one closing row: no grouped paths. The ledger
    // row above names the shell's own result, which reads exit 0 too.
    let call = ledger_row(&grid, left + 1, kind, command);
    let exits: Vec<u16> = (call + 1..48)
        .filter(|y| row_text(&grid, *y).contains("exit 0"))
        .collect();
    assert_eq!(exits.len(), 1, "one closing row");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn a_failing_shell_shows_its_headline_then_exit_one_in_removed() {
    let setup = Setup::new();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "shell", "arguments": {"command": "echo out; exit 1"}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    allow_shell(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    let grid = open_call(&mut run, gutter, "shell", "echo out", "$ echo out; exit 1");
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let call = ledger_row(&grid, left + 1, "shell", "echo out");
    let head =
        find_row_below(&grid.rows, call, "nonzero_exit: Exit code 1.").expect("the headline");
    // The headline is the first opened row.
    assert_eq!(head, call + 1, "headline first");
    // The command, its output, then exactly one closing row in removed.
    let rows: Vec<String> = grid.rows.clone();
    assert!(rows.iter().any(|row| row.contains("$ echo out; exit 1")));
    assert!(rows.iter().any(|row| row.contains("        out")));
    // The tool's closing line draws once, as the headline's message:
    // no output row repeats it.
    let below: Vec<String> = grid.rows.iter().skip(call as usize + 1).cloned().collect();
    let closings: Vec<&String> = below
        .iter()
        .filter(|row| row.contains("Exit code 1."))
        .collect();
    assert_eq!(closings.len(), 1, "no repeated tool line");
    let exits: Vec<u16> = (call + 1..48)
        .filter(|y| {
            let row = row_text(&grid, *y);
            row.contains("exit 1") && !row.contains("$ ")
        })
        .collect();
    assert_eq!(exits.len(), 1, "one closing row");
    let cell = grid.cell(left + 1 + 8, exits[0]);
    assert_eq!(cell.fg, REMOVED, "exit 1 in removed");
    let _ = right;
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn a_killed_shell_shows_killed_by_a_signal() {
    let setup = Setup::new();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "shell", "arguments": {"command": "kill -KILL $$"}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    allow_shell(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    let grid = open_call(&mut run, gutter, "shell", "kill -KILL", "killed by");
    let rows: Vec<String> = grid.rows.clone();
    assert!(rows.iter().any(|row| row.contains("$ kill -KILL $$")));
    let killed: Vec<u16> = (0..48)
        .filter(|y| row_text(&grid, *y).contains("killed by SIGKILL"))
        .collect();
    assert_eq!(killed.len(), 1, "one closing row");
    let y = killed[0];
    let row = row_text(&grid, y);
    let at = row.find("killed by SIGKILL").expect("the closing line");
    for (i, _) in "killed by SIGKILL".chars().enumerate() {
        assert_eq!(
            grid.cell(at as u16 + i as u16, y).fg,
            REMOVED,
            "closing cell"
        );
    }
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn an_extension_tool_with_json_content_shows_it_indented() {
    let setup = Setup::new();
    setup.lua(
        "json",
        "fiber.tool(\"json_data\", { description = \"The json_data tool.\", input_schema = { type = \"object\" }, timeout = 2000, effects = { effects = { \"reads\" }, reversible = true }, run = function() return '{\"b\":[1,2],\"a\":{\"c\":true}}' end })\n",
    );
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "json_data", "arguments": {}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    // The call's empty arguments name no target: its row is the
    // turn's only "other" one.
    let grid = open_call(&mut run, gutter, "other", "", "\"c\": true");
    let expected = [
        "{",
        "  \"a\": {",
        "    \"c\": true",
        "  },",
        "  \"b\": [",
        "    1,",
        "    2",
        "  ]",
        "}",
    ];
    for line in expected {
        assert!(
            grid.rows
                .iter()
                .any(|row| row.contains(&format!("        {line}"))),
            "{line} indented"
        );
    }
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// Runs `git` in `dir` directly, with no deadline and no process group,
/// panicking on failure.
fn git_without_deadline(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[test]
fn the_changed_files_view_draws_a_gutter_and_reverse_video_words() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    git_without_deadline(&workspace, &["init"]);
    git_without_deadline(&workspace, &["config", "user.email", "test@fiber.dev"]);
    git_without_deadline(&workspace, &["config", "user.name", "fiber test"]);
    fs::write(workspace.join("f.txt"), "call foo(a);\n").unwrap();
    git_without_deadline(&workspace, &["add", "f.txt"]);
    git_without_deadline(&workspace, &["commit", "-m", "add f"]);
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "edit", "arguments": {"path": "f.txt", "edits": [
                    {"old_text": "call foo(a);\n", "new_text": "call bar(a);\n"},
                ]}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    // The panel's Changed files card names the edited file; its row
    // opens the diff straight away.
    let grid = run.wait_screen("the changed files card", |grid| {
        grid.rows.iter().any(|row| row.contains("Changed files"))
    });
    let y = find_row(&grid.rows, "f.txt +1").expect("the file row");
    let row = row_text(&grid, y);
    let at = row.find("f.txt").expect("the file row");
    click(&mut run, at as u16 + 2, y);
    let grid = run.wait_screen("the file diff", |grid| {
        grid.rows.iter().any(|row| row.contains("+call bar(a);"))
    });
    // No diff headers draw.
    assert!(
        !grid
            .rows
            .iter()
            .any(|row| row.contains("diff --git") || row.contains("+++ ")),
        "headers dropped"
    );
    // A dim gutter numbers the changed lines; the differing words draw
    // reversed.
    let removed = find_row(&grid.rows, "-call foo(a);").expect("the removed line");
    let added = find_row(&grid.rows, "+call bar(a);").expect("the added line");
    for (y, word, colour) in [(removed, "foo(a);", REMOVED), (added, "bar(a);", ADDED)] {
        let row = row_text(&grid, y);
        let at = row.find(word).expect("the marked word");
        let mut marked = String::new();
        for (i, _) in word.chars().enumerate() {
            let cell = grid.cell(at as u16 + i as u16, y);
            assert!(cell.inverse, "marked ({at}, {y})");
            assert_eq!(cell.fg, colour);
            marked.push_str(&cell.symbol);
        }
        assert_eq!(marked, word);
    }
    // The gutter before the removed line is dim.
    let row = row_text(&grid, removed);
    let at = row.find("-call foo(a);").expect("the removed line");
    let gutter: String = row.chars().take(at).collect();
    assert!(gutter.ends_with("1 "), "{gutter:?}");
    let start = u16::try_from(at).unwrap() - 2;
    assert_dim(&grid, removed, start, start + 2, "the gutter");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn the_more_row_is_dim_and_clicking_it_opens_the_whole_call() {
    let setup = Setup::new();
    let lines = (1..=25).map(|n| format!("A{n:04}\n")).collect::<String>();
    fs::write(setup.workspace().join("a.txt"), &lines).unwrap();
    script(
        &setup,
        json!([
            {"reasoning": REASONING, "tool_calls": [
                {"name": "read", "arguments": {"path": "a.txt"}},
            ]},
            {"text": ["Done."]},
        ]),
    );
    config(&setup);
    let (mut run, from) = started(&setup);
    finish(&mut run, "Done.");
    run.turn_finished(from);
    let (_grid, gutter) = open_ledger(&mut run);
    let grid = open_call(&mut run, gutter, "read", "a.txt", "… 5 more lines");
    let y = find_row(&grid.rows, "thought ").expect("the group row");
    let (left, right) = card_edges(&grid, y, 2);
    let gutter = left + 1;
    let more = find_row(&grid.rows, "… 5 more lines (Enter to view)").expect("the more row");
    assert_dim(&grid, more, gutter + 8, right - 1, "the more row");
    // The more row opens the whole call, without the inline line cap.
    click(&mut run, gutter + 10, more);
    let grid = run.wait_screen("the call view opens", |grid| {
        grid.rows.iter().any(|row| row.contains("A0025"))
    });
    assert!(grid.rows.iter().any(|row| row.contains("Read  a.txt")));
    assert!(!grid.rows.iter().any(|row| row.contains("… 5 more lines")));
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
