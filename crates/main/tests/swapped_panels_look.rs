//! Binary-level tests of the `/tools`, `/rules`, `/settings` and `/skills`
//! views (`docs/tui.md`, "Swapped views"): the real binary on a 160x48 pty
//! in truecolour, with the scripted provider answering, draws the doc's
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
use std::path::PathBuf;

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

/// The two columns before the switches of a row whose focus is on
/// everywhere or on this project: the focused one holds the mark "›".
fn marks(on_everywhere: bool) -> (&'static str, &'static str) {
    if on_everywhere {
        (" ", "›")
    } else {
        ("›", " ")
    }
}

/// Whether `text` reads `notes_save · full · <digits,> bytes · this
/// project›<project> · everywhere <everywhere>`, the mark "›" on the
/// focused switch's side and a space on the other.
fn reads_as_tool_row(text: &str, project: &str, everywhere: &str, on_everywhere: bool) -> bool {
    let Some(rest) = text.strip_prefix("notes_save · full · ") else {
        return false;
    };
    let (project_mark, everywhere_mark) = marks(on_everywhere);
    let Some((size, rest)) = rest.split_once(" bytes · this project") else {
        return false;
    };
    size.chars().all(|ch| ch.is_ascii_digit() || ch == ',')
        && !size.is_empty()
        && rest == format!("{project_mark}{project} · everywhere{everywhere_mark}{everywhere}")
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
fn tools_ready(grid: &Grid, project: &str, everywhere: &str, on_everywhere: bool) -> bool {
    let Some((y, text)) = tool_row(grid) else {
        return false;
    };
    if !reads_as_tool_row(&text, project, everywhere, on_everywhere) {
        return false;
    }
    let Some(x) = col_at_or(grid, y, "notes_save") else {
        return false;
    };
    if x < 2 || grid.cell(x - 2, y).symbol != "›" || grid.cell(x - 1, y).symbol != " " {
        return false;
    }
    let after = |label: &str| col_at_or(grid, y, label).map(|x| x + label.chars().count() as u16);
    let (Some(at_project), Some(at_everywhere)) = (after("this project"), after("everywhere"))
    else {
        return false;
    };
    // The mark is bold on the focused side and a plain space on the other;
    // the switch follows it.
    let (project_mark, everywhere_mark) = marks(on_everywhere);
    for (at, mark) in [(at_project, project_mark), (at_everywhere, everywhere_mark)] {
        let cell = grid.cell(at, y);
        if cell.symbol != mark || cell.bold != (mark == "›") {
            return false;
        }
    }
    if !(switch_weighs(grid, y, at_project + 1, project)
        && switch_weighs(grid, y, at_everywhere + 1, everywhere))
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
    let grid = run.wait_screen("the tools rows", |grid| {
        tools_ready(grid, "[on]", "[on]", false)
    });
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
    assert!(reads_as_tool_row(&text, "[on]", "[on]", false), "{text}");
    let x = col_at(&grid, y, "notes_save");
    assert_eq!(grid.cell(x - 2, y).symbol, "›");
    assert!(!grid.cell(x - 2, y).inverse);
    assert!(is_plain_of_reverse(&grid, y, "notes_save · full · "));
    assert!(is_plain_of_reverse(&grid, y, "›[on] · everywhere [on]"));
    assert_eq!(grid.cell(x - 2, read).symbol, " ", "no gutter on read");
    // Space flips the focused switch, this project's, and the gutter stays.
    run.write(b" ");
    let grid = run.wait_screen("the project switch off", |grid| {
        tools_ready(grid, "[off]", "[on]", false)
    });
    let (y, text) = tool_row(&grid).expect("the tool row");
    assert!(reads_as_tool_row(&text, "[off]", "[on]", false), "{text}");
    assert_eq!(grid.cell(col_at(&grid, y, "notes_save") - 2, y).symbol, "›");
    // → moves the bold mark to the everywhere switch; the gutter stays
    // and no column moves.
    let project_x = col_at(&grid, y, "[off]");
    run.write(b"\x1b[C");
    let grid = run.wait_screen("the mark on everywhere", |grid| {
        tools_ready(grid, "[off]", "[on]", true)
    });
    let (y, text) = tool_row(&grid).expect("the tool row");
    assert!(reads_as_tool_row(&text, "[off]", "[on]", true), "{text}");
    assert_eq!(col_at(&grid, y, "[off]"), project_x);
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

/// A setup with same-name repo and personal `review` skills: the repo
/// copy wins and shadows the personal one. The scripted provider only
/// needs to keep the session alive, as for `/rules`.
fn skills_setup() -> Setup {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": ["Done."]}]}),
    );
    config(&setup, json!({}));
    for (dir, description) in [
        (
            setup.workspace().join(".agents/skills/review"),
            "Checks a diff.",
        ),
        (setup.home().join("skills/review"), "Personal notes."),
    ] {
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: review\ndescription: {description}\n---\nBody.\n"),
        )
        .unwrap();
    }
    setup
}

/// The `/skills` row for the winning `review` skill: its text from the name on.
/// Only the row's own cells are read: the view does not span the full
/// grid, so a side panel may draw to its right.
fn skill_row(grid: &Grid) -> Option<(u16, String)> {
    let want = "review · Checks a diff. · repo · seen · this project›[on] · everywhere [on] · shadows personal";
    let y = find_row(grid, "shadows personal")?;
    let x = col_at_or(grid, y, "review · Checks a diff.")?;
    let end = x.checked_add(want.chars().count() as u16)?;
    if end > 160 {
        return None;
    }
    let text: String = (x..end).map(|x| grid.cell(x, y).symbol.clone()).collect();
    Some((y, text))
}

/// Whether `text` reads the doc's row: `review · Checks a diff. · repo ·
/// seen · this project›[on] · everywhere [on] · shadows personal`, the
/// mark "›" on the focused this-project switch's side and a space on the
/// other, the shadows clause past both switches.
fn reads_as_skill_row(text: &str) -> bool {
    text == "review · Checks a diff. · repo · seen · this project›[on] · everywhere [on] · shadows personal"
}

/// The `/skills` screen once every property the test reads has drawn:
/// the winner's row with its two `[on]` switches, the "› " gutter on it
/// and the dim last row.
fn skills_ready(grid: &Grid) -> bool {
    let Some((y, text)) = skill_row(grid) else {
        return false;
    };
    if !reads_as_skill_row(&text) {
        return false;
    }
    let Some(x) = col_at_or(grid, y, "review · Checks a diff.") else {
        return false;
    };
    if x < 2 || grid.cell(x - 2, y).symbol != "›" || grid.cell(x - 1, y).symbol != " " {
        return false;
    }
    let after = |label: &str| col_at_or(grid, y, label).map(|x| x + label.chars().count() as u16);
    let (Some(at_project), Some(at_everywhere)) = (after("this project"), after("everywhere"))
    else {
        return false;
    };
    // The mark is bold on the focused side and a plain space on the
    // other; the switch follows it.
    let project = grid.cell(at_project, y);
    let everywhere = grid.cell(at_everywhere, y);
    if project.symbol != "›" || !project.bold || everywhere.symbol != " " || everywhere.bold {
        return false;
    }
    if !(switch_weighs(grid, y, at_project + 1, "[on]")
        && switch_weighs(grid, y, at_everywhere + 1, "[on]"))
    {
        return false;
    }
    let Some(footer) = find_row(grid, "Enter shows the text · Ctrl+G opens its file") else {
        return false;
    };
    is_dim(grid, footer, "Enter shows the text · Ctrl+G opens its file")
}

#[test]
fn the_skills_view_matches_the_doc() {
    let setup = skills_setup();
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    // `/skills` reads the session on screen, so start one first.
    let from = run.output().len();
    run.write(b"go\r");
    run.turn_finished(from);
    run.write(b"/skills\r");
    let grid = run.wait_screen("the skills rows", skills_ready);
    // The winner's row reads exactly as the doc says it does.
    let (y, text) = skill_row(&grid).expect("the skill row");
    assert!(reads_as_skill_row(&text), "{text}");
    let x = col_at(&grid, y, "review · Checks a diff.");
    assert_eq!(grid.cell(x - 2, y).symbol, "›");
    assert!(!grid.cell(x - 2, y).inverse);
    assert!(is_plain_of_reverse(&grid, y, "review · Checks a diff. · "));
    assert!(is_plain_of_reverse(&grid, y, "›[on] · everywhere [on]"));
    // `[on]` is bold and not dim on both switches.
    assert!(is_bold(&grid, y, "[on]"));
    let after = |label: &str| col_at(&grid, y, label) + label.chars().count() as u16;
    assert!(switch_weighs(&grid, y, after("this project") + 1, "[on]"));
    assert!(switch_weighs(&grid, y, after("everywhere") + 1, "[on]"));
    // The shadowed personal copy keeps its own row with no clause. Only
    // the row's own cells are read, as above.
    let loser = row_with(&grid, "personal · hidden");
    let loser_x = col_at(&grid, loser, "review · Personal notes.");
    let loser_text: String = (loser_x..loser_x + 100)
        .map(|x| grid.cell(x, loser).symbol.clone())
        .collect::<String>()
        .to_owned();
    assert!(
        loser_text.contains("review · Personal notes."),
        "{loser_text}"
    );
    assert!(!loser_text.contains("shadows"), "{loser_text}");
    // The last row is the doc's hint line, dim.
    assert!(!grid.contents.contains("↑↓ move"), "{}", grid.contents);
    assert!(!grid.contents.contains("choose a switch"));
    let footer = row_with(&grid, "Enter shows the text · Ctrl+G opens its file");
    assert!(is_dim(
        &grid,
        footer,
        "Enter shows the text · Ctrl+G opens its file"
    ));
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

/// The installed extension's name in the narrow `/skills` fixtures.
const LONG_EXTENSION: &str = "fiber.test/very-long-extension-name";

/// The narrow fixture's installed extension directory: the expected
/// shadow path is built from it, never from a hard-coded mapping of the
/// name.
fn installed_extension_dir(setup: &Setup) -> PathBuf {
    let root = setup.home().join("extensions");
    let installed: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("very-long-extension-name"))
        })
        .collect();
    assert_eq!(
        installed.len(),
        1,
        "the narrow fixture's extension in {root:?}"
    );
    installed.into_iter().next().unwrap()
}

/// A setup with same-name repository, personal and long-named extension
/// `review` skills: the repository copy wins and shadows the other two.
fn narrow_skills_setup() -> Setup {
    let setup = skills_setup();
    // A long-named extension with its own same-name skill, shadowed by
    // the repository copy.
    setup.lua("very-long-extension-name", "-- no tools\n");
    let dir = installed_extension_dir(&setup).join("skills/review");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        "---\nname: review\ndescription: Extension notes.\n---\nBody.\n",
    )
    .unwrap();
    setup
}

/// The narrow `/skills` screen once the winner's row has drawn with its
/// shadows clause tail-cut past both whole switches. The conversation
/// column leaves its last cell to the scroll bar, so the 100-wide pty
/// draws the view 99 wide. The predicate checks every cell property the
/// test reads — the exact row, the gutter, the switch weights and the dim
/// footer — so a partial redraw can't satisfy it (`docs/testing/screens.md`,
/// "Screens"). Only then is the view ready for Enter.
fn narrow_skills_ready(grid: &Grid) -> bool {
    let want = "review · Checks a diff. · repo · seen · this project›[on] · everywhere [on] · shadows personal…";
    let Some(y) = find_row(grid, "shadows personal…") else {
        return false;
    };
    let Some(x) = col_at_or(grid, y, "review · Checks a diff.") else {
        return false;
    };
    let text: String = (x..100)
        .map(|x| grid.cell(x, y).symbol.clone())
        .collect::<String>()
        .trim_end()
        .to_owned();
    if text != want {
        return false;
    }
    if x < 2 || grid.cell(x - 2, y).symbol != "›" || grid.cell(x - 1, y).symbol != " " {
        return false;
    }
    if grid.cell(x - 2, y).inverse {
        return false;
    }
    if !(0..100).all(|x| !grid.cell(x, y).inverse) {
        return false;
    }
    let after = |label: &str| col_at_or(grid, y, label).map(|x| x + label.chars().count() as u16);
    let (Some(at_project), Some(at_everywhere)) = (after("this project"), after("everywhere"))
    else {
        return false;
    };
    let project = grid.cell(at_project, y);
    let everywhere = grid.cell(at_everywhere, y);
    if project.symbol != "›" || !project.bold || everywhere.symbol != " " || everywhere.bold {
        return false;
    }
    if !(switch_weighs(grid, y, at_project + 1, "[on]")
        && switch_weighs(grid, y, at_everywhere + 1, "[on]"))
    {
        return false;
    }
    let Some(footer) = find_row(grid, "Enter shows the text · Ctrl+G opens its file") else {
        return false;
    };
    is_dim(grid, footer, "Enter shows the text · Ctrl+G opens its file")
}

#[test]
fn the_skills_view_keeps_switches_whole_on_a_narrow_screen() {
    let setup = narrow_skills_setup();
    let mut run = Run::spawn(&setup, 100, 24, &[], &TRUECOLOUR);
    run.ready();
    // `/skills` reads the session on screen, so start one first.
    let from = run.output().len();
    run.write(b"go\r");
    run.turn_finished(from);
    run.write(b"/skills\r");
    let grid = run.wait_screen("the narrow skills rows", narrow_skills_ready);
    // The winner's row: both switches whole, the shadows clause tail-cut
    // past them.
    let y = row_with(&grid, "shadows personal…");
    let x = col_at(&grid, y, "review · Checks a diff.");
    assert_eq!(grid.cell(x - 2, y).symbol, "›");
    assert!(!grid.cell(x - 2, y).inverse);
    let text: String = (x..100)
        .map(|x| grid.cell(x, y).symbol.clone())
        .collect::<String>()
        .trim_end()
        .to_owned();
    assert_eq!(
        text,
        "review · Checks a diff. · repo · seen · this project›[on] · everywhere [on] · shadows personal…"
    );
    // No cell of the row is reversed; `[on]` is bold and not dim on both
    // switches.
    assert!(
        (0..100).all(|x| !grid.cell(x, y).inverse),
        "{}",
        grid.contents
    );
    let after = |label: &str| col_at(&grid, y, label) + label.chars().count() as u16;
    assert!(switch_weighs(&grid, y, after("this project") + 1, "[on]"));
    assert!(switch_weighs(&grid, y, after("everywhere") + 1, "[on]"));
    // The shadowed personal copy keeps its own row with no clause.
    let loser = row_with(&grid, "review · Personal notes.");
    assert!(
        !grid.rows[loser as usize].contains("shadows"),
        "{}",
        grid.contents
    );
    // Enter lists every shadow source and path above the text
    // (`docs/tui.md:892-893`). The long extension line wraps at the view's
    // width, so the paths are read across the pane's wrapped rows. The rows
    // above already prove the view ready for Enter
    // (`docs/testing/screens.md`, "Screens").
    run.write(b"\r");
    let personal_path = setup.home().join("skills/review/SKILL.md");
    let extension_path = installed_extension_dir(&setup).join("skills/review/SKILL.md");
    let personal_label = "shadows personal ·";
    let extension_label = format!("shadows {LONG_EXTENSION} ·");
    let grid = run.wait_screen("the shadow list", |grid| {
        find_row(grid, personal_label).is_some() && find_row(grid, &extension_label).is_some()
    });
    // Every shadowed path reads whole across the wrapped rows: paths hold
    // no space, and each wrapped row is trimmed, so joining the rows
    // recovers one broken inside a word.
    let pane: String = grid
        .rows
        .iter()
        .map(|row| row.trim())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        pane.contains(personal_path.to_str().unwrap()),
        "{}",
        grid.contents
    );
    assert!(
        pane.contains(extension_path.to_str().unwrap()),
        "{}",
        grid.contents
    );
    // Path order (`docs/invocation.md:474` discovery order), then a blank
    // line, then the skill's text: the hub returns the whole SKILL.md,
    // frontmatter included.
    let shadowed = row_with(&grid, personal_label);
    let extension_row = row_with(&grid, &extension_label);
    let fence = row_with(&grid, "---");
    let body = row_with(&grid, "Body.");
    assert!(shadowed < extension_row && extension_row < fence && fence < body);
    assert!(
        grid.rows[fence as usize - 1].trim().is_empty(),
        "{}",
        grid.contents
    );
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}
