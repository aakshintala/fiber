//! Binary-level tests of the search look (`docs/tui.md`, "Search" and
//! "Selection and copy") and of the swapped views' top row (`docs/tui.md",
//! "Swapped views"): the real binary on a 160x48 pty in truecolour,
//! with the scripted provider answering, draws the search bar as a
//! `surface` slab with half-block edges, the selection on the `selection`
//! tint, "Copied" in `info` below the search box when it is open, and
//! every swapped view's top row as its name bold, its command dim and
//! "Esc returns" dim right-aligned on the `surface` tint.

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
use support::hold::Hold;
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

/// Whether the raw output holds a reverse-video SGR parameter: an SGR
/// sequence (`ESC [`, only digits and `;`, then `m`) with a `7` among
/// its parameters. Cursor moves such as `ESC [7;62H` are not SGR and
/// never match: their final byte is not `m`.
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
        // Anything but digits and `;` is another sequence (a cursor
        // move, say): step one byte on, so a real SGR later on the same
        // stretch is still seen.
        if !params
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b';')
        {
            rest = &rest[1..];
            continue;
        }
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

/// The column where `needle` starts on `y`, if any, matching the cells
/// one by one, so a wide cell before it changes nothing.
fn col_at_or(grid: &Grid, y: u16, needle: &str) -> Option<u16> {
    let want: Vec<char> = needle.chars().collect();
    if want.is_empty() {
        return None;
    }
    let (width, _) = grid.size();
    for x in 0..width {
        let mut ok = true;
        for (dx, want) in want.iter().enumerate() {
            let xi = x.checked_add(u16::try_from(dx).ok()?)?;
            if xi >= width || grid.cell(xi, y).symbol != want.to_string() {
                ok = false;
                break;
            }
        }
        if ok {
            return Some(x);
        }
    }
    None
}

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    col_at_or(grid, y, needle).unwrap_or_else(|| panic!("{needle:?} on row {y}"))
}

/// The slab's content row: the row holding the count with the search
/// glyph, once every property below holds, so a partial redraw cannot
/// satisfy the wait. Returns the content row and the slab's columns.
fn slab_cells(grid: &Grid) -> Option<(u16, u16, u16)> {
    let y = row_with_or(&grid.rows, "1 of 5")?;
    let text: String = (0..160u16)
        .map(|x| grid.cell(x, y).symbol.clone())
        .collect();
    if !text.contains("⌕ lock") {
        return None;
    }
    // The slab's run: the contiguous `surface` run around the count,
    // so other `surface` cells on the row (a rail card, say) stay out.
    let count_first = u16::try_from(
        text.find("1 of 5")
            .map(|byte| text[..byte].chars().count())?,
    )
    .ok()?;
    let mut first = count_first;
    while first > 0 && grid.cell(first - 1, y).bg == SURFACE {
        first -= 1;
    }
    let mut last = count_first;
    while last + 1 < 160 && grid.cell(last + 1, y).bg == SURFACE {
        last += 1;
    }
    if last + 1 - first != 40 {
        return None;
    }
    if !(first..=last).all(|x| grid.cell(x, y).bg == SURFACE) {
        return None;
    }
    // The content row reads " ⌕ lock█" then the count with one trailing
    // blank, right-aligned in its 40 columns.
    let content: String = (first..=last)
        .map(|x| grid.cell(x, y).symbol.clone())
        .collect();
    if !content.starts_with(" ⌕ lock█") {
        return None;
    }
    if !content.ends_with("1 of 5 ") {
        return None;
    }
    // " ⌕ " in `attention`, the query bold, the █ in the text colour
    // with no dim, and the count dim.
    let at = first.checked_add(
        u16::try_from(
            content
                .find('⌕')
                .map(|byte| content[..byte].chars().count())?,
        )
        .ok()?,
    )?;
    if grid.cell(at, y).fg != ATTENTION {
        return None;
    }
    let query = first.checked_add(3)?;
    if !(query..query + 4).all(|x| grid.cell(x, y).bold) {
        return None;
    }
    let block = query.checked_add(4)?;
    if grid.cell(block, y).symbol != "█"
        || grid.cell(block, y).fg != Colour::Default
        || grid.cell(block, y).dim
    {
        return None;
    }
    let count_at = last.checked_sub(6)?;
    if !(count_at..=last - 1).all(|x| grid.cell(x, y).dim) {
        return None;
    }
    // Five rows: the ▄ edge above, one blank row, the content row, one
    // blank row, the ▀ edge below.
    if y < 2 || y + 2 >= 48 {
        return None;
    }
    for (row, want) in [(y - 2, "▄"), (y + 2, "▀")] {
        if !(first..=last).all(|x| {
            let cell = grid.cell(x, row);
            cell.symbol == want && cell.fg == SURFACE && cell.bg == Colour::Default
        }) {
            return None;
        }
    }
    for row in [y - 1, y + 1] {
        if !(first..=last).all(|x| {
            let cell = grid.cell(x, row);
            cell.symbol == " " && cell.bg == SURFACE
        }) {
            return None;
        }
    }
    // The slab ends one column left of the scroll bar's column: the
    // cell past its right edge keeps the default background.
    let past = grid.cell(last + 1, y);
    if past.symbol != " " || past.bg != Colour::Default {
        return None;
    }
    Some((y, first, last))
}

/// The first row containing `needle`, if any.
fn row_with_or(rows: &[String], needle: &str) -> Option<u16> {
    rows.iter()
        .position(|row| row.contains(needle))
        .and_then(|y| u16::try_from(y).ok())
}

/// "Copied" with its foreground: its column and row when its six
/// cells all carry `info`.
fn copied_at(grid: &Grid) -> Option<(u16, u16)> {
    let y = row_with_or(&grid.rows, "Copied")?;
    let at = col_at_or(grid, y, "Copied")?;
    for dx in 0..6u16 {
        let x = at.checked_add(dx)?;
        if x >= 160 || grid.cell(x, y).fg != INFO {
            return None;
        }
    }
    Some((at, y))
}

/// The finished drag-select: exactly the dragged cells on the
/// `selection` tint, and "Copied" in `info` on the top row. Every cell
/// property the asserts read is checked here, so a partial redraw
/// cannot satisfy the wait.
fn drag_selected(grid: &Grid) -> bool {
    let Some(y) = row_with_or(&grid.rows, "drag across") else {
        return false;
    };
    let Some(from) = col_at_or(grid, y, "across") else {
        return false;
    };
    let Some(to) = from.checked_add(5) else {
        return false;
    };
    if to >= 160 {
        return false;
    }
    // Exactly the dragged span, nowhere else.
    for row in 0..48u16 {
        for x in 0..160u16 {
            let selected = grid.cell(x, row).bg == SELECTION;
            if selected != (row == y && (from..=to).contains(&x)) {
                return false;
            }
        }
    }
    // "Copied" in `info` on the top row.
    copied_at(grid).is_some_and(|(_, row)| row == 0)
}

/// "Copied" below the search box: the bar's content row, with "Copied"
/// in `info` on the row under the slab's lower edge. Every cell property
/// the asserts read is checked here, so a partial redraw cannot satisfy
/// the wait.
fn copied_below(grid: &Grid) -> bool {
    let Some(bar) = row_with_or(&grid.rows, "⌕") else {
        return false;
    };
    let Some(copied) = bar.checked_add(3) else {
        return false;
    };
    if copied >= 48 {
        return false;
    }
    copied_at(grid).is_some_and(|(_, row)| row == copied)
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
    // The content row sits on the `surface` tint across its width.
    for x in first..=last {
        assert_eq!(grid.cell(x, y).bg, SURFACE, "({x}, {y})");
    }
    // " ⌕ " in `attention`, the query bold, the █ in the text colour
    // with no dim, and the count dim.
    let query = first + 3;
    assert_eq!(grid.cell(query - 1, y).fg, ATTENTION);
    for x in query..query + 4 {
        assert!(grid.cell(x, y).bold, "query ({x}, {y})");
    }
    assert_eq!(grid.cell(query + 4, y).symbol, "█");
    assert_eq!(grid.cell(query + 4, y).fg, Colour::Default);
    assert!(!grid.cell(query + 4, y).dim);
    let count = col_at(&grid, y, "1 of 5");
    for dx in 0..6 {
        assert!(grid.cell(count + dx, y).dim, "count {dx}");
    }
    // The count ends one blank before the slab's right edge: its last
    // cell sits two columns before the slab's right edge.
    assert_eq!(count + 6, last, "the count's right edge");
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
    assert!(!has_reverse(&run.output()), "no cell is reversed");
}

#[test]
fn a_drag_selects_and_copies() {
    let setup = setup_with_reply("drag across this line to copy it");
    let mut run = turned(&setup, "go");
    let grid = run.wait_screen("the reply", |grid| grid.contents.contains("drag across"));
    let y = row_with(&grid, "drag across");
    let from = col_at(&grid, y, "across");
    let to = from + 5;
    let before = run.output().len();
    drag(&mut run, y, from, to);
    // The selection once the release copies: every dragged cell on the
    // `selection` tint, and "Copied" at the top-right.
    let grid = run.wait_screen("the selection", drag_selected);
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
    assert!(!has_reverse(&run.output()[before..]), "no cell is reversed");
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
    let grid = run.wait_screen("the search bar", |grid| grid.contents.contains('⌕'));
    let bar = row_with(&grid, "⌕");
    let grid = run.wait_screen("the reply under the bar", |grid| {
        grid.contents.contains("drag across")
    });
    let y = row_with(&grid, "drag across");
    assert!(y > bar + 2, "the drag stays below the slab");
    let from = col_at(&grid, y, "across");
    drag(&mut run, y, from, from + 5);
    let grid = run.wait_screen("Copied under the bar", copied_below);
    let copied = row_with(&grid, "Copied");
    // "Copied" sits on the row under the slab's lower edge, never
    // inside it.
    assert_eq!(copied, bar + 3, "Copied sits below the slab");
    let at = col_at(&grid, copied, "Copied");
    for dx in 0..6 {
        assert_eq!(grid.cell(at + dx, copied).fg, INFO, "Copied {dx}");
    }
}

/// A setup whose scripted model reads a large file and then answers
/// `REPLY`: the read gives the `/context` view tool results to draw,
/// and both steps report usage under the 400k handoff point, so no
/// handoff clears them before the views open.
const REPLY: &str = "The quay ledger balances.";

fn setup_with_read() -> Setup {
    let setup = Setup::new();
    // One line is enough: the test only checks that the read appears
    // among the largest tool results.
    fs::write(
        setup.workspace().join("a.txt"),
        "ledger line with padding to fill bytes\n",
    )
    .unwrap();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "read", "arguments": {"path": "a.txt"}}],
             "usage": {"input": 200000, "output": 10}},
            {"text": [REPLY], "usage": {"input": 300000, "output": 5}},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "handoff": {"tokens": 400000},
            "budget": {"usd": 20}, "hub": {"idle_exit_ms": 1000}}),
    );
    setup
}

/// Clicks the cell at `(x, y)`: an SGR press then its release on the
/// same cell.
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// The top row's `surface` run around the view's title: row 0 holds
/// `" {name}"`, so the run is found from the title and widened over
/// the contiguous `surface` cells. The search walks row 0 only, so a
/// `surface` card elsewhere on the screen stays out.
fn top_run(grid: &Grid, name: &str) -> (u16, u16) {
    let needle = format!(" {name}");
    let first = col_at_or(grid, 0, &needle)
        .unwrap_or_else(|| panic!("the {name} top row in\n{}", grid.contents));
    let mut last = first;
    while last + 1 < 160 && grid.cell(last + 1, 0).bg == SURFACE {
        last += 1;
    }
    (first, last)
}

/// Asserts row 0 reads `" {name}  {command}"` then `"Esc returns"`
/// ending at the run's last cell: the whole run on `surface`, the name
/// bold, the command and `"Esc returns"` dim, and no ✕ anywhere in
/// it. Returns the run for the click that closes the view.
fn assert_top_row(grid: &Grid, name: &str, command: &str) -> (u16, u16) {
    let (first, last) = top_run(grid, name);
    assert!(last > first + 30, "the top row spans its row");
    for x in first..=last {
        assert_eq!(grid.cell(x, 0).bg, SURFACE, "top row ({x}, 0)");
    }
    let text: String = (first..=last)
        .map(|x| grid.cell(x, 0).symbol.clone())
        .collect();
    let head = format!(" {name}  {command}");
    assert!(
        text.starts_with(&head),
        "the top row names its view: {text:?}"
    );
    assert!(
        text.ends_with("Esc returns"),
        "Esc returns ends the row: {text:?}"
    );
    assert!(!text.contains('✕'), "no ✕ on the top row: {text:?}");
    let name_at = first + 1;
    for (dx, _cell) in name.chars().enumerate() {
        let x = name_at + dx as u16;
        assert!(grid.cell(x, 0).bold, "the name is bold ({x}, 0)");
    }
    let command_at = name_at + name.chars().count() as u16 + 2;
    for dx in 0..command.chars().count() as u16 {
        assert!(grid.cell(command_at + dx, 0).dim, "the command is dim");
    }
    let esc_at = last - 10;
    for dx in 0..11 {
        assert!(grid.cell(esc_at + dx, 0).dim, "Esc returns is dim");
    }
    (first, last)
}

/// Opens the slash command `command` and waits for the conversation to
/// give way to its view: the reply leaves the screen. The wait reads
/// content only, so it passes at the base; the asserts after it fail
/// there, which is what makes this test red before the fix.
fn open_view(run: &mut Run, command: &str) -> Grid {
    run.write(format!("/{command}\r").as_bytes());
    run.wait_screen("the view", |grid| !grid.contents.contains(REPLY))
}

/// Esc closes the open view: the reply is back on the screen.
fn esc_closes(run: &mut Run) {
    run.write(b"\x1b[27u");
    run.wait_screen("the conversation back", |grid| {
        grid.contents.contains(REPLY)
    });
}

#[test]
fn the_context_usage_and_tools_top_rows_match_the_doc() {
    let setup = setup_with_read();
    let mut run = turned(&setup, "go");
    // `/context`: the top row, the bold sizes, tool results in
    // `error`, and the handoff marker and label in `attention`.
    let grid = open_view(&mut run, "context");
    let (first, last) = assert_top_row(&grid, "Context", "/context");
    // The sizes row names the window: the doc's "268k tokens" bold
    // with its dim tail. ("of the" tells it from the panel's "30% of
    // window".) The base row names no window, so this fails there.
    let sizes = grid
        .rows
        .iter()
        .position(|row| row.contains("of the") && row.contains("tokens"))
        .unwrap_or_else(|| panic!("the sizes row in\n{}", grid.contents));
    let sizes = sizes as u16;
    let at = col_at(&grid, sizes, "tokens");
    for dx in 0..6 {
        assert!(grid.cell(at + dx, sizes).bold, "tokens is bold");
    }
    // The bar: the handoff marker in `attention` with its label. The
    // base bar draws no colour, so both fail there. (Per-category █
    // runs need a tokens-per-byte rate, which the scripted provider
    // never reports; the unit tests cover them with an injected rate.)
    let bar = grid
        .rows
        .iter()
        .position(|row| row.contains('█') && row.contains('│'))
        .unwrap_or_else(|| panic!("the context bar in\n{}", grid.contents));
    let bar = bar as u16;
    assert!(
        (0..160).any(|x| grid.cell(x, bar).symbol == "│" && grid.cell(x, bar).fg == ATTENTION),
        "the handoff marker in attention"
    );
    let label = col_at(&grid, bar + 1, "handoff 400k");
    for dx in 0..12 {
        assert_eq!(
            grid.cell(label + dx, bar + 1).fg,
            ATTENTION,
            "the handoff label {dx}"
        );
    }
    // The read landed in the largest tool results: the heading bold,
    // the row under it dim. Both draw plain at the base, so both fail
    // there.
    let largest = row_with(&grid, "Largest tool results");
    let at = col_at(&grid, largest, "Largest tool results");
    for (dx, _cell) in "Largest tool results".chars().enumerate() {
        assert!(
            grid.cell(at + dx as u16, largest).bold,
            "the largest heading is bold"
        );
    }
    let read = largest + 1;
    let at = col_at(&grid, read, "read");
    for dx in 0..4 {
        assert!(grid.cell(at + dx, read).dim, "the tool name is dim");
    }
    // A click on the middle of the top row returns to the
    // conversation: at the base only the ✕ closes, so the wait fails
    // there.
    click(&mut run, (first + last) / 2, 0);
    run.wait_screen("the conversation back", |grid| {
        grid.contents.contains(REPLY)
    });
    // The click leaves the pointer on row 0, where it would hover the
    // next view's top row off its tint: move it to a body row first.
    run.write(b"\x1b[<35;1;21M");
    // `/usage`: the top row, the three bold group headings, and the
    // dim budget row with `budget.usd` set.
    let grid = open_view(&mut run, "usage");
    assert_top_row(&grid, "Usage", "/usage");
    for heading in ["By turn", "By model", "By delegate"] {
        let row = row_with(&grid, heading);
        let at = col_at(&grid, row, heading);
        for (dx, _cell) in heading.chars().enumerate() {
            assert!(grid.cell(at + dx as u16, row).bold, "{heading} is bold");
        }
    }
    let budget = row_with(&grid, "budget left $20.00 of $20.00");
    let at = col_at(&grid, budget, "budget left $20.00 of $20.00");
    for dx in 0.."budget left $20.00 of $20.00".chars().count() as u16 {
        assert!(grid.cell(at + dx, budget).dim, "the budget row is dim");
    }
    esc_closes(&mut run);
    // `/tools`: the top row. The tools answer may still be on its
    // way; the row draws while it waits.
    let grid = open_view(&mut run, "tools");
    assert_top_row(&grid, "Tools", "/tools");
    esc_closes(&mut run);
}

/// A held delegate at 65% context and a background job on the same FIFO.
/// Neither can finish until the test releases its open FIFO or quits.
fn running_setup() -> (Setup, Hold, String) {
    let root = fakes::TempDir::under(std::path::Path::new("/tmp"), "p3");
    fs::create_dir_all(root.path().join("h")).unwrap();
    fs::create_dir_all(root.path().join("w")).unwrap();
    let setup = Setup {
        root,
        deadline: support::Deadline::start(),
    };
    let command = format!("head -n 1 {}", setup.workspace().join("hold").display());
    write_json(
        &setup.workspace().join("d.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": command}}], "usage": {"input": 650000}},
            {"text": ["Reviewed."]}
        ]}),
    );
    // The parent turn reports input usage on its first step, so the
    // Session `cache hits` row draws with the turn's first fold: later
    // delegate spend rewrites the row's values instead of adding a row,
    // and no later fold moves the cards under coordinates the test read.
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "delegate_spawn", "arguments": {
                "description": "review the diff", "prompt": "go", "model": "fiber:scripted/d.json"}}],
             "usage": {"input": 1200}},
            {"tool_calls": [{"name": "shell", "arguments": {"command": command, "run_in_background": true}}]},
            {"text": [REPLY]}
        ]}),
    );
    write_json(
        &setup.workspace().join("r.json"),
        &json!({"steps": [
            {"text": ["allow"]}, {"text": ["allow"]}, {"text": ["allow"]}, {"text": ["allow"]}
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json", "reviewer": {"model": "scripted/r.json"},
            "roles": {"delegate": "fiber:scripted/d.json"},
            "hub": {"idle_exit_ms": 1000}
        }),
    );
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
            json!({"decision": "allow", "tool": "shell", "prefix": command}),
        ),
    )
    .unwrap();
    let hold = Hold::park(&setup, &command);
    (setup, hold, command)
}

fn narrow_running(setup: &Setup) -> Run {
    let mut run = Run::spawn(setup, 80, 24, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    run.wait_screen("the parent reply", |grid| grid.contents.contains(REPLY));
    run.wait_screen("the narrow running counts", |grid| {
        grid.contents.contains("1 delegate running") && grid.contents.contains("1 job running")
    });
    run
}

fn open_running(run: &mut Run, count: &str, heading: &str) -> Grid {
    let grid = run.wait_screen("the running count", |grid| grid.contents.contains(count));
    let y = row_with(&grid, count);
    click(run, col_at(&grid, y, count), y);
    run.wait_screen("the running list", |grid| grid.contents.contains(heading))
}

/// Durations are normalised to their shape, never compared with real time.
fn running_duration(text: &str) -> bool {
    text.strip_suffix(['s', 'm', 'h', 'd'])
        .is_some_and(|digits| {
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
}

/// The delegate list's whole promised row, including its colours and inks.
fn delegate_list_ready(grid: &Grid) -> bool {
    let Some(y) = row_with_or(&grid.rows, "Delegates · 1 running") else {
        return false;
    };
    let Some(x) = col_at_or(grid, y, "Delegates · 1 running") else {
        return false;
    };
    let Some(desc) = grid.rows.get(usize::from(y + 1)) else {
        return false;
    };
    let Some(duration) = desc.trim().strip_prefix("◆ review the diff · ") else {
        return false;
    };
    if !running_duration(duration) {
        return false;
    }
    if !(0.."Delegates · 1 running".chars().count() as u16).all(|dx| grid.cell(x + dx, y).bold) {
        return false;
    }
    let detail = y + 2;
    let Some(model) = col_at_or(grid, detail, "scripted/d.json · 1 call · s_") else {
        return false;
    };
    if model != x + 2 {
        return false;
    }
    let Some(bar) = col_at_or(grid, detail, "▆▆▆▆▆▆░░░░ 65%") else {
        return false;
    };
    if !(model..bar).all(|col| grid.cell(col, detail).dim) {
        return false;
    }
    if !(0..6).all(|dx| grid.cell(bar + dx, detail).fg == ATTENTION) {
        return false;
    }
    if !(6..10).all(|dx| grid.cell(bar + dx, detail).fg == Colour::Rgb(58, 58, 74)) {
        return false;
    }
    if !(10..14).all(|dx| grid.cell(bar + dx, detail).dim) {
        return false;
    }
    let Some(hint) = row_with_or(&grid.rows, "a row opens that item's view") else {
        return false;
    };
    let Some(hint_x) = col_at_or(grid, hint, "a row opens that item's view") else {
        return false;
    };
    (0.."a row opens that item's view".chars().count() as u16)
        .all(|dx| grid.cell(hint_x + dx, hint).dim)
}

#[test]
fn the_running_lists_match_the_doc() {
    let (setup, _hold, command) = running_setup();
    let mut run = narrow_running(&setup);
    open_running(&mut run, "1 delegate running", "Delegates · 1 running");
    let grid = run.wait_screen("the delegate list's rows and colours", delegate_list_ready);
    assert!(delegate_list_ready(&grid));
    esc_closes(&mut run);
    let grid = open_running(&mut run, "1 job running", "Jobs · 1 running");
    let heading = row_with(&grid, "Jobs · 1 running");
    let x = col_at(&grid, heading, "Jobs · 1 running");
    for dx in 0.."Jobs · 1 running".chars().count() as u16 {
        assert!(grid.cell(x + dx, heading).bold);
    }
    let row = grid.rows[usize::from(heading + 1)].trim();
    let tail = row
        .strip_prefix(&format!("◆ {command} · "))
        .expect("the job command");
    let (elapsed, job) = tail.split_once(" · ").expect("the job elapsed and id");
    assert!(running_duration(elapsed));
    assert!(job.starts_with("j_"));
    let hint = row_with(&grid, "a row opens that item's view");
    let x = col_at(&grid, hint, "a row opens that item's view");
    for dx in 0.."a row opens that item's view".chars().count() as u16 {
        assert!(grid.cell(x + dx, hint).dim);
    }
}

/// The two no-file results, with their complete labels and match inks.
fn search_results_ready(grid: &Grid) -> bool {
    if col_at_or(grid, 0, " Search  ⌕ lock").is_none() {
        return false;
    }
    let (first, last) = top_run(grid, "Search");
    if !(first..=last).all(|x| grid.cell(x, 0).bg == SURFACE) {
        return false;
    }
    for (text, bold) in [("Search", true), ("⌕ lock", false), ("Esc returns", false)] {
        let Some(x) = col_at_or(grid, 0, text) else {
            return false;
        };
        if !(0..text.chars().count() as u16).all(|dx| {
            if bold {
                grid.cell(x + dx, 0).bold
            } else {
                grid.cell(x + dx, 0).dim
            }
        }) {
            return false;
        }
    }
    let Some(count) = row_with_or(&grid.rows, "of 2 · ⌕ lock") else {
        return false;
    };
    let Some(x) = col_at_or(grid, count, "of 2 · ⌕ lock") else {
        return false;
    };
    if !(0.."of 2 · ⌕ lock".chars().count() as u16).all(|dx| grid.cell(x + dx, count).dim) {
        return false;
    }
    for label in ["turn 1", "turn 2"] {
        let Some(y) = row_with_or(&grid.rows, label) else {
            return false;
        };
        let text: String = (first..=last).map(|x| grid.cell(x, y).symbol).collect();
        if text.trim() != label {
            return false;
        }
        if !(last - 5..=last).all(|x| grid.cell(x, y).dim) {
            return false;
        }
    }
    let Some(hint) = row_with_or(&grid.rows, "Enter jumps to the match") else {
        return false;
    };
    if !(0..23).all(|dx| grid.cell(first + dx, hint).dim) {
        return false;
    }
    if grid.cell((first + last) / 2, hint + 1).symbol != "▄" {
        return false;
    }
    let cells = (0..grid.size().1)
        .flat_map(|y| (first..=last).map(move |x| (x, y)))
        .map(|(x, y)| grid.cell(x, y));
    let (mut matched, mut current) = (false, false);
    for cell in cells {
        if cell.bg == MATCH {
            if cell.fg != WHITE {
                return false;
            }
            matched = true;
        }
        if cell.bg == CURRENT {
            if cell.fg != BLACK {
                return false;
            }
            current = true;
        }
    }
    matched && current
}

#[test]
fn the_search_results_rows_match_the_doc() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("a.txt"), "filehit\n").unwrap();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "read", "arguments": {"path": "a.txt"}}]},
            {"text": ["lock one"]}, {"text": ["lock two"]}
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    let mut run = turned(&setup, "first");
    let from = run.output().len();
    run.write(b"second\r");
    run.turn_finished(from);
    run.write(b"\x06lock");
    run.wait_screen("the two search matches", |grid| {
        grid.contents.contains("of 2")
    });
    run.write(b"\x06");
    let grid = run.wait_screen("the result rows, labels and inks", search_results_ready);
    assert!(search_results_ready(&grid));
    let (first, last) = assert_top_row(&grid, "Search", "⌕ lock");
    click(&mut run, (first + last) / 2, 0);
    run.wait_screen(
        "the conversation after the search top row returns",
        |grid| {
            grid.contents.contains("lock two")
                && !grid.contents.contains("Enter jumps to the match")
        },
    );
    run.write(b"\x1b[27u");
    run.wait_screen("the closed search bar", |grid| !grid.contents.contains('⌕'));
    run.write(b"\x1b[<35;1;21M");
    run.write(b"\x06filehit");
    run.wait_screen("the file-call match", |grid| grid.contents.contains("of 1"));
    run.write(b"\x06");
    let grid = run.wait_screen("the file-call path and turn", |grid| {
        let Some(y) = row_with_or(&grid.rows, "turn 1") else {
            return false;
        };
        let Some(path) = col_at_or(grid, y, "a.txt") else {
            return false;
        };
        let Some(turn) = col_at_or(grid, y, "turn 1") else {
            return false;
        };
        (0..5).all(|dx| grid.cell(path + dx, y).bold)
            && (0..6).all(|dx| grid.cell(turn + dx, y).dim)
    });
    let y = row_with(&grid, "turn 1");
    assert!(grid.rows[usize::from(y)].contains("a.txt"));
    let (_, last) = assert_top_row(&grid, "Search", "⌕ filehit");
    assert_eq!(col_at(&grid, y, "turn 1") + 5, last);
}

fn changed_file_row_ready(grid: &Grid) -> bool {
    if !grid.rows[0].contains(" Changed files  diff against HEAD") {
        return false;
    }
    let Some(y) = row_with_or(&grid.rows, "changed.txt · +2 −0") else {
        return false;
    };
    let Some(x) = col_at_or(grid, y, "changed.txt · +2 −0") else {
        return false;
    };
    (0..11).all(|dx| grid.cell(x + dx, y).bold)
        && (11..14).all(|dx| grid.cell(x + dx, y).dim)
        && (14..16).all(|dx| grid.cell(x + dx, y).fg == Colour::Rgb(115, 201, 145))
        && (17..19).all(|dx| grid.cell(x + dx, y).fg == Colour::Rgb(255, 93, 115))
}

#[test]
fn the_changed_files_rows_match_the_doc() {
    let setup = Setup::new();
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "write", "arguments": {"path": "changed.txt", "content": "one\ntwo\n"}}]},
            {"text": [REPLY]}
        ]}),
    );
    write_json(
        &setup.workspace().join("r.json"),
        &json!({"steps": [{"text": ["allow"]}]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({
            "model": "scripted/s.json", "reviewer": {"model": "scripted/r.json"}, "hub": {"idle_exit_ms": 1000}
        }),
    );
    let mut run = turned(&setup, "go");
    let grid = run.wait_screen("the changed-files panel card", |grid| {
        session_settled(grid) && panel_item_at(grid, "changed.txt").is_some()
    });
    let (_, y) = panel_item_at(&grid, "changed.txt").unwrap();
    let x = col_at(&grid, y + 1, "+2");
    click(&mut run, x, y + 1);
    let grid = run.wait_screen("the changed-file row and inks", changed_file_row_ready);
    assert!(changed_file_row_ready(&grid));
}

/// Turn complete (`✓ READY`) and the Session cost row figured: no later fold in
/// this turn moves the cards. The speed row keeps its reserved place with
/// a dash until its timed reply, so it is not part of the settle.
fn session_settled(grid: &Grid) -> bool {
    // The panel starts at column 100, as in `panel_item_at`: a `cost`
    // elsewhere (a conversation echo, another card) does not settle us.
    support::session_cost_drawn(grid, 100) && grid.contents.contains("✓ READY")
}

/// The Session spend row figured in the panel: the parent script reports
/// input usage, so `cache hits` lands with the turn's first fold and
/// later delegate spend only rewrites the row's values
/// (`crates/tui/src/view/panel.rs`, `session_rows`). The reserved row
/// draws the label with a dim dash, so the wait reads the figure: a row
/// whose value is not the dash. A wait that supplies panel coordinates
/// for a later write holds until the figure lands, so no later fold
/// moves the cards under the stored coordinates.
fn session_spend_drawn(grid: &Grid) -> bool {
    panel_item_at(grid, "cache hits").is_some_and(|(_, y)| {
        grid.rows
            .get(usize::from(y))
            .is_some_and(|row| row.contains('%'))
    })
}

fn panel_item_at(grid: &Grid, description: &str) -> Option<(u16, u16)> {
    grid.rows.iter().enumerate().find_map(|(y, _)| {
        let y = u16::try_from(y).ok()?;
        let x = col_at_or(grid, y, description)?;
        (x >= 100).then_some((x, y))
    })
}

/// A card's first item, not a matching subject in another card's progress row.
fn panel_card_item_at(grid: &Grid, card: &str, description: &str) -> Option<(u16, u16)> {
    let (_, heading) = panel_item_at(grid, card)?;
    let y = heading.checked_add(1)?;
    Some((col_at_or(grid, y, description)?, y))
}

#[test]
fn the_item_views_top_row_closes_them() {
    let (setup, hold, command) = running_setup();
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"go\r");
    let grid = run.wait_screen("the parent reply and running panel cards", |grid| {
        grid.contents.contains(REPLY)
            && panel_item_at(grid, "review the diff").is_some()
            && panel_item_at(grid, "Jobs").is_some()
            && support::session_cost_drawn(grid, 100)
            && session_spend_drawn(grid)
    });
    let (x, y) = panel_item_at(&grid, "Jobs").unwrap();
    click(&mut run, x, y);
    run.wait_screen("the expanded Jobs card", |grid| {
        panel_item_at(grid, "head -n 1").is_some()
    });
    for (card, description, crumb) in [
        (
            "Delegates",
            "review the diff",
            "Delegate  ◆ review the diff".to_owned(),
        ),
        ("Jobs", "head -n 1", format!("Job  ◆ {command}")),
    ] {
        let grid = run.wait_screen("the running panel card's first item", |grid| {
            support::session_cost_drawn(grid, 100)
                && session_spend_drawn(grid)
                && panel_card_item_at(grid, card, description).is_some()
        });
        let (x, y) = panel_card_item_at(&grid, card, description).unwrap();
        // The hover frame acknowledges this cell as a live click target.
        // Card surface (26, 26, 34) lifts by 20 per channel under the pointer.
        run.write(format!("\x1b[<35;{};{}M", x + 1, y + 1).as_bytes());
        run.wait_screen("the running item's hover acknowledgement", |grid| {
            support::session_cost_drawn(grid, 100)
                && session_spend_drawn(grid)
                && panel_card_item_at(grid, card, description) == Some((x, y))
                && grid.cell(x, y).bg == Colour::Rgb(46, 46, 54)
        });
        click(&mut run, x, y);
        let grid = run.wait_screen("the item's breadcrumb without a cross", |grid| {
            grid.rows[0].contains(&crumb) && !grid.rows[0].contains('✕')
        });
        assert!(grid.rows[0].contains(&crumb));
        assert!(!grid.rows[0].contains('✕'));
        click(&mut run, 80, 0);
        run.wait_screen(
            "the conversation after clicking the item's top row",
            |grid| grid.contents.contains(REPLY) && !grid.rows[0].contains(&crumb),
        );
    }
    // Tear down in the open: kill the run, unlink the FIFO, then close
    // the release (EOF lets every blocked `head` read and exit) and
    // re-kill the hold pattern until no holder matches. Dropping the
    // last writer before a head opens the FIFO strands it there forever
    // (`docs/testing.md`, "Waits and timeouts"), and the drop's one-shot
    // watchdog kill only reaps what has exec'd by then. Unlinking first
    // closes the delayed-start window the ticket names: a head that
    // starts after the unlink fails its open with ENOENT and exits
    // within milliseconds, which the temp-dir detector's bounded wait
    // absorbs, so cleanup may stop at the first zero. The re-kill still
    // reaps openers caught mid-syscall across the unlink. If cleanup
    // cannot complete, it panics naming the stragglers; the temp-dir
    // detector is skipped while unwinding.
    let pattern = hold.path().display().to_string();
    drop(run);
    fs::remove_file(hold.path()).unwrap();
    drop(hold);
    let deadline = setup.deadline.cleanup_phase();
    let mut clean = false;
    while !deadline.left().is_zero() {
        if fakes::kill_matching(pattern.as_str()).is_err() {
            break;
        }
        if fakes::matching_exits(
            pattern.as_str(),
            fakes::Deadline::after(std::time::Duration::from_millis(100)),
        ) {
            clean = true;
            break;
        }
    }
    if !clean {
        let stragglers = fakes::matching(pattern.as_str()).unwrap_or_default();
        panic!(
            "3875 cleanup: the hold pattern never stayed clear (stragglers: {stragglers:?}); the temp-dir detector is skipped while unwinding"
        );
    }
}
