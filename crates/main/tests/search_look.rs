//! Binary-level tests of the search look (`docs/tui.md`, "Search" and
//! "Selection and copy"): the real binary on a 160x48 pty in truecolour,
//! with the scripted provider answering, draws the search bar as a
//! `surface` slab with half-block edges, the selection on the `selection`
//! tint, and "Copied" in `info` below the search box when it is open.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use serde_json::json;
use support::pty::{Colour, Grid, Run};
use support::{Setup, TRUECOLOUR, write_json};

/// `surface` in the dark theme.
const SURFACE: Colour = Colour::Rgb(26, 26, 34);
/// `attention` in the dark theme.
const ATTENTION: Colour = Colour::Rgb(255, 159, 67);
/// `info` in the dark theme.
const INFO: Colour = Colour::Rgb(125, 211, 252);
/// `match` in the dark theme.
const MATCH: Colour = Colour::Rgb(90, 74, 26);
/// `match_current` in the dark theme.
const CURRENT: Colour = Colour::Rgb(255, 159, 67);
/// `selection` in the dark theme.
const SELECTION: Colour = Colour::Rgb(38, 79, 120);
/// The match text colours.
const WHITE: Colour = Colour::Rgb(255, 255, 255);
const BLACK: Colour = Colour::Rgb(0, 0, 0);

/// A setup whose scripted model answers `reply` to the first prompt.
fn setup_with_reply(reply: &str) -> Setup {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [{"text": reply}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    setup
}

/// Spawns `fiber` on a 160 by 48 terminal, sends `prompt`, and waits for
/// the turn to finish.
fn turned(setup: &Setup, prompt: &str) -> Run {
    let mut run = Run::spawn(setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let from = run.output().len();
    run.write(format!("{prompt}\r").as_bytes());
    run.turn_finished(from);
    run
}

/// Whether the raw output holds a reverse-video SGR parameter: a `7`
/// between `;` (or `[`) and `;` (or `m`).
fn has_reverse(output: &[u8]) -> bool {
    let mut rest = output;
    while let Some(start) = rest.iter().position(|byte| *byte == 0x1b) {
        rest = &rest[start..];
        if rest.len() < 3 || rest[1] != b'[' {
            rest = &rest[1..];
            continue;
        }
        let Some(end) = rest.iter().position(|byte| *byte == b'm') else {
            break;
        };
        let params = &rest[2..end];
        if params
            .split(|byte| *byte == b';')
            .any(|param| param == b"7")
        {
            return true;
        }
        rest = &rest[end + 1..];
    }
    false
}

/// The first row containing `needle`, or a panic naming it.
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

/// The slab's content row: the row holding the count with the search
/// glyph, once every property below holds, so a partial redraw cannot
/// satisfy the wait. Returns the content row and the slab's columns.
fn slab_cells(grid: &Grid) -> Option<(u16, usize, usize)> {
    let y = row_with_or(&grid.rows, "1 of 5")?;
    let text: String = (0..160).map(|x| grid.cell(x, y).symbol.clone()).collect();
    let query_at = text.find("⌕ lock")?;
    if query_at > 40 {
        return None;
    }
    let run: Vec<usize> = (0..160)
        .filter(|x| grid.cell(*x as u16, y).bg == SURFACE)
        .collect();
    let first = *run.first()?;
    let last = *run.last()?;
    if last + 1 - first != 40 {
        return None;
    }
    if !(first..=last).all(|x| grid.cell(x as u16, y).bg == SURFACE) {
        return None;
    }
    // The content row reads " ⌕ lock█" then the count with one trailing
    // blank, right-aligned in its 40 columns.
    let content: String = (first..=last)
        .map(|x| grid.cell(x as u16, y).symbol.clone())
        .collect();
    if !content.starts_with(" ⌕ lock█") {
        return None;
    }
    if !content.ends_with("1 of 5 ") {
        return None;
    }
    // " ⌕ " in `attention`, the query bold, the █ and the count dim.
    let at = first + content.find('⌕').map(|byte| content[..byte].chars().count())?;
    if grid.cell(at as u16, y).fg != ATTENTION {
        return None;
    }
    let query = first + 3;
    if !(query..query + 4).all(|x| grid.cell(x as u16, y).bold) {
        return None;
    }
    let block = query + 4;
    if grid.cell(block as u16, y).symbol != "█"
        || !grid.cell(block as u16, y).dim
    {
        return None;
    }
    let count_at = last - 6;
    if !(count_at..=last - 1).all(|x| grid.cell(x as u16, y).dim) {
        return None;
    }
    // Five rows: the ▄ edge above, one blank row, the content row, one
    // blank row, the ▀ edge below.
    if y < 2 || y + 2 >= 48 {
        return None;
    }
    for (row, want) in [(y - 2, "▄"), (y + 2, "▀")] {
        if !(first..=last).all(|x| {
            let cell = grid.cell(x as u16, row);
            cell.symbol == want && cell.fg == SURFACE && cell.bg == Colour::Default
        }) {
            return None;
        }
    }
    for row in [y - 1, y + 1] {
        if !(first..=last).all(|x| {
            let cell = grid.cell(x as u16, row);
            cell.symbol == " " && cell.bg == SURFACE
        }) {
            return None;
        }
    }
    // The slab ends one column left of the scroll bar's column: the
    // cell past its right edge keeps the default background.
    let past = grid.cell((last + 1) as u16, y);
    if past.symbol != " " || past.bg != Colour::Default {
        return None;
    }
    Some((y, first, last))
}

/// The first row containing `needle`, if any.
fn row_with_or(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .map(|y| y as u16)
}

/// A press at `(from, y)`, a drag to `(to, y)` and its release, in one
/// write so the reports stay ordered.
fn drag(run: &mut Run, y: u16, from: u16, to: u16) {
    run.write(
        format!(
            "\x1b[<0;{};{}M\x1b[<32;{};{}M\x1b[<3;{};{}m",
            from + 1,
            y + 1,
            to + 1,
            y + 1,
            to + 1,
            y + 1,
        )
        .as_bytes(),
    );
}

/// Every cell with the selection background.
fn selected_cells(grid: &Grid) -> Vec<(u16, u16)> {
    let mut out = Vec::new();
    for y in 0..48u16 {
        for x in 0..160u16 {
            if grid.cell(x, y).bg == SELECTION {
                out.push((x, y));
            }
        }
    }
    out
}

#[test]
fn the_search_bar_matches_the_doc() {
    let setup = setup_with_reply("lock lock lock lock lock");
    let mut run = turned(&setup, "go");
    run.write(b"\x06lock");
    // The bar once the scan reports all five matches: every property
    // the asserts read is checked here, so a partial redraw cannot
    // satisfy the wait.
    let grid = run.wait_screen("the search bar", |grid| slab_cells(grid).is_some());
    let (y, first, last) = slab_cells(&grid).expect("the search bar");
    assert_eq!(last + 1 - first, 40, "the bar is 40 columns wide");
    let (first, last) = (first as u16, last as u16);
    // The content row sits on the `surface` tint across its width.
    for x in first..=last {
        assert_eq!(grid.cell(x, y).bg, SURFACE, "({x}, {y})");
    }
    // " ⌕ " in `attention`, the query bold, the █ and the count dim.
    let query = first + 3;
    assert_eq!(grid.cell(query - 1, y).fg, ATTENTION);
    for x in query..query + 4 {
        assert!(grid.cell(x, y).bold, "query ({x}, {y})");
    }
    assert_eq!(grid.cell(query + 4, y).symbol, "█");
    assert!(grid.cell(query + 4, y).dim);
    let count = col_at(&grid, y, "1 of 5");
    for dx in 0..5 {
        assert!(grid.cell(count + dx, y).dim, "count {dx}");
    }
    // The count ends two columns before the slab's right edge.
    assert_eq!(count + 5, last - 1, "the count's right edge");
    // The slab ends one column left of the scroll bar's column.
    let past = grid.cell(last + 1, y);
    assert_eq!(past.symbol, " ");
    assert_eq!(past.bg, Colour::Default);
    // Half-block edges over the default background, above and below,
    // with one blank row inside each.
    for (row, want) in [(y - 2, "▄"), (y + 2, "▀")] {
        for x in first..=last {
            let cell = grid.cell(x, row);
            assert_eq!(cell.symbol, want, "({x}, {row})");
            assert_eq!(cell.fg, SURFACE, "({x}, {row})");
            assert_eq!(cell.bg, Colour::Default, "({x}, {row})");
        }
    }
    for row in [y - 1, y + 1] {
        for x in first..=last {
            let cell = grid.cell(x, row);
            assert_eq!(cell.symbol, " ", "({x}, {row})");
            assert_eq!(cell.bg, SURFACE, "({x}, {row})");
        }
    }
    // Every match on the `match` tint with white text, the current one
    // on `match_current` with black text.
    let mut plain = 0;
    let mut current = 0;
    for row in 0..48u16 {
        for x in 0..160u16 {
            let cell = grid.cell(x, row);
            if cell.bg == MATCH {
                assert_eq!(cell.fg, WHITE, "match ({x}, {row})");
                plain += 1;
            } else if cell.bg == CURRENT {
                assert_eq!(cell.fg, BLACK, "current ({x}, {row})");
                current += 1;
            }
        }
    }
    assert!(plain > 0, "a match is marked");
    assert!(current > 0, "the current match is brighter");
    // No cell takes the reverse attribute.
    assert!(
        !has_reverse(&run.output()),
        "no cell is reversed"
    );
}

#[test]
fn a_drag_selects_and_copies() {
    let setup = setup_with_reply("drag across this line to copy it");
    let mut run = turned(&setup, "go");
    let grid = run.wait_screen("the reply", |grid| {
        grid.contents.contains("drag across")
    });
    let y = row_with(&grid, "drag across");
    let from = col_at(&grid, y, "across");
    let to = from + 5;
    let before = run.output().len();
    drag(&mut run, y, from, to);
    // The selection once the release copies: every dragged cell on the
    // `selection` tint, and "Copied" at the top-right.
    let grid = run.wait_screen("the selection", |grid| {
        !selected_cells(grid).is_empty() && grid.contents.contains("Copied")
    });
    let cells = selected_cells(&grid);
    assert!(!cells.is_empty(), "the drag selects");
    for (x, row) in &cells {
        assert_eq!(*row, y, "the selection stays on the dragged row");
        assert!(
            (from..=to).contains(x),
            "the selection stays in the dragged span ({x}, {row})"
        );
    }
    for x in from..=to {
        assert!(
            cells.contains(&(x, y)),
            "the dragged cell ({x}, {y}) is selected"
        );
    }
    assert!(
        !has_reverse(&run.output()[before..]),
        "no cell is reversed"
    );
    // On release "Copied" shows in `info` at the top-right, row 0 with
    // the bar closed.
    let copied = row_with(&grid, "Copied");
    assert_eq!(copied, 0, "Copied sits on the top row");
    let at = col_at(&grid, copied, "Copied");
    for dx in 0..6 {
        assert_eq!(grid.cell(at + dx, copied).fg, INFO, "Copied {dx}");
    }
}

#[test]
fn copied_sits_below_the_search_box() {
    let setup = setup_with_reply("drag across this line to copy it");
    let mut run = turned(&setup, "go");
    run.write(b"\x06lock");
    // The bar opens first: its content row anchors the slab.
    let grid = run.wait_screen("the search bar", |grid| {
        grid.contents.contains('⌕')
    });
    let bar = row_with(&grid, "⌕");
    let grid = run.wait_screen("the reply under the bar", |grid| {
        grid.contents.contains("drag across")
    });
    let y = row_with(&grid, "drag across");
    assert!(y > bar + 2, "the drag stays below the slab");
    let from = col_at(&grid, y, "across");
    drag(&mut run, y, from, from + 5);
    let grid = run.wait_screen("Copied under the bar", |grid| {
        grid.contents.contains("Copied")
    });
    let copied = row_with(&grid, "Copied");
    // "Copied" sits on the row under the slab's lower edge, never
    // inside it.
    assert_eq!(copied, bar + 3, "Copied sits below the slab");
    let at = col_at(&grid, copied, "Copied");
    for dx in 0..6 {
        assert_eq!(grid.cell(at + dx, copied).fg, INFO, "Copied {dx}");
    }
}
