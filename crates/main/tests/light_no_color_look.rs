//! Binary-level tests of the light theme at 256 colours and the
//! colourless boxes (`docs/tui.md`, "Look", "Themes"): the real binary
//! under a pseudo-terminal at 160x48, asserting the `vt100` grid the
//! shared driver rebuilds. Local helpers use the shared driver
//! read-only; no new binary switches, provider dependencies or support
//! edits.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use support::pty::{Colour, Grid, HOME_TITLE, Run};
use support::{Setup, TRUECOLOUR};

/// Whether `grid` shows `needle` anywhere.
fn shows(grid: &Grid, needle: &str) -> bool {
    (0..48).any(|y| {
        let row: String = (0..160).map(|x| grid.cell(x, y).symbol).collect();
        row.contains(needle)
    })
}

/// The first position of `needle` on `grid`.
fn find(grid: &Grid, needle: &str) -> (u16, u16) {
    for y in 0..48 {
        let row: String = (0..160).map(|x| grid.cell(x, y).symbol).collect();
        if let Some(x) = row.find(needle) {
            return (u16::try_from(x).unwrap(), y);
        }
    }
    panic!("{needle:?} is not on the screen");
}

/// Spawns 160x48 with the scripted provider and the light theme, runs
/// one turn to `finished` and returns the setup, the run and the drawn
/// grid. With no `COLORTERM` the terminal has 256 colours; `ghostty`
/// draws the stripes, and is not a truecolour override.
#[track_caller]
fn light_run() -> (Setup, Run, Grid) {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Lig", "ht."]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "tui": {"theme": "light",
            "rail": {"shown": false}}, "hub": {"idle_exit_ms": 200}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], &[("TERM_PROGRAM", "ghostty")]);
    // The end of the first frame proves the input reader runs before
    // the prompt goes out.
    run.ready();
    let from = run.output().len();
    run.write(b"show light\r");
    // The reply stays drawn, so the grid check before the finished
    // title is order-free.
    let screen = run.wait_screen("the light turn", light_drawn);
    run.turn_finished(from);
    assert!(light_drawn(&screen), "the finished light turn");
    (setup, run, screen)
}

/// Whether the grid shows a finished light turn with everything the
/// light assertions read: the `completed` status, the prompt and reply
/// surfaced on their tints off the white page, the full white page, the
/// turn's edge run and the striped input box. A partial redraw can't
/// satisfy it: every asserted property is checked.
fn light_drawn(grid: &Grid) -> bool {
    if !grid.contents.contains("completed") {
        return false;
    }
    if !grid.contents.contains("show light") {
        return false;
    }
    // The reply surfaced: indexed colours, off the white page.
    let Some((x, y)) = (0..48).find_map(|y| {
        (0..157).find_map(|x| {
            let word: String = (0..3).map(|dx| grid.cell(x + dx, y).symbol).collect();
            (word == "Lig").then_some((x, y))
        })
    }) else {
        return false;
    };
    if !(0..3).all(|dx| {
        let cell = grid.cell(x + dx, y);
        matches!(cell.fg, Colour::Indexed(_))
            && matches!(cell.bg, Colour::Indexed(_))
            && cell.bg != Colour::Indexed(231)
            && !cell.dim
    }) {
        return false;
    }
    // The white page: every blank cell above the first content row is
    // the page, with a blank page row above the turn. Past the grip,
    // which takes column 0.
    let Some(first) = (1..45).find(|y| (1..126).any(|x| grid.cell(x, *y).symbol.as_str() != " "))
    else {
        return false;
    };
    if first <= 1 {
        return false;
    }
    for y in 1..first {
        for x in 1..126 {
            let cell = grid.cell(x, y);
            if cell.symbol.as_str() != " " || cell.bg != Colour::Indexed(231) {
                return false;
            }
        }
    }
    // The person's prompt bubble sits on its own indexed tint, off the
    // page too.
    let mut prompt_at: Option<(u16, u16)> = None;
    for py in 0..48 {
        let row: String = (0..160).map(|x| grid.cell(x, py).symbol).collect();
        if let Some(px) = row.find("show light") {
            prompt_at = u16::try_from(px).ok().map(|px| (px, py));
            break;
        }
    }
    let Some((px, py)) = prompt_at else {
        return false;
    };
    let prompt = grid.cell(px, py);
    if !(matches!(prompt.bg, Colour::Indexed(_)) && prompt.bg != Colour::Indexed(231)) {
        return false;
    }
    // The turn card's ▄ edge between the page and the reply: a full
    // surfaced run, not a single lucky cell.
    if (first..y)
        .find(|row| {
            let run = (1..126)
                .filter(|col| {
                    let cell = grid.cell(*col, *row);
                    cell.symbol == "▄" && matches!(cell.fg, Colour::Indexed(_))
                })
                .count();
            run >= 100
        })
        .is_none()
    {
        return false;
    }
    // The input box's striped surface: every asserted cell indexed,
    // with the stripe in its surface.
    for x in 1..126 {
        if !matches!(grid.cell(x, 46).bg, Colour::Indexed(_)) {
            return false;
        }
    }
    let stripe = grid.cell(1, 46);
    stripe.symbol.as_str() == "▌" && matches!(stripe.fg, Colour::Indexed(_))
}

#[test]
fn light_at_256_paints_its_own_white_page() {
    let (_setup, mut run, screen) = light_run();
    // The prompt and the reply drew.
    assert!(screen.contents.contains("show light"));
    assert!(screen.contents.contains("Lig"));
    // Every blank cell above the first content row is the page: the
    // colour cube's white, 231, which the grey ramp lacks.
    let first = (1..45)
        .find(|y| (1..126).any(|x| screen.cell(x, *y).symbol.as_str() != " "))
        .expect("a content row above the input box");
    assert!(first > 1, "a blank page row above the turn");
    for y in 1..first {
        for x in 1..126 {
            let cell = screen.cell(x, y);
            assert_eq!(cell.symbol.as_str(), " ", "page cell ({x}, {y})");
            assert_eq!(cell.bg, Colour::Indexed(231), "page cell ({x}, {y})");
        }
    }
    // The reply sits on its turn card: indexed text on an indexed tint
    // that stays visible off the page.
    let (x, y) = find(&screen, "Lig");
    for dx in 0..3 {
        let cell = screen.cell(x + dx, y);
        assert!(matches!(cell.fg, Colour::Indexed(_)), "reply fg ({x}, {y})");
        assert!(matches!(cell.bg, Colour::Indexed(_)), "reply bg ({x}, {y})");
        assert_ne!(cell.bg, Colour::Indexed(231), "reply bg ({x}, {y})");
        assert!(!cell.dim);
    }
    // The person's prompt bubble sits on its own indexed tint, off the
    // page too.
    let (px, py) = find(&screen, "show light");
    let prompt = screen.cell(px, py);
    assert!(matches!(prompt.bg, Colour::Indexed(_)), "prompt bg");
    assert_ne!(prompt.bg, Colour::Indexed(231), "prompt bg");
    // The turn card's ▄ edge between the page and the reply, and the
    // input box's striped surface: full surfaced runs, not single cells.
    let edge = (first..y).find(|row| {
        let run = (1..126)
            .filter(|col| {
                let cell = screen.cell(*col, *row);
                cell.symbol == "▄" && matches!(cell.fg, Colour::Indexed(_))
            })
            .count();
        run >= 100
    });
    assert!(edge.is_some(), "a turn edge run above the reply");
    for x in 1..126 {
        let cell = screen.cell(x, 46);
        assert!(matches!(cell.bg, Colour::Indexed(_)), "box cell ({x}, 46)");
    }
    let stripe = screen.cell(1, 46);
    assert_eq!(stripe.symbol.as_str(), "▌");
    assert!(matches!(stripe.fg, Colour::Indexed(_)));
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// Spawns 160x48 with the scripted provider and `NO_COLOR` set over a
/// truecolour terminal, runs one turn to `finished` and returns the
/// setup and the run.
#[track_caller]
fn no_color_run() -> (Setup, Run) {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Hel", "lo."]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}}),
    );
    let env: Vec<(&str, &str)> = [("NO_COLOR", "1")].into_iter().chain(TRUECOLOUR).collect();
    let mut run = Run::spawn(&setup, 160, 48, &[], &env);
    // The end of the first frame proves the input reader runs before
    // the prompt goes out.
    run.ready();
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |screen| shows(screen, "Hel"));
    run.turn_finished(from);
    (setup, run)
}

/// Whether the grid shows a rail card's box with everything the rail
/// assertions read: the rail card's dim box on the default colours
/// with its sides, the finished reply readable in the turn's box, the
/// turn's edge run, the prompt bubble's blank edges with no box sides,
/// and the input box's border. A partial redraw can't satisfy it: every
/// asserted property is checked.
fn rail_boxed(grid: &Grid) -> bool {
    // The finished reply still drawn, readable on the defaults.
    let Some((rx, ry)) = find_cell(grid, "Hel") else {
        return false;
    };
    for dx in 0..3 {
        let cell = grid.cell(rx + dx, ry);
        if !(cell.fg == Colour::Default && cell.bg == Colour::Default && !cell.dim) {
            return false;
        }
    }
    // The rail card's box: dim corners and fills on the defaults.
    let mut railed = false;
    for y in 0..48 {
        if grid.cell(0, y).symbol != "┌" {
            continue;
        }
        let Some(end) = (1..30).find(|&x| grid.cell(x, y).symbol == "┐") else {
            continue;
        };
        if !(1..end).all(|x| grid.cell(x, y).symbol == "─") {
            continue;
        }
        if y + 1 >= 48 {
            continue;
        }
        if !(0..=end).all(|x| {
            let (top, side) = (grid.cell(x, y), grid.cell(x, y + 1));
            top.fg == Colour::Default
                && top.bg == Colour::Default
                && top.dim
                && (x == 0
                    || x == end
                    || side.symbol != "│"
                    || (side.fg == Colour::Default && side.bg == Colour::Default && side.dim))
        }) {
            continue;
        }
        // The rail's sides close the card's first body row.
        let left = grid.cell(0, y + 1);
        let right = grid.cell(end, y + 1);
        if !(left.symbol == "│"
            && right.symbol == "│"
            && left.fg == Colour::Default
            && left.bg == Colour::Default
            && left.dim
            && right.fg == Colour::Default
            && right.bg == Colour::Default
            && right.dim)
        {
            continue;
        }
        railed = true;
        break;
    }
    if !railed {
        return false;
    }
    // Select the same nearest edge as the assertion below. Once a row
    // matches the edge selector, an incomplete edge fails rather than
    // falling through to an older row.
    let Some(top) = (0..ry).rev().find(|top| {
        (25..125).any(|x| {
            let cell = grid.cell(x, *top);
            cell.symbol == "┌" && cell.dim && cell.fg == Colour::Default
        })
    }) else {
        return false;
    };
    let Some(width) = (25..126).find(|x| grid.cell(*x, top).symbol == "┐") else {
        return false;
    };
    if width - 25 <= 40
        || !(25..=width).all(|x| {
            let cell = grid.cell(x, top);
            cell.bg == Colour::Default && cell.dim
        })
    {
        return false;
    }
    // Select the first prompt occurrence, as the assertion below does.
    // Validate it in place: an incomplete bubble cannot be skipped in
    // favour of a later occurrence.
    let Some((px, py)) = (0..48).find_map(|py| {
        let row: String = (30..160).map(|x| grid.cell(x, py).symbol).collect();
        row.find("again")
            .and_then(|off| u16::try_from(30 + off).ok())
            .map(|px| (px, py))
    }) else {
        return false;
    };
    if px <= 30 || py + 1 >= 48 {
        return false;
    }
    let end: u16 = (px..160)
        .find(|x| grid.cell(*x, py).symbol.as_str() == " ")
        .unwrap_or(160);
    if (px.saturating_sub(2)..end.min(160).saturating_add(1))
        .any(|x| grid.cell(x, py).symbol.as_str() == "│")
    {
        return false;
    }
    for y in [py.saturating_sub(1), py + 1] {
        for x in px..end.min(160) {
            if grid.cell(x, y).symbol.as_str() != " " {
                return false;
            }
        }
    }
    // The input box's border: ┌─┐ above, │ on each side and └─┘ three
    // rows below past its rule and status row, dim on the defaults,
    // with the prompt one column in.
    let Some((left, row)) = input_side_on(grid) else {
        return false;
    };
    if row == 0 || row + 3 >= 48 {
        return false;
    }
    let Some(right) = (left + 1..160).find(|&x| grid.cell(x, row - 1).symbol == "┐") else {
        return false;
    };
    for x in left..=right {
        for (y, corner, fill) in [(row - 1, "┌", "─"), (row + 3, "└", "─")] {
            let want = if x == left {
                corner
            } else if x == right {
                if y == row - 1 { "┐" } else { "┘" }
            } else {
                fill
            };
            let cell = grid.cell(x, y);
            if cell.symbol.as_str() != want
                || (cell.fg, cell.bg) != (Colour::Default, Colour::Default)
                || !cell.dim
            {
                return false;
            }
        }
    }
    grid.cell(left, row).symbol.as_str() == "│"
        && grid.cell(left + 1, row).symbol.as_str() == "›"
        && grid.cell(right, row).symbol.as_str() == "│"
}

/// The first position of `needle` on `grid`, when drawn.
fn find_cell(grid: &Grid, needle: &str) -> Option<(u16, u16)> {
    for y in 0..48 {
        let row: String = (0..160).map(|x| grid.cell(x, y).symbol).collect();
        if let Some(x) = row.find(needle) {
            return u16::try_from(x).ok().map(|x| (x, y));
        }
    }
    None
}

/// The input box's side cell, when drawn: the bottom-most `›` past a `│`.
fn input_side_on(screen: &Grid) -> Option<(u16, u16)> {
    for y in (0..48).rev() {
        for x in 1..160 {
            if screen.cell(x, y).symbol.as_str() != "›" {
                continue;
            }
            if screen.cell(x - 1, y).symbol.as_str() == "│" {
                return Some((x - 1, y));
            }
        }
    }
    None
}

fn input_side(screen: &Grid) -> (u16, u16) {
    for y in (0..48).rev() {
        for x in 1..160 {
            if screen.cell(x, y).symbol.as_str() != "›" {
                continue;
            }
            if screen.cell(x - 1, y).symbol.as_str() == "│" {
                return (x - 1, y);
            }
        }
    }
    panic!("no boxed input row in the screen");
}

#[test]
fn no_color_boxes_every_surface_on_the_final_frame() {
    let (_setup, mut run) = no_color_run();
    // Ctrl+N reaches home, and a second scripted session brings the
    // rail; the test waits for that rail.
    let home_from = run.output().len();
    run.write(b"\x0e");
    run.wait_screen("home", |screen| screen.cell(150, 20).bg == Colour::Default);
    run.wait_bytes(home_from, HOME_TITLE, "the home title");
    let from = run.output().len();
    run.write(b"again\r");
    // The reply stays drawn, so the grid check before the finished
    // title is order-free; the rail holds the reply card.
    run.wait_screen("the reply", |screen| shows(screen, "Hel"));
    run.turn_finished(from);
    let screen = run.wait_screen("the rail card's box", rail_boxed);
    assert!(rail_boxed(&screen), "incomplete rail card box");
    // The turn's card is a thin dim box on the default colours, with the
    // reply readable inside: corners and fills above and below it.
    let (rx, ry) = find(&screen, "Hel");
    for dx in 0..3 {
        let cell = screen.cell(rx + dx, ry);
        assert_eq!(cell.fg, Colour::Default, "reply fg");
        assert!(!cell.dim, "reply dim");
        assert_eq!(cell.bg, Colour::Default, "reply bg");
    }
    let edge = (0..ry).rev().find(|y| {
        (25..125).any(|x| {
            screen.cell(x, *y).symbol == "┌"
                && screen.cell(x, *y).dim
                && screen.cell(x, *y).fg == Colour::Default
        })
    });
    let top = edge.expect("a turn box top edge above the reply");
    let width = (25..126)
        .find(|x| screen.cell(*x, top).symbol == "┐")
        .expect("the turn box corner");
    assert!(width - 25 > 40, "a card-wide box, not a fragment");
    for x in 25..=width {
        let cell = screen.cell(x, top);
        assert_eq!(cell.bg, Colour::Default, "turn edge ({x}, {top})");
        assert!(cell.dim, "turn edge ({x}, {top})");
    }
    // The second prompt bubble keeps blank edges and no box sides: its
    // row holds the prompt between blanks, with blank rows above and
    // below its span. The rail card's title names the session too, so
    // the bubble is the occurrence past the rail.
    let (px, py) = (0..48)
        .find_map(|y| {
            let row: String = (30..160).map(|x| screen.cell(x, y).symbol).collect();
            row.find("again")
                .map(|x| (30 + u16::try_from(x).unwrap(), y))
        })
        .expect("the prompt bubble");
    let end: u16 = (px..160)
        .find(|x| screen.cell(*x, py).symbol.as_str() == " ")
        .unwrap_or(160);
    assert!(px > 30, "the prompt sits right-aligned, {px}");
    for x in px.saturating_sub(2)..end.min(160).saturating_add(1) {
        assert_ne!(
            screen.cell(x, py).symbol.as_str(),
            "│",
            "prompt side ({x}, {py})"
        );
    }
    for y in [py.saturating_sub(1), py + 1] {
        for x in px..end.min(160) {
            assert_eq!(
                screen.cell(x, y).symbol.as_str(),
                " ",
                "prompt edge ({x}, {y})"
            );
        }
    }
    // The input box's border on the final frame: ┌─┐ above, │ on each
    // side and └─┘ three rows below past its rule and status row, dim,
    // with the prompt one column in.
    let (left, row) = input_side(&screen);
    let right = (left + 1..160)
        .find(|&x| screen.cell(x, row - 1).symbol == "┐")
        .expect("the input box corner");
    for x in left..=right {
        for (y, corner, fill) in [(row - 1, "┌", "─"), (row + 3, "└", "─")] {
            let want = if x == left {
                corner
            } else if x == right {
                if y == row - 1 { "┐" } else { "┘" }
            } else {
                fill
            };
            let cell = screen.cell(x, y);
            assert_eq!(cell.symbol.as_str(), want, "input edge ({x}, {y})");
            assert_eq!(
                (cell.fg, cell.bg),
                (Colour::Default, Colour::Default),
                "({x}, {y})"
            );
            assert!(cell.dim, "input edge ({x}, {y})");
        }
    }
    assert_eq!(screen.cell(left, row).symbol.as_str(), "│");
    assert_eq!(screen.cell(left + 1, row).symbol.as_str(), "›");
    assert_eq!(screen.cell(right, row).symbol.as_str(), "│");
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
