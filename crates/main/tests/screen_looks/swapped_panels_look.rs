//! Binary-level tests of the `/tools`, `/rules`, `/settings` and `/skills`
//! views (`docs/tui.md`, "Swapped views"): the real binary on a 160x48 pty
//! in truecolour, with the scripted provider answering, draws the doc's
//! rows. Source headings are bold, a switch reads `[on]` bold or `[off]`
//! dim, a rule's ✕ is in `error`, the settings view has a dim note, a bold
//! open tab and a footer with bold keys, and each view's last rows are
//! dim. The focused row
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

use crate::support;

use std::fs;
use std::path::PathBuf;

use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// `error` in the dark theme.
const ERROR: Colour = Colour::Rgb(255, 93, 115);
/// The dark page: the panel tint (`docs/tui.md`, "Themes").
const PAGE: Colour = Colour::Rgb(12, 12, 17);

/// `accent` in the dark theme: a focused row's label and mark.
const ACCENT: Colour = Colour::Rgb(110, 170, 254);

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

/// The first row holding `needle` left of the panel: the Changed files
/// list's rows draw in the conversation column, while the panel repeats
/// the file names on the right.
fn list_row(grid: &Grid, needle: &str) -> Option<u16> {
    let (_, height) = grid.size();
    (0..height).find(|y| col_at_or(grid, *y, needle).is_some_and(|x| x < 100))
}

/// The first list row holding `needle`, or a panic naming it.
fn list_row_with(grid: &Grid, needle: &str) -> u16 {
    list_row(grid, needle).unwrap_or_else(|| row_with(grid, needle))
}

/// The list row's text past the inset gutter: the row spans the full
/// grid with the panel on the right, so exact asserts match this prefix.
fn list_text(grid: &Grid, y: u16) -> String {
    grid.rows[usize::from(y)].trim_start().to_owned()
}

/// The grid's text for a failure message: one read line here instead of
/// one per assert.
fn contents(grid: &Grid) -> String {
    grid.contents.clone()
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

/// Whether every cell of `needle`, found on `y`, is bold in the accent
/// colour: a focused row's label (`docs/tui.md`, "Swapped views").
fn is_focused_label(grid: &Grid, y: u16, needle: &str) -> bool {
    col_at_or(grid, y, needle).is_some_and(|x| {
        (0..needle.chars().count() as u16).all(|dx| {
            let cell = grid.cell(x + dx, y);
            cell.bold && cell.fg == ACCENT
        })
    })
}

/// Whether `needle` on `y` is the focused file row: the "› " mark in
/// accent in the gutter and its name bold in accent (`docs/tui.md`,
/// "Swapped views"). The file rows indent past the gutter, so the mark
/// is the row's first non-blank cell rather than the cell before the
/// name.
fn is_focused_row(grid: &Grid, y: u16, needle: &str) -> bool {
    let Some(x) = col_at_or(grid, y, needle) else {
        return false;
    };
    if x < 2 {
        return false;
    }
    let mark = (0..x).find_map(|at| {
        let cell = grid.cell(at, y);
        (cell.symbol != " ").then_some((cell.symbol.clone(), cell.fg))
    });
    if mark
        .as_ref()
        .is_none_or(|(symbol, fg)| symbol != "›" || *fg != ACCENT)
    {
        return false;
    }
    is_focused_label(grid, y, needle)
}

/// The `/tools` row for `notes_save`: its text from the name on.
fn tool_row(grid: &Grid) -> Option<(u16, String)> {
    let y = find_row(grid, " notes_save · full · ")?;
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

/// The label the "› " gutter marks in the settings view, if one does: the
/// text after the gutter up to the first double space. The conversation
/// panel to the right never starts a row with the mark.
fn focused_label(grid: &Grid) -> Option<String> {
    grid.rows.iter().find_map(|row| {
        let rest = row.trim_start().strip_prefix("› ")?;
        Some(rest.split("  ").next().unwrap_or(rest).trim().to_owned())
    })
}

#[test]
fn the_settings_view_matches_the_doc() {
    let setup = tools_setup(json!({}));
    let mut run = turned(&setup);
    run.write(b"/settings\r");
    let keys = "←→ tab · / search · Enter change · Tab layer · Delete reset";
    // The wait checks every property the asserts below read, so a partial
    // redraw cannot satisfy it.
    let mut grid = run.wait_screen("the settings tabs", |grid| {
        let (Some(note), Some(bar), Some(footer)) = (
            find_row(grid, "Changes save automatically"),
            find_row(grid, "Appearance"),
            find_row(grid, "Delete reset"),
        ) else {
            return false;
        };
        is_dim(grid, note, "Changes save automatically")
            && bar == note + 2
            && is_bold(grid, bar, "Appearance")
            && col_at_or(grid, bar, "Models").is_some()
            && !is_bold(grid, bar, "Models")
            && grid.rows[usize::from(footer)].trim() == keys
            && is_bold(grid, footer, "Enter")
            && col_at_or(grid, footer, "change").is_some()
            && !is_bold(grid, footer, "change")
    });
    // Under the name row: the dim note, then the tab bar with the open
    // tab bold.
    let note = row_with(&grid, "Changes save automatically");
    assert!(is_dim(&grid, note, "Changes save automatically"));
    let bar = row_with(&grid, "Appearance");
    // settled: "the settings tabs" checked the bar row and the note.
    assert_eq!(bar, note + 2, "{}", grid.contents);
    assert!(is_bold(&grid, bar, "Appearance"));
    assert!(!is_bold(&grid, bar, "Models"));
    // The footer names its keys, the keys bold.
    let footer = row_with(&grid, "Delete reset");
    // settled: "the settings tabs" checked the footer text.
    assert_eq!(grid.rows[usize::from(footer)].trim(), keys);
    assert!(is_bold(&grid, footer, "Enter"));
    assert!(!is_bold(&grid, footer, "change"));
    // Notifications: wait for the slider, then move to Bell before using
    // Right to leave the tab.
    run.write(b"\x1b[C");
    run.wait_screen("the focused Quota notice slider", |grid| {
        focused_label(grid).as_deref() == Some("Quota notice")
    });
    run.write(b"\x1b[B");
    run.wait_screen("the focused Bell row", |grid| {
        focused_label(grid).as_deref() == Some("Bell")
    });
    run.write(b"\x1b[C");
    run.wait_screen("the Models tab", |grid| {
        find_row(grid, "Default model").is_some_and(|y| {
            let text = grid.rows[usize::from(y)].trim_end();
            text.contains("scripted/s.json ›") && !text.contains("\"scripted/s.json\"")
        }) && find_row(grid, "Models").is_some_and(|y| is_bold(grid, y, "Models"))
    });
    grid = run.wait_screen("the focused non-numeric Models row", |grid| {
        focused_label(grid).as_deref() == Some("Default model")
    });
    let model = row_with(&grid, "Default model");
    // settled: "the Models tab" checked the row text.
    let text = grid.rows[usize::from(model)].trim_end().to_owned();
    assert!(text.contains("scripted/s.json ›"), "{text}");
    assert!(!text.contains("\"scripted/s.json\""), "unquoted: {text}");
    assert!(is_bold(&grid, find_row(&grid, "Models").unwrap(), "Models"));
    // Move to Permissions and find its non-numeric Reviewer model row. The
    // tab also contains the screen-covered Reviewer notes row.
    run.write(b"\x1b[C");
    let mut grid = run.wait_screen("the Permissions tab", |grid| {
        find_row(grid, "Reviewer model").is_some()
            && find_row(grid, "Permissions").is_some_and(|row| is_bold(grid, row, "Permissions"))
    });
    for _ in 0..12 {
        if focused_label(&grid).as_deref() == Some("Reviewer model") {
            break;
        }
        let before = focused_label(&grid);
        run.write(b"\x1b[B");
        grid = run.wait_screen("the Permissions focus moved", |next| {
            focused_label(next) != before
        });
    }
    assert_eq!(focused_label(&grid).as_deref(), Some("Reviewer model"));
    // Move to Sessions; walk the focus to `compaction.tokens` below.
    run.write(b"\x1b[C");
    grid = run.wait_screen("the Sessions tab", |grid| {
        find_row(grid, "Compaction tokens").is_some()
    });
    for _ in 0..40 {
        let on = focused_label(&grid);
        if on.as_deref() == Some("Compaction tokens") {
            break;
        }
        run.write(b"\x1b[B");
        grid = run.wait_screen("the gutter moved", |next| focused_label(next) != on);
    }
    // Under the list: the description, then the key's path, where it is
    // set and what a change costs, dim, ahead of the footer.
    let reload = "compaction.tokens · using the default · changing compaction.tokens needs a reload (~84k tokens)";
    // No input is pending: this wait checks every property read below, so
    // the frame the gutter moved onto is whole.
    grid = run.wait_screen("the Compaction tokens row whole", |grid| {
        let (Some(compaction), Some(detail), Some(footer)) = (
            find_row(grid, "Compaction tokens"),
            find_row(grid, "compaction.tokens ·"),
            find_row(grid, "Delete reset"),
        ) else {
            return false;
        };
        focused_label(grid).as_deref() == Some("Compaction tokens")
            && grid.rows[usize::from(compaction)].contains("400k tokens")
            && is_focused_label(grid, compaction, "Compaction tokens")
            && grid.rows[usize::from(detail)].trim() == reload
            && is_dim(grid, detail, reload)
            && detail + 1 == footer
            && !grid.contents.contains("Ctrl+G opens the file")
    });
    assert_eq!(focused_label(&grid).as_deref(), Some("Compaction tokens"));
    let compaction = row_with(&grid, "Compaction tokens");
    assert!(
        // settled: "the Compaction tokens row whole" checked the value.
        grid.rows[usize::from(compaction)].contains("400k tokens"),
        "{}",
        // settled: "the Compaction tokens row whole" checked the value.
        grid.contents
    );
    assert!(is_focused_label(&grid, compaction, "Compaction tokens"));
    let detail = row_with(&grid, "compaction.tokens ·");
    assert_eq!(
        // settled: "the Compaction tokens row whole" checked the detail row.
        grid.rows[usize::from(detail)].trim(),
        reload,
        "{}",
        // settled: "the Compaction tokens row whole" checked the detail row.
        grid.contents
    );
    assert!(is_dim(&grid, detail, reload));
    // settled: "the Compaction tokens row whole" checked both rows and the text.
    assert_eq!(detail + 1, footer, "{}", grid.contents);
    // settled: "the Compaction tokens row whole" checked the text is absent.
    assert!(!grid.contents.contains("Ctrl+G opens the file"));
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

#[test]
fn dark_settings_body_sits_on_the_panel_tint() {
    // Every swapped view draws on the page: the dark page paints on the
    // panel tint, so the terminal's own background shows nowhere
    // (`docs/tui.md`, "Themes").
    let setup = tools_setup(json!({}));
    let mut run = turned(&setup);
    run.write(b"/settings\r");
    let keys = "←→ tab · / search · Enter change · Tab layer · Delete reset";
    // debt: two "the settings tabs" predicates stay (this one and
    // the_settings_view_matches_the_doc's); the grid-read ratchet caps
    // per-file unmarked reads (look.rs 163, this file 50;
    // xtask/src/screen_ratchet.rs) and a per-file helper's reads count
    // where it is defined, so a shared helper waits for a third
    // predicate; support/ is outside the fence.
    let grid = run.wait_screen("the settings tabs", |grid| {
        let (Some(note), Some(bar), Some(footer)) = (
            find_row(grid, "Changes save automatically"),
            find_row(grid, "Appearance"),
            find_row(grid, "Delete reset"),
        ) else {
            return false;
        };
        is_dim(grid, note, "Changes save automatically")
            // The tab bar sits below the dim note: the view's name row,
            // then "Changes save automatically" dim, then the tab bar
            // (`docs/tui.md`, "Swapped views").
            && bar > note
            && is_bold(grid, bar, "Appearance")
            && col_at_or(grid, bar, "Models").is_some()
            && !is_bold(grid, bar, "Models")
            && grid.rows[usize::from(footer)].trim() == keys
            && is_bold(grid, footer, "Enter")
            && col_at_or(grid, footer, "change").is_some()
            && !is_bold(grid, footer, "change")
    });
    // settled: "the settings tabs" checked the bar row and the note.
    let note = row_with(&grid, "Changes save automatically");
    // The note row's trailing cells are blank page: the panel tint.
    let last = (0..160)
        .rfind(|x| grid.cell(*x, note).symbol.as_str() != " ")
        .expect("the note row's text");
    // settled: "the settings tabs" checked the note text with no input
    // since.
    for x in last + 1..160 {
        let cell = grid.cell(x, note);
        assert_eq!(cell.symbol.as_str(), " ", "body cell ({x}, {note})");
        assert_eq!(cell.bg, PAGE, "body cell ({x}, {note})");
    }
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

/// A Changed files card item's cell in the panel: the first cell at or
/// after the panel columns holding `needle`.
fn panel_item_at(grid: &Grid, needle: &str) -> Option<(u16, u16)> {
    grid.rows.iter().enumerate().find_map(|(y, _)| {
        let y = y as u16;
        let x = col_at_or(grid, y, needle)?;
        (x >= 100).then_some((x, y))
    })
}

#[test]
fn the_changed_files_untracked_row_reads_added_with_its_line_count() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "test@fiber.dev"]);
    git(&workspace, &["config", "user.name", "fiber test"]);
    fs::write(workspace.join("initial.txt"), "base\n").unwrap();
    git(&workspace, &["add", "initial.txt"]);
    git(&workspace, &["commit", "-m", "base"]);
    write_json(
        &workspace.join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "write", "arguments": {"path": "new.txt", "content": "one\ntwo\nthree\n"}}]},
            {"tool_calls": [{"name": "edit", "arguments": {"path": "new.txt", "edits": [
                {"old_text": "two\n", "new_text": "TWO\n"},
            ]}}]},
            {"text": ["Done." ]},
        ]}),
    );
    write_json(
        &workspace.join("r.json"),
        &json!({"steps": [{"text": ["allow"]}, {"text": ["allow"]}]}),
    );
    config(&setup, json!({"reviewer": {"model": "scripted/r.json"}}));
    let mut run = turned(&setup);
    let grid = run.wait_screen("the changed files card", |grid| {
        panel_item_at(grid, "new.txt").is_some()
    });
    // The card keeps Fiber's own edit sums: three added lines from the
    // write and one changed line from the edit.
    let (_, y) = panel_item_at(&grid, "new.txt").unwrap();
    let x = col_at(&grid, y + 1, "+4");
    // The totals row opens the Changed files list.
    click(&mut run, x, y + 1);
    let grid = run.wait_screen("the untracked file row", |grid| {
        grid.rows[0].contains("Changed files")
            && list_row(grid, "new.txt")
                .is_some_and(|y| grid.rows[usize::from(y)].contains("    A  new.txt  +3 −0"))
    });
    let y = list_row_with(&grid, "new.txt");
    // settled: the wait above matched the whole file row.
    assert!(is_bold(&grid, y, "new.txt"), "{}", contents(&grid));
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

#[test]
fn the_repeatedly_edited_tracked_file_row_matches_numstat() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    git(&workspace, &["init"]);
    git(&workspace, &["config", "user.email", "test@fiber.dev"]);
    git(&workspace, &["config", "user.name", "fiber test"]);
    fs::write(workspace.join("f.txt"), "a\nb\nc\nd\ne\n").unwrap();
    git(&workspace, &["add", "f.txt"]);
    git(&workspace, &["commit", "-m", "add f"]);
    write_json(
        &workspace.join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "edit", "arguments": {"path": "f.txt", "edits": [
                {"old_text": "a\n", "new_text": "a1\n"},
            ]}}]},
            {"tool_calls": [{"name": "edit", "arguments": {"path": "f.txt", "edits": [
                {"old_text": "a1\n", "new_text": "a2\n"},
            ]}}]},
            {"tool_calls": [{"name": "edit", "arguments": {"path": "f.txt", "edits": [
                {"old_text": "b\n", "new_text": "b1\n"},
            ]}}]},
            {"text": ["Done." ]},
        ]}),
    );
    write_json(
        &workspace.join("r.json"),
        &json!({"steps": [{"text": ["allow"]}, {"text": ["allow"]}, {"text": ["allow"]}]}),
    );
    config(&setup, json!({"reviewer": {"model": "scripted/r.json"}}));
    let mut run = turned(&setup);
    let grid = run.wait_screen("the changed files card", |grid| {
        panel_item_at(grid, "f.txt").is_some()
    });
    // The card keeps Fiber's own edit sums: each of the three edits
    // adds and removes one line.
    let (_, y) = panel_item_at(&grid, "f.txt").unwrap();
    let x = col_at(&grid, y + 1, "+3");
    // The totals row opens the Changed files list.
    click(&mut run, x, y + 1);
    // Git's own figures for the path, read after the turn's edits.
    let out = std::process::Command::new("git")
        .args(["diff", "--numstat", "HEAD", "--", "f.txt"])
        .current_dir(&workspace)
        .output()
        .unwrap_or_else(|error| panic!("git numstat: {error}"));
    assert!(out.status.success(), "git numstat: {out:?}");
    let numstat =
        String::from_utf8(out.stdout).unwrap_or_else(|error| panic!("numstat UTF-8: {error}"));
    let mut fields = numstat.split_whitespace();
    let (added, removed) = (fields.next().unwrap_or("?"), fields.next().unwrap_or("?"));
    let wanted = format!("+{added} \u{2212}{removed}");
    let grid = run.wait_screen("the tracked file row with git figures", |grid| {
        grid.rows[0].contains("Changed files")
            && list_row(grid, "f.txt").is_some_and(|y| grid.rows[usize::from(y)].contains(&wanted))
    });
    let y = list_row_with(&grid, "f.txt");
    // settled: the wait above matched the tracked file row with git figures.
    assert!(list_text(&grid, y).contains(&wanted), "{}", contents(&grid));
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

#[test]
fn the_changed_files_list_groups_folders_with_letters_figures_totals_and_focus() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    write_json(
        &workspace.join("s.json"),
        &json!({"steps": [
            {"tool_calls": [
                {"name": "write", "arguments": {"path": "src/one.rs", "content": "1\n2\n3\n4\n5\n6\n7\n8\n9\n10\n11\n12\n"}},
                {"name": "write", "arguments": {"path": "top.txt", "content": "a\nb\nc\n"}},
            ]},
            {"text": ["Done."]},
        ]}),
    );
    write_json(
        &workspace.join("r.json"),
        &json!({"steps": [{"text": ["allow"]}, {"text": ["allow"]}]}),
    );
    config(&setup, json!({"reviewer": {"model": "scripted/r.json"}}));
    let mut run = turned(&setup);
    run.wait_screen("the changed files card", |grid| {
        panel_item_at(grid, "one.rs").is_some()
    });
    // The card sums Fiber's own writes: twelve lines plus three; the
    // totals row opens the Changed files list (`docs/testing/screens.md`,
    // "Screens": the frame-locked helper finds the target in the frame it
    // acts on).
    run.click_where("the card totals row", |grid| panel_item_at(grid, "+15"));
    let grid = run.wait_screen("the grouped list", |grid| {
        grid.rows[0].contains("Changed files")
            && list_row(grid, "./").is_some()
            && list_row(grid, "src/").is_some()
            && list_row(grid, "top.txt").is_some_and(|y| grid.rows[usize::from(y)].contains(" +3"))
            && list_row(grid, "one.rs").is_some_and(|y| grid.rows[usize::from(y)].contains("+12"))
            && list_row(grid, "2 files").is_some()
    });
    // `docs/tui.md`, "Swapped views" Changed files Folders promises this
    // order: folders in path order with their files under them in name
    // order ("./" holds the top-level file, "src/" the nested one).
    let dot = list_row_with(&grid, "./");
    let top = list_row_with(&grid, "top.txt");
    let src = list_row_with(&grid, "src/");
    let one = list_row_with(&grid, "one.rs");
    assert!(dot < top && top < src && src < one, "{}", contents(&grid));
    // The figures align in one column: the shorter count pads left.
    // The rows span the full grid with the panel on the right, so the
    // asserts trim the inset gutter and match the list text's prefix.
    // settled: the wait above matched the focused file row's figures.
    assert!(
        list_text(&grid, top).starts_with("›     M  top.txt   +3 −0"),
        "{}",
        contents(&grid)
    );
    // settled: the wait above matched the nested file row's figures.
    // "one.rs" (6) pads to "top.txt" (7), so its figures share the column.
    assert!(
        list_text(&grid, one).starts_with("M  one.rs   +12 −0"),
        "{}",
        contents(&grid)
    );
    // The first file row has the focus.
    // settled: the wait above matched the focused file row's figures.
    assert!(is_focused_row(&grid, top, "top.txt"), "{}", contents(&grid));
    // The totals line the file count with the summed figures.
    let totals = list_row_with(&grid, "2 files");
    // settled: the wait above matched the totals row.
    assert!(
        list_text(&grid, totals).starts_with("2 files  +15 −0"),
        "{}",
        contents(&grid)
    );
    // settled: the wait above matched the totals row.
    assert!(is_dim(&grid, totals, "2 files"), "{}", contents(&grid));
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

#[test]
fn the_changed_files_committed_file_stays_dim_with_no_exit_code() {
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
            {"tool_calls": [{"name": "edit", "arguments": {"path": "f.txt", "edits": [
                {"old_text": "call bar(a);\n", "new_text": "call foo(a);\n"},
            ]}}]},
            {"text": ["Done."]},
        ]}),
    );
    write_json(
        &workspace.join("r.json"),
        &json!({"steps": [{"text": ["allow"]}, {"text": ["allow"]}]}),
    );
    config(&setup, json!({"reviewer": {"model": "scripted/r.json"}}));
    let mut run = turned(&setup);
    run.wait_screen("the changed files card", |grid| {
        panel_item_at(grid, "f.txt").is_some()
    });
    // The totals row opens the Changed files list (`docs/testing/screens.md`,
    // "Screens": the frame-locked helper finds the target in the frame it
    // acts on).
    run.click_where("the totals row", |grid| {
        let (_, y) = panel_item_at(grid, "f.txt")?;
        let x = col_at_or(grid, y + 1, "+2")?;
        Some((x, y + 1))
    });
    // The undone file reads dim without being chosen: the figures
    // answer omits it and tracking does not call it untracked.
    let grid = run.wait_screen("the dim no-diff row", |grid| {
        grid.rows[0].contains("Changed files")
            && list_row(grid, "f.txt")
                .is_some_and(|y| grid.rows[usize::from(y)].contains("no changes"))
    });
    let y = list_row_with(&grid, "f.txt");
    // settled: the wait above matched the whole no-diff row.
    assert!(is_dim(&grid, y, "no changes"), "{}", contents(&grid));
    // Choosing the file reads its empty diff against HEAD.
    run.write(b"\r");
    run.wait_screen("the no-diff sentence", |grid| {
        grid.rows[0].contains("Changed files")
            && find_row(grid, "No changes against HEAD: committed or undone").is_some()
            // The sentence proves the whole diff answer arrived, so no
            // "Exit code" closing line reaches the screen with it.
            && !grid.contents.contains("Exit code")
    });
    // Back in the list, the undone file stays listed and dim.
    run.write(b"\x1b[D");
    let grid = run.wait_screen("the dim no-diff row after choosing", |grid| {
        grid.rows[0].contains("Changed files")
            && list_row(grid, "f.txt")
                .is_some_and(|y| grid.rows[usize::from(y)].contains("no changes"))
    });
    let y = list_row_with(&grid, "f.txt");
    // settled: the wait above matched the whole no-diff row.
    assert!(is_dim(&grid, y, "no changes"), "{}", contents(&grid));
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}

#[test]
fn the_changed_files_open_in_editor_click_runs_the_same_path_as_ctrl_g() {
    let setup = Setup::new();
    let workspace = fs::canonicalize(setup.workspace()).unwrap();
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
    // The editor records its arguments (`+N`, the path) and appends a line
    // to the file it is given, so the binary's reread shows the appended
    // hunk: that screen is the editor-return signal, and the test reads the
    // recorded arguments only after it, never polling the file while the
    // editor may still run (`docs/testing.md`, "Waits and timeouts").
    // (`docs/testing/screens.md`, "Screens": a read outside a wait follows
    // the wait that settled its cells.)
    let editor = workspace.join("editor.sh");
    let argv = workspace.join("argv");
    let write_editor = |marker: &str| {
        fs::write(
            &editor,
            format!(
                "printf '%s\\n' \"$@\" >> \"{}\"\nprintf '{}\\n' >> \"$2\"\n",
                argv.display(),
                marker
            ),
        )
        .unwrap();
    };
    write_editor("call baz1(a);");
    let editor_cmd = format!("/bin/sh {}", editor.display());
    let env = [
        ("EDITOR", editor_cmd.as_str()),
        TRUECOLOUR[0],
        TRUECOLOUR[1],
        TRUECOLOUR[2],
    ];
    let mut run = Run::spawn(&setup, 160, 48, &[], &env);
    run.ready();
    let from = run.output().len();
    run.write(b"go\r");
    run.turn_finished(from);
    run.wait_screen("the changed files card", |grid| {
        panel_item_at(grid, "f.txt").is_some()
    });
    // The totals row opens the Changed files list (`docs/testing/screens.md`,
    // "Screens": the frame-locked helper finds the target in the frame it
    // acts on).
    run.click_where("the totals row", |grid| {
        let (_, y) = panel_item_at(grid, "f.txt")?;
        let x = col_at_or(grid, y + 1, "+1")?;
        Some((x, y + 1))
    });
    run.wait_screen("the file row", |grid| {
        grid.rows[0].contains("Changed files") && list_row(grid, "f.txt").is_some()
    });
    // Choosing the file shows its hunks under the editor target.
    run.write(b"\r");
    run.wait_screen("the file diff with the editor target", |grid| {
        grid.rows[0].contains("Changed files")
            && find_row(grid, "Open in editor ›").is_some()
            && find_row(grid, "+call bar(a);").is_some()
    });
    // The click needs no row-position signal beyond the frame-locked
    // helper: it finds the target in the frame it acts on.
    run.click_where("the Open in editor target", |grid| {
        grid.text_at("Open in editor ›")
    });
    // The appended hunk proves the click's editor exited and the view
    // reread: the recorded arguments are settled, so read them directly.
    // settled: the wait above matched the click's appended hunk.
    run.wait_screen("the click's edited hunk", |grid| {
        grid.rows[0].contains("Changed files") && find_row(grid, "+call baz1(a);").is_some()
    });
    let wanted = vec![
        "+1".to_owned(),
        workspace.join("f.txt").display().to_string(),
    ];
    let recorded: Vec<String> = fs::read_to_string(&argv)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(recorded, wanted, "the click runs the editor at +1");
    // The second run appends a distinct line, so its return has its own
    // signal. Rewriting the script after the first return is safe: the
    // first editor has exited, as its hunk proves.
    fs::write(&argv, "").unwrap();
    write_editor("call baz2(a);");
    run.write(b"\x07");
    // settled: the wait below matched the Ctrl+G appended hunk.
    run.wait_screen("the Ctrl+G edited hunk", |grid| {
        grid.rows[0].contains("Changed files") && find_row(grid, "+call baz2(a);").is_some()
    });
    let recorded: Vec<String> = fs::read_to_string(&argv)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        recorded, wanted,
        "Ctrl+G runs the same editor path as the click"
    );
    // Back in the list, as the editor-return reread proves, before
    // quitting from the conversation.
    run.write(b"\x1b[D");
    run.wait_screen("the returned list", |grid| {
        grid.rows[0].contains("Changed files") && list_row(grid, "f.txt").is_some()
    });
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

/// The `/skills` row for the winning `review` skill, with the documented
/// cells in order (`docs/tui.md`, "Swapped views").
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
    let Some(loser) = find_row(grid, "review · Personal notes.") else {
        return false;
    };
    let Some(loser_row) = grid.rows.get(usize::from(loser)) else {
        return false;
    };
    if loser_row.contains("shadows") {
        return false;
    }
    let Some(footer) = find_row(grid, "Enter shows the text · Ctrl+G opens its file") else {
        return false;
    };
    is_dim(grid, footer, "Enter shows the text · Ctrl+G opens its file")
}

/// Every property read after Enter has drawn: both complete shadow paths in
/// source-precedence order, the blank separator, and the skill text
/// (`docs/tui.md`, "Swapped views"; `docs/testing/screens.md`, "Screens").
fn shadow_list_ready(grid: &Grid, personal_path: &str, extension_path: &str) -> bool {
    let pane: String = grid
        .rows
        .iter()
        .map(|row| row.trim())
        .collect::<Vec<_>>()
        .join("");
    if !pane.contains(personal_path) || !pane.contains(extension_path) {
        return false;
    }
    let extension_label = format!("shadows {LONG_EXTENSION} ·");
    let (Some(personal), Some(extension), Some(fence), Some(body)) = (
        find_row(grid, "shadows personal ·"),
        find_row(grid, &extension_label),
        find_row(grid, "---"),
        find_row(grid, "Body."),
    ) else {
        return false;
    };
    if !(personal < extension && extension < fence && fence < body) {
        return false;
    }
    fence
        .checked_sub(1)
        .and_then(|row| grid.rows.get(usize::from(row)))
        .is_some_and(|row| row.trim().is_empty())
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
    let view_from = run.output().len();
    run.write(b"/skills\r");
    run.wait_bytes(
        view_from,
        b"\x1b[?2026l",
        "the end of the skills view frame",
    );
    run.wait_screen("the narrow skills rows", narrow_skills_ready);
    // The wait predicate checks the full shadow block before the test reads
    // it (`docs/testing/screens.md`, "Screens").
    run.write(b"\r");
    let personal_path = setup.home().join("skills/review/SKILL.md");
    let extension_path = installed_extension_dir(&setup).join("skills/review/SKILL.md");
    run.wait_screen("the shadow list", |grid| {
        shadow_list_ready(
            grid,
            personal_path.to_str().unwrap(),
            extension_path.to_str().unwrap(),
        )
    });
    run.write(b"\x03\x03\r");
    assert_eq!(run.wait().status.code(), Some(0));
}
