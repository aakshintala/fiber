//! Binary-level tests of the `/tools`, `/rules` and `/settings` views
//! (`docs/tui.md`, "Swapped views"): the real binary on a 160x48 pty in
//! truecolour, with the scripted provider answering, draws the doc's
//! rows. Source headings are bold, a switch reads `[on]` bold or `[off]`
//! dim, a rule's ✕ is in `error`, the settings table has a dim header and
//! aligned columns, and each view's last rows are dim. The focused row
//! carries a "› " gutter and no reverse video. The changed-files diff's
//! `@@` row is dim.

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

/// `error` in the dark theme.
const ERROR: Colour = Colour::Rgb(255, 93, 115);

/// The prompt tokens the scripted turns report: what a reload rebuilds.
const PROMPT_TOKENS: u64 = 84_000;

/// The scripted model, the rail hidden, a rebuild cost to report.
fn config(setup: &Setup, extra: serde_json::Value) {
    let mut config = json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
        "tui": {"rail": {"shown": false}}});
    for (key, value) in extra.as_object().unwrap() {
        config[key] = value.clone();
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// A setup whose first prompt calls the extension tool `notes_save`, then
/// answers "Done.", both steps reporting `PROMPT_TOKENS` of input so the
/// views have a cache rebuild to name.
fn tools_setup(extra: serde_json::Value) -> Setup {
    let setup = Setup::new();
    setup.lua(
        "notes",
        "fiber.tool(\"notes_save\", { description = \"The notes_save tool.\", input_schema = { type = \"object\" }, timeout = 2000, effects = { effects = { \"reads\" }, reversible = true }, run = function() return 'saved' end })\n",
    );
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "notes_save", "arguments": {}}],
             "usage": {"input": PROMPT_TOKENS}},
            {"text": ["Done."], "usage": {"input": PROMPT_TOKENS}},
        ]}),
    );
    config(&setup, extra);
    setup
}

/// The binary started, prompted and its turn finished.
fn turned(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let from = run.output().len();
    run.write(b"go\r");
    run.turn_finished(from);
    run
}

/// The first row containing `needle`.
fn find_row(grid: &Grid, needle: &str) -> Option<u16> {
    grid.rows
        .iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
}

/// The first row containing `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    find_row(grid, needle).unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents))
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

/// Whether every cell of `needle`, found on `y`, is bold.
fn is_bold(grid: &Grid, y: u16, needle: &str) -> bool {
    col_at_or(grid, y, needle)
        .is_some_and(|x| (0..needle.chars().count() as u16).all(|dx| grid.cell(x + dx, y).bold))
}

/// Whether every non-blank cell of `needle`, found on `y`, is dim.
fn is_dim(grid: &Grid, y: u16, needle: &str) -> bool {
    col_at_or(grid, y, needle)
        .is_some_and(|x| (0..needle.chars().count() as u16).all(|dx| grid.cell(x + dx, y).dim))
}

/// Whether no cell of `needle`, found on `y`, is reversed.
fn is_plain_of_reverse(grid: &Grid, y: u16, needle: &str) -> bool {
    col_at_or(grid, y, needle)
        .is_some_and(|x| (0..needle.chars().count() as u16).all(|dx| !grid.cell(x + dx, y).inverse))
}

/// A click at cell (`x`, `y`).
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// The `/tools` row for `notes_save`: its text from the name on.
fn tool_row(grid: &Grid) -> Option<(u16, String)> {
    let y = find_row(grid, "notes_save · full · ")?;
    let x = col_at_or(grid, y, "notes_save · full · ")?;
    let text: String = (x..160).map(|x| grid.cell(x, y).symbol.clone()).collect();
    Some((y, text.trim_end().to_owned()))
}

/// Whether `text` reads `notes_save · full · <digits,> bytes · this project
/// <project> · everywhere <everywhere>`.
fn reads_as_tool_row(text: &str, project: &str, everywhere: &str) -> bool {
    let Some(rest) = text.strip_prefix("notes_save · full · ") else {
        return false;
    };
    let Some((size, rest)) = rest.split_once(" bytes · this project ") else {
        return false;
    };
    size.chars().all(|ch| ch.is_ascii_digit() || ch == ',')
        && !size.is_empty()
        && rest == format!("{project} · everywhere {everywhere}")
}

/// Whether the cells from `x` on row `y` spell the switch `word`, `[on]`
/// bold and not dim or `[off]` dim and not bold.
fn switch_weighs(grid: &Grid, y: u16, x: u16, word: &str) -> bool {
    word.chars().enumerate().all(|(dx, ch)| {
        let cell = grid.cell(x + dx as u16, y);
        cell.symbol == ch.to_string()
            && if word == "[on]" {
                cell.bold && !cell.dim
            } else {
                cell.dim && !cell.bold
            }
    })
}

/// The `/tools` screen once every property the test reads has drawn: the
/// bold headings, the row with its two switches in `project` and
/// `everywhere` form with those switches' weight, the "› " gutter on it
/// and the dim last row.
fn tools_ready(grid: &Grid, project: &str, everywhere: &str) -> bool {
    let Some((y, text)) = tool_row(grid) else {
        return false;
    };
    if !reads_as_tool_row(&text, project, everywhere) {
        return false;
    }
    let Some(x) = col_at_or(grid, y, "notes_save") else {
        return false;
    };
    if x < 2 || grid.cell(x - 2, y).symbol != "›" || grid.cell(x - 1, y).symbol != " " {
        return false;
    }
    let switch_after =
        |label: &str| col_at_or(grid, y, label).map(|x| x + label.chars().count() as u16);
    let (Some(at_project), Some(at_everywhere)) =
        (switch_after("this project "), switch_after("everywhere "))
    else {
        return false;
    };
    if !(switch_weighs(grid, y, at_project, project)
        && switch_weighs(grid, y, at_everywhere, everywhere))
    {
        return false;
    }
    let Some(footer) = find_row(grid, "a change rebuilds the cache") else {
        return false;
    };
    is_bold(grid, row_with(grid, "Built-in"), "Built-in")
        && find_row(grid, "Extension · ")
            .is_some_and(|heading| is_bold(grid, heading, "Extension · "))
        && is_dim(
            grid,
            footer,
            "a change rebuilds the cache (~84k tokens) on reload",
        )
}

#[test]
fn the_tools_view_matches_the_doc() {
    let setup = tools_setup(json!({}));
    let mut run = turned(&setup);
    run.write(b"/tools\r");
    let grid = run.wait_screen("the tools rows", |grid| tools_ready(grid, "[on]", "[on]"));
    // The headings: a built-in source and the extension's, which names
    // its own installed name.
    let heading = row_with(&grid, "Extension · ");
    let text: String = (0..160)
        .map(|x| grid.cell(x, heading).symbol.clone())
        .collect();
    assert!(text.contains("notes"), "{text}");
    assert!(is_bold(&grid, row_with(&grid, "Built-in"), "Built-in"));
    // A built-in tool has no switch.
    let read = row_with(&grid, "read · full · ");
    let read_text: String = (0..160)
        .map(|x| grid.cell(x, read).symbol.clone())
        .collect::<String>()
        .trim_end()
        .to_owned();
    assert!(!read_text.contains('['), "{read_text}");
    // The last row is the cost line; the old keys line is gone.
    assert!(!grid.contents.contains("↑↓ move"), "{}", grid.contents);
    assert!(!grid.contents.contains("choose a switch"));
    // The focused row has the gutter and no reverse video: the row's
    // switches draw as their own cells do.
    let (y, text) = tool_row(&grid).expect("the tool row");
    assert!(reads_as_tool_row(&text, "[on]", "[on]"), "{text}");
    let x = col_at(&grid, y, "notes_save");
    assert_eq!(grid.cell(x - 2, y).symbol, "›");
    assert!(!grid.cell(x - 2, y).inverse);
    assert!(is_plain_of_reverse(&grid, y, "notes_save · full · "));
    assert!(is_plain_of_reverse(&grid, y, "[on] · everywhere [on]"));
    assert_eq!(grid.cell(x - 2, read).symbol, " ", "no gutter on read");
    // Space flips the focused switch, this project's, and the gutter stays.
    run.write(b" ");
    let grid = run.wait_screen("the project switch off", |grid| {
        tools_ready(grid, "[off]", "[on]")
    });
    let (y, text) = tool_row(&grid).expect("the tool row");
    assert!(reads_as_tool_row(&text, "[off]", "[on]"), "{text}");
    assert_eq!(grid.cell(col_at(&grid, y, "notes_save") - 2, y).symbol, "›");
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

#[test]
fn the_rules_view_matches_the_doc() {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": ["Done."]}]}),
    );
    config(&setup, json!({}));
    let rules = setup.home().join("rules");
    fs::write(
        &rules,
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": "git status",
                "added": 1000, "session_id": "s_aaa"})
        ),
    )
    .unwrap();
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"/rules\r");
    let grid = run.wait_screen("the rules rows", |grid| {
        let Some(y) = find_row(grid, "allow shell:git status · added ") else {
            return false;
        };
        let Some(x) = col_at_or(grid, y, "allow shell:git status") else {
            return false;
        };
        let text: String = (x..160).map(|x| grid.cell(x, y).symbol.clone()).collect();
        let Some(cross) = col_at_or(grid, y, "✕") else {
            return false;
        };
        rules_row(text.trim_end())
            && grid.cell(cross, y).fg == ERROR
            && find_row(grid, "Global").is_some_and(|h| is_bold(grid, h, "Global"))
            && find_row(grid, "This project").is_some_and(|h| is_bold(grid, h, "This project"))
            && find_row(grid, "Ctrl+G opens the rules file")
                .is_some_and(|f| is_dim(grid, f, "Ctrl+G opens the rules file"))
            && grid.cell(x - 2, y).symbol == "›"
    });
    let y = row_with(&grid, "allow shell:git status");
    let x = col_at(&grid, y, "allow shell:git status");
    // The focus is the gutter alone: no cell of the row is reversed.
    assert!(is_plain_of_reverse(
        &grid,
        y,
        "allow shell:git status · added "
    ));
    assert!(!grid.cell(col_at(&grid, y, "✕"), y).inverse);
    assert!(x >= 2);
    assert!(!grid.contents.contains("Global rules"), "{}", grid.contents);
    // Delete revokes the selected rule: the row goes and the line leaves
    // the file. (The ✕ is a pointer target; a test clicks only a row
    // whose position a signal has fixed, and this view has no such
    // signal, so the keyboard path revokes.)
    run.write(b"\x1b[3~");
    let grid = run.wait_screen("the rule revoked", |grid| {
        find_row(grid, "allow shell:git status").is_none()
            && grid
                .contents
                .contains("Revoked; applies to the next call judged.")
    });
    assert!(find_row(&grid, "allow shell:git status").is_none());
    assert!(
        !fs::read_to_string(&rules).unwrap().contains("git status"),
        "the line is still in the file"
    );
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

/// Whether `text` reads `allow shell:git status · added <digits>d ago in
/// s_aaa · ✕`.
fn rules_row(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("allow shell:git status · added ") else {
        return false;
    };
    let Some((age, rest)) = rest.split_once("d ago in ") else {
        return false;
    };
    !age.is_empty() && age.chars().all(|ch| ch.is_ascii_digit()) && rest.starts_with("s_aaa · ✕")
}

/// The key the "› " gutter marks in the settings table, if one does: the
/// first word after the gutter on the row whose gutter column holds it.
fn selected_key(grid: &Grid, x0: u16, header: u16) -> Option<String> {
    (header + 1..47).find_map(|y| {
        if grid.cell(x0, y).symbol != "›" {
            return None;
        }
        let text: String = (x0 + 2..160)
            .map(|x| grid.cell(x, y).symbol.clone())
            .collect();
        text.split_whitespace().next().map(str::to_owned)
    })
}

#[test]
fn the_settings_view_matches_the_doc() {
    let setup = tools_setup(json!({}));
    let mut run = turned(&setup);
    run.write(b"/settings\r");
    let mut grid = run.wait_screen("the settings table", |grid| {
        find_row(grid, "key").is_some()
            && find_row(grid, "Ctrl+G opens the file").is_some()
            && find_row(grid, "layer").is_some()
    });
    let header = row_with(&grid, "layer");
    let key_x = col_at(&grid, header, "key");
    let x0 = key_x - 2;
    // Move the selection to `handoff.tokens`, a key that applies on
    // reload; each press waits for the gutter to move.
    for _ in 0..200 {
        let on = selected_key(&grid, x0, header);
        if on.as_deref() == Some("handoff.tokens") {
            break;
        }
        run.write(b"\x1b[B");
        grid = run.wait_screen("the gutter moved", |next| {
            selected_key(next, x0, header) != on
        });
    }
    assert_eq!(
        selected_key(&grid, x0, header).as_deref(),
        Some("handoff.tokens")
    );
    // The header is dim and names the three columns.
    for word in ["key", "value", "layer"] {
        assert!(is_dim(&grid, header, word), "{word} is dim");
    }
    let layer_x = col_at(&grid, header, "layer");
    let value_x = col_at(&grid, header, "value");
    // The model's value draws unquoted, in the value column, with its
    // layer in the layer column.
    let model = (header + 1..47)
        .find(|y| {
            grid.rows[usize::from(*y)]
                .trim_start_matches(['›', ' '])
                .starts_with("model ")
        })
        .unwrap_or_else(|| panic!("the model row in\n{}", grid.contents));
    assert_eq!(col_at(&grid, model, "scripted/s.json"), value_x);
    assert!(
        !grid.rows[usize::from(model)].contains("\"scripted/s.json\""),
        "the model's value is unquoted"
    );
    assert_eq!(col_at(&grid, model, "global"), layer_x);
    // Every row's layer starts where the header's does.
    let handoff = row_with(&grid, "handoff.tokens");
    assert_eq!(col_at(&grid, handoff, "default"), layer_x);
    assert_eq!(col_at(&grid, handoff, "400000"), value_x);
    assert!(is_plain_of_reverse(&grid, handoff, "handoff.tokens"));
    // The reload line and the file hint are the view's last two rows, dim.
    let hint = row_with(&grid, "Ctrl+G opens the file");
    assert!(is_dim(&grid, hint, "Ctrl+G opens the file"));
    let reload = "changing handoff.tokens needs a reload (~84k tokens)";
    assert_eq!(find_row(&grid, reload), Some(hint - 1), "{}", grid.contents);
    assert!(is_dim(&grid, hint - 1, reload));
    assert!(!grid.contents.contains("↑↓ move"), "{}", grid.contents);
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

/// Runs `git` in `dir`, panicking on failure.
fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[test]
fn the_changed_files_hunk_row_is_dim() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "test@fiber.dev"]);
    git(&workspace, &["config", "user.name", "fiber test"]);
    fs::write(workspace.join("f.txt"), "call foo(a);\n").unwrap();
    git(&workspace, &["add", "f.txt"]);
    git(&workspace, &["commit", "-m", "add f"]);
    write_json(
        &workspace.join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "edit", "arguments": {"path": "f.txt", "edits": [
                {"old_text": "call foo(a);\n", "new_text": "call bar(a);\n"},
            ]}}]},
            {"text": ["Done."]},
        ]}),
    );
    config(&setup, json!({}));
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let from = run.output().len();
    run.write(b"go\r");
    run.turn_finished(from);
    let grid = run.wait_screen("the changed files card", |grid| {
        find_row(grid, "f.txt +1").is_some()
    });
    let y = row_with(&grid, "f.txt +1");
    click(&mut run, col_at(&grid, y, "f.txt") + 2, y);
    let grid = run.wait_screen("the file diff", |grid| {
        find_row(grid, "+call bar(a);").is_some() && find_row(grid, "@@ -1 +1 @@").is_some()
    });
    let hunk = row_with(&grid, "@@ -1 +1 @@");
    assert!(is_dim(&grid, hunk, "@@ -1 +1 @@"), "{}", grid.contents);
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}
