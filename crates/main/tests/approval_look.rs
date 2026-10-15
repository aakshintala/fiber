//! Binary-level tests of the approval panel (`docs/tui.md`, "An
//! approval"): the real binary in a pseudo-terminal draws the standing ask
//! and a reviewer's escalation as the prototype draws them, in truecolour
//! and at 256 colours.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::fs;

use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// The standing-ask tint in truecolour.
const APPROVAL_BG: Colour = Colour::Rgb(26, 26, 34);
/// A reviewer's escalation tint in truecolour.
const ALERT_BG: Colour = Colour::Rgb(80, 28, 32);
/// `attention` in the dark theme.
const ATTENTION: Colour = Colour::Rgb(255, 159, 67);
/// `info` in the dark theme.
const INFO: Colour = Colour::Rgb(125, 211, 252);
/// `error` in the dark theme.
const ERROR: Colour = Colour::Rgb(255, 93, 115);

/// The scripted script making one `shell` call.
fn shell_script(command: &str) -> serde_json::Value {
    json!({"steps": [{"tool_calls": [{"name": "shell", "arguments": {"command": command}}]}]})
}

/// A setup whose scripted model makes the shell call `command`.
fn shell_setup(command: &str) -> Setup {
    let setup = Setup::new();
    write_json(&setup.workspace().join("s.json"), &shell_script(command));
    setup
}

/// The config naming the workspace script, with `extra` beside the model.
fn configure(setup: &Setup, extra: serde_json::Value) {
    let mut config = json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}});
    if let Some(into) = config.as_object_mut()
        && let Some(from) = extra.as_object()
    {
        for (key, value) in from {
            into.insert(key.clone(), value.clone());
        }
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// A standing project ask for `prefix`: the project's rules live in Fiber
/// home at `projects/<key>/rules` (`docs/state.md`, "Projects"), placed
/// through the canonical project key.
fn ask_rule(setup: &Setup, prefix: &str) {
    let key = log::project_key(&doors::project(&setup.workspace()));
    let rule = setup.home().join("projects").join(key).join("rules");
    fs::create_dir_all(rule.parent().unwrap()).unwrap();
    fs::write(
        rule,
        format!(
            "{}\n",
            json!({"decision": "ask", "tool": "shell", "prefix": prefix})
        ),
    )
    .unwrap();
}

/// Spawns `fiber` on a 160 by 48 terminal with `env` after the cleared
/// environment, so every run keeps today's screen.
fn terminal(setup: &Setup, env: &[(&str, &str)]) -> Run {
    Run::spawn(setup, 160, 48, &[], env)
}

/// The first row holding `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    u16::try_from(
        grid.rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents)),
    )
    .unwrap_or(u16::MAX)
}

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    let want: Vec<char> = needle.chars().collect();
    for x in 0..160u16 {
        let mut ok = !want.is_empty();
        for (dx, want) in want.iter().enumerate() {
            let xi = x.saturating_add(u16::try_from(dx).unwrap_or(u16::MAX));
            if xi >= 160 || grid.cell(xi, y).symbol != want.to_string() {
                ok = false;
                break;
            }
        }
        if ok {
            return x;
        }
    }
    panic!("{needle:?} on row {y}")
}

/// Quits the run: two interrupts and Enter, as the journeys quit.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    run.wait_screen("the primary screen with the resume line", |grid| {
        !grid.alternate_screen && !grid.hide_cursor && grid.contents.contains("fiber resume")
    });
    let output = run.wait();
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn a_standing_ask_draws_the_panel_typing_feeds_back_and_esc_puts_it_aside() {
    let setup = shell_setup("cargo mutants -p hub");
    configure(&setup, json!({}));
    ask_rule(&setup, "cargo");
    let mut run = terminal(&setup, &TRUECOLOUR);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"run it\r");
    // The session shows its name, the first prompt: the first frame can
    // still carry the session id, so the wait holds until the named
    // header draws.
    let grid = run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("Approval 1 of 1 · run it")
    });
    let header = row_with(&grid, "Approval 1 of 1");
    // The panel's tinted run: every cell of the panel's text rows takes
    // the approval tint.
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, header).bg == APPROVAL_BG)
        .expect("the tinted run");
    assert!(
        grid.rows[usize::from(header)].contains("Approval 1 of 1 · run it · irreversible"),
        "{}",
        grid.contents
    );
    // Every cell of the panel's text rows takes the approval tint, the
    // stripe its `attention` cell, and the edges draw above and below:
    // header, tool, facts, blank, two choices, blank, hint.
    for x in 0..end + 1 {
        assert_eq!(grid.cell(x, header - 1).symbol.as_str(), "▄", "top {x}");
        assert_eq!(grid.cell(x, header + 8).symbol.as_str(), "▀", "bottom {x}");
    }
    let text: Vec<u16> = (header..header + 8).collect();
    for y in text.iter().copied() {
        for x in 0..end + 1 {
            assert_eq!(grid.cell(x, y).bg, APPROVAL_BG, "({x}, {y})");
        }
        assert_eq!(grid.cell(0, y).symbol.as_str(), "▌", "row {y}");
        assert_eq!(grid.cell(0, y).fg, ATTENTION, "row {y}");
    }
    // No row holds the call's JSON.
    assert!(
        !grid.rows.iter().any(|row| row.contains("{\"")),
        "{:?}",
        grid.rows
    );
    // The header: "Approval 1 of 1" bold, the session in `attention`,
    // " · irreversible" in `error`, "Esc puts it aside" dim at the end.
    let head = col_at(&grid, header, "Approval 1 of 1");
    for dx in 0..15 {
        let cell = grid.cell(head + dx, header);
        assert_eq!(cell.fg, ATTENTION, "col {dx}");
        assert!(cell.bold, "col {dx}");
    }
    let esc = col_at(&grid, header, "Esc puts it aside");
    for dx in 0..17 {
        let cell = grid.cell(esc + dx, header);
        assert!(cell.dim, "col {dx}");
    }
    // Right-aligned: the tint's last cell is the hint's last one.
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, header).bg == APPROVAL_BG)
        .expect("the tinted run");
    assert_eq!(end, esc + 16);
    let loud = col_at(&grid, header, "irreversible");
    for dx in 0..12 {
        let cell = grid.cell(loud + dx, header);
        assert_eq!(cell.fg, ERROR, "col {dx}");
        assert!(cell.bold, "col {dx}");
    }
    // The session in `attention`, not bold.
    let session = col_at(&grid, header, "run it");
    for dx in 0..6 {
        let cell = grid.cell(session + dx, header);
        assert_eq!(cell.fg, ATTENTION, "col {dx}");
        assert!(!cell.bold, "col {dx}");
    }
    // The tool's name and its main argument, then the facts line: the
    // tool row sits right below the header.
    let tool = header + 1;
    assert!(
        grid.rows[usize::from(tool)].contains("shell cargo mutants -p hub"),
        "{}",
        grid.contents
    );
    let shell = col_at(&grid, tool, "shell");
    assert_eq!(grid.cell(shell, tool).fg, INFO);
    for dx in [0, 4, 6, 25] {
        assert!(grid.cell(shell + dx, tool).bold, "col {dx}");
    }
    let facts = tool + 1;
    assert!(
        grid.rows[usize::from(facts)].contains("runs a command · asked by the project rule cargo"),
        "{}",
        grid.contents
    );
    // The facts line is dim.
    let runs = col_at(&grid, facts, "runs a command");
    for dx in [0, 13] {
        assert!(grid.cell(runs + dx, facts).dim, "col {dx}");
    }
    let cargo = col_at(&grid, facts, "cargo");
    for dx in [0, 4] {
        assert!(grid.cell(cargo + dx, facts).dim, "col {dx}");
    }
    // The choices, numbered, and the hint: the focused mark in
    // `attention", its label bold, the number dim, the hint dim.
    let focused = row_with(&grid, "▸ 1 Allow once");
    let pointed = col_at(&grid, focused, "▸");
    assert_eq!(grid.cell(pointed, focused).fg, ATTENTION);
    let chosen = col_at(&grid, focused, "Allow once");
    for dx in [0, 5, 9] {
        let cell = grid.cell(chosen + dx, focused);
        assert!(cell.bold, "col {dx}");
    }
    let number = col_at(&grid, focused, "1 Allow once");
    assert!(grid.cell(number, focused).dim);
    row_with(&grid, "2 Deny");
    let hint = row_with(&grid, "↑↓ choose");
    let hint_at = col_at(&grid, hint, "↑↓ choose");
    for dx in [0, 10, 30] {
        assert!(grid.cell(hint_at + dx, hint).dim, "col {dx}");
    }
    // Typing "2" is feedback, not a choice: the panel stays open with
    // "2" on deny, and no reply goes out.
    run.write(b"2");
    let grid = run.wait_screen("the feedback", |grid| grid.contents.contains("Deny  2"));
    assert!(grid.contents.contains("Allow once"), "the panel stays open");
    // Esc puts the request aside behind its badge above the working line.
    run.write(b"\x1b[27u");
    let grid = run.wait_screen("the badge", |grid| {
        grid.contents.contains("approvals waiting")
    });
    let badge = row_with(&grid, "approvals waiting");
    assert!(
        grid.rows[usize::from(badge)].contains("1 approvals waiting · click to reopen"),
        "{}",
        grid.contents
    );
    let warning = col_at(&grid, badge, "⚠");
    let mark = grid.cell(warning, badge);
    assert_eq!(mark.fg, ATTENTION);
    assert!(!mark.bold);
    let rest = col_at(&grid, badge, "approvals waiting");
    for dx in [0, 10, 17] {
        let cell = grid.cell(rest + dx, badge);
        assert_eq!(cell.fg, ATTENTION, "col {dx}");
        assert!(cell.bold, "col {dx}");
    }
    quit(run);
}

#[test]
fn a_reviewer_failure_escalates_to_an_alert_panel() {
    let setup = shell_setup("cargo mutants -p hub");
    // An empty reviewer script: the reviewer's first request fails, which
    // reads as a reviewer failure. No rule asks first.
    write_json(&setup.workspace().join("empty.json"), &json!({"steps": []}));
    configure(
        &setup,
        json!({"reviewer": {"model": "scripted/empty.json"}}),
    );
    let mut run = terminal(&setup, &TRUECOLOUR);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"run it\r");
    let grid = run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("Approval 1 of 1")
    });
    let header = row_with(&grid, "Approval 1 of 1");
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, header).bg == ALERT_BG)
        .expect("the tinted run");
    // Every panel row takes the alert tint with an error stripe: ten
    // text rows from the header to the bottom edge.
    let bottom = (header..48)
        .find(|y| {
            grid.rows[usize::from(*y)]
                .chars()
                .take(usize::from(end) + 1)
                .all(|cell| cell == '▀')
        })
        .expect("the bottom edge");
    assert_eq!(bottom - header, 10, "ten text rows to the edge");
    for y in header..bottom {
        for x in 0..end + 1 {
            assert_eq!(grid.cell(x, y).bg, ALERT_BG, "({x}, {y})");
        }
        assert_eq!(grid.cell(0, y).symbol.as_str(), "▌", "row {y}");
        assert_eq!(grid.cell(0, y).fg, ERROR, "row {y}");
    }
    let facts = row_with(&grid, "the reviewer failed");
    assert!(
        grid.rows[usize::from(facts)]
            .trim_end()
            .ends_with("the reviewer failed"),
        "{}",
        grid.contents
    );
    // Esc puts it aside; quitting then lands on the primary screen.
    run.write(b"\x1b[27u");
    run.wait_screen("the badge", |grid| {
        grid.contents.contains("approvals waiting")
    });
    quit(run);
}

#[test]
fn an_escalation_takes_palette_52_at_256_colours() {
    let setup = shell_setup("cargo mutants -p hub");
    write_json(&setup.workspace().join("empty.json"), &json!({"steps": []}));
    configure(
        &setup,
        json!({"reviewer": {"model": "scripted/empty.json"}}),
    );
    let mut run = terminal(&setup, &[("TERM_PROGRAM", "ghostty")]);
    run.wait_screen("the first frame", |grid| grid.contents.contains("›"));
    run.ready();
    run.write(b"run it\r");
    let grid = run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("Approval 1 of 1")
    });
    // The alert tint's 256-colour entry, on every cell of the header row.
    let header = row_with(&grid, "Approval 1 of 1");
    let end = (0..160)
        .rev()
        .find(|x| grid.cell(*x, header).bg == Colour::Indexed(52))
        .expect("the tinted run");
    for y in [header] {
        for x in 0..end + 1 {
            assert_eq!(grid.cell(x, y).bg, Colour::Indexed(52), "({x}, {y})");
        }
    }
    run.write(b"\x1b[27u");
    run.wait_screen("the badge", |grid| {
        grid.contents.contains("approvals waiting")
    });
    quit(run);
}
