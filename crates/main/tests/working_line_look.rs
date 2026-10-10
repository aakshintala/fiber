//! Binary-level tests of the working line, the steering queue and the
//! input box (`docs/tui.md`, "The working line", "Steering", "The input
//! box"): the real binary on a sized pty in truecolour, with a scripted
//! provider whose sleep call holds the turn open on approval while the
//! journey queues steering messages, selects one, and fills the input box.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::{Path, PathBuf};

use support::Setup;
use support::pty::{Colour, Grid, Run};

/// Truecolour on a terminal that draws stripes, as look.rs passes it.
const TRUECOLOUR: [(&str, &str); 3] = [
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("TERM_PROGRAM", "ghostty"),
];

/// The dark theme's attention, info and surface in truecolour.
const ATTENTION: Colour = Colour::Rgb(255, 159, 67);
const INFO: Colour = Colour::Rgb(125, 211, 252);
const SURFACE: Colour = Colour::Rgb(26, 26, 34);

/// The working line's spinner frames (`docs/tui.md`, "The working line").
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Escape, Alt+X and Alt+A as the kitty keyboard protocol encodes them;
/// Alt+Up keeps its xterm form (`crates/tui/src/keys.rs`).
const ESC: &[u8] = b"\x1b[27u";
const ALT_UP: &[u8] = b"\x1b[1;3A";
const ALT_X: &[u8] = b"\x1b[120;3u";
const ALT_A: &[u8] = b"\x1b[97;3u";

// Backspaces far past any draft below: extras on an empty draft are
// no-ops.
fn clear_draft(run: &mut Run) {
    run.write(&vec![b"\x1b[127u".as_slice(); 200].concat());
}

/// A scripted provider holding the turn open with fragments paced every
/// `every_ms`: only the reduced-motion test uses it, where the pacing
/// drives the animation and each assertion waits on a fragment's bytes.
fn script(setup: &Setup, fragments: usize, every_ms: u64) {
    let text: Vec<String> = (0..fragments).map(|n| format!("frag{n:02} ")).collect();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [
            {"text": text, "every_ms": every_ms},
            {"text": ["done."]},
        ]}),
    );
}

/// A scripted provider whose turn calls `shell` with `command`, then says
/// done: the call asks approval, and answering it runs the command.
/// Steers sent while the approval waits queue visibly; answering it runs
/// the tool with the input box back (`docs/tui.md`, "Steering").
fn shell_script(setup: &Setup, command: &str) {
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": command}}]},
            {"text": ["done."]},
            {"text": ["done."]},
        ]}),
    );
}

/// A shell command that returns once `hold` is gone. The test creates the
/// file before the turn starts and removes it when it has read what it
/// needs, so the tool runs exactly as long as the test says. The loop
/// stops itself at 120 s (`docs/testing.md`, "Running tests").
fn hold_command(hold: &Path) -> String {
    format!(
        "i=0; while [ -e '{}' ] && [ $i -lt 2400 ]; do sleep 0.05; i=$((i+1)); done",
        hold.display()
    )
}

/// The turn's shell call, held on a marker file the test removes.
fn held_tool(setup: &Setup) -> PathBuf {
    let hold = setup.workspace().join("hold");
    std::fs::write(&hold, "").expect("the hold marker");
    shell_script(setup, &hold_command(&hold));
    hold
}

/// Answers the approval on screen and waits for the held tool to run
/// with the approval panel gone: the box the windows read is the running
/// turn's.
fn run_held_tool(run: &mut Run) {
    run.wait_screen("the approval", |grid| {
        grid.rows.iter().any(|row| row.contains("allow once"))
    });
    run.write(b"\r");
    run.wait_screen("the tool running", |grid| {
        !grid.rows.iter().any(|row| row.contains("allow once")) && session_box(grid).is_some()
    });
}

/// Puts the approval aside so the session box shows behind its badge, then
/// waits for the box.
fn aside_approval(run: &mut Run) {
    run.wait_screen("the approval", |grid| {
        grid.rows.iter().any(|row| row.contains("allow once"))
    });
    run.write(ESC);
    run.wait_screen("the box behind the badge", |grid| {
        session_box(grid).is_some()
    });
}

/// Reopens the approval and answers it, which lets the turn run on.
fn answer_approval(run: &mut Run) {
    run.write(ALT_A);
    run.wait_screen("the reopened approval", |grid| {
        grid.rows.iter().any(|row| row.contains("allow once"))
    });
    run.write(b"\r");
}

/// The isolated config with the scripted model, and `tui.reduced_motion`
/// as asked (`docs/configuration.md`).
fn config(setup: &Setup, reduced_motion: bool) {
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({
            "model": "scripted/s.json",
            "hub": {"idle_exit_ms": 1000},
            "tui": {"reduced_motion": reduced_motion},
        }),
    );
}

/// The first row containing `needle`.
fn find_row(rows: &[String], needle: &str) -> Option<usize> {
    rows.iter().position(|row| row.contains(needle))
}

/// A row index as a cell coordinate: the screens are fixed sizes.
fn at(y: usize) -> u16 {
    u16::try_from(y).expect("a screen row")
}

/// The cells a row's text takes: every glyph on screen is one cell.
fn width(row: &str) -> u16 {
    u16::try_from(row.chars().count()).unwrap_or(u16::MAX)
}

/// The column of `ch` in `row`, counting cells.
fn col_of(row: &str, ch: char) -> Option<u16> {
    let mut col = 0u16;
    for cell in row.chars() {
        if cell == ch {
            return Some(col);
        }
        col = col.saturating_add(1);
    }
    None
}

/// Whether the cell is blank.
fn is_blank(grid: &Grid, x: u16, y: u16) -> bool {
    grid.cell(x, y).symbol.as_str() == " "
}

/// The working line's row: its word's column, its spinner's column and
/// symbol, and the attention columns inside its word.
struct Working {
    row: u16,
    word: u16,
    spinner: (u16, String),
    band: Vec<u16>,
}

/// The working line on `grid`, if its word shows with a glimmer band.
fn working(grid: &Grid) -> Option<Working> {
    let row = at(find_row(&grid.rows, "Working")?);
    let word = col_of(&grid.rows[row as usize], 'W')?;
    let spinner_col = word.saturating_sub(2);
    let spinner = grid.cell(spinner_col, row).symbol.clone();
    let mut band = Vec::new();
    for x in word..word.saturating_add(7) {
        if grid.cell(x, row).fg == ATTENTION {
            band.push(x);
        }
    }
    if band.is_empty() {
        None
    } else {
        Some(Working {
            row,
            word,
            spinner: (spinner_col, spinner),
            band,
        })
    }
}

/// The last row holding the input box's stripe: the session box is the
/// only striped surface left, the queue having none.
fn input_row(grid: &Grid) -> Option<u16> {
    (0..grid.rows.len())
        .rev()
        .find(|y| grid.rows[*y].contains('▌'))
        .map(at)
}

/// A text row holding `needle` whose box edges both show: a frame may
/// arrive with the row but before its edges.
fn boxed_row(grid: &Grid, needle: &str) -> Option<(u16, u16)> {
    let y = find_row(&grid.rows, needle)?;
    let x = col_of(&grid.rows[y], needle.chars().next()?)?;
    box_around(&grid.rows, at(y), x)?;
    Some((at(y), x))
}

/// The session box's row once its edges both show.
fn session_box(grid: &Grid) -> Option<(u16, u16)> {
    let y = input_row(grid)?;
    let x = col_of(&grid.rows[y as usize], '▌')?;
    box_around(&grid.rows, y, x)?;
    Some((y, x))
}

/// The box around text row `y`: the nearest ▄ run above and ▀ run
/// below through column `x`, with their extents. Runs elsewhere on the
/// same rows, such as the panel's cards, do not count.
fn box_around(text: &[String], y: u16, x: u16) -> Option<(u16, u16, u16, u16)> {
    let run = |row: &str, glyph: char| {
        let cells: Vec<char> = row.chars().collect();
        if cells.get(x as usize) != Some(&glyph) {
            return None;
        }
        let mut left = x;
        while left > 0 && cells.get(left as usize - 1) == Some(&glyph) {
            left -= 1;
        }
        let mut right = x;
        while cells.get(right as usize + 1) == Some(&glyph) {
            right += 1;
        }
        Some((left, right))
    };
    let mut top = y;
    while top > 0 && run(&text[top as usize - 1], '▄').is_none() {
        top -= 1;
    }
    if top == 0 {
        return None;
    }
    top -= 1;
    let (left, _) = run(&text[top as usize], '▄')?;
    let mut bottom = y;
    while (bottom as usize) + 1 < text.len() && run(&text[bottom as usize + 1], '▀').is_none() {
        bottom += 1;
    }
    if (bottom as usize) + 1 >= text.len() {
        return None;
    }
    bottom += 1;
    let (_, right) = run(&text[bottom as usize], '▀')?;
    Some((top, bottom, left, right))
}

/// The box's fill: every text row between its edges is surface background
/// edge to edge, and both edge runs are the surface colour over the
/// surrounding background (`docs/tui.md`, "The input box", "Look").
fn assert_fill(grid: &Grid, y: u16, x: u16, what: &str) {
    let (top, bottom, left, right) = box_around(&grid.rows, y, x).expect(what);
    for edge in [top, bottom] {
        for x in left..=right {
            let cell = grid.cell(x, edge);
            assert_eq!(cell.fg, SURFACE, "{what} edge ({x}, {edge})");
            assert_eq!(cell.bg, Colour::Default, "{what} edge ({x}, {edge})");
        }
    }
    for row in top + 1..bottom {
        for x in left..=right {
            let cell = grid.cell(x, row);
            assert_eq!(cell.bg, SURFACE, "{what} fill ({x}, {row})");
        }
    }
}

/// Every content cell from `start` to `end` on row `y` is dim: the
/// span covers the chrome alone, never the panel beside it.
fn assert_dim_span(grid: &Grid, y: u16, start: u16, end: u16, what: &str) {
    for x in start..end {
        if !is_blank(grid, x, y) {
            assert!(grid.cell(x, y).dim, "{what} cell {x}");
        }
    }
}

/// Quits an idle run with an empty draft, as look.rs does.
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

/// The working line's glimmer, the steering queue and the input box at
/// 160x48: two frames prove the band shifts its attention cells, the
/// spinner stays in attention, the elapsed time and tail stay dim, the
/// queue selects with its stripe and hint, and the box fills for empty,
/// one-word and wrapping drafts on home and in a session.
#[test]
fn the_working_line_glimmers_and_the_queue_selects() {
    let setup = Setup::new();
    let hold = held_tool(&setup);
    config(&setup, false);
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();

    // Home with an empty draft: the box fills edge to edge.
    let mut grid = run.wait_screen("home with its box", |grid| {
        boxed_row(grid, "? for shortcuts").is_some()
    });
    let (home_row, home_x) = boxed_row(&grid, "? for shortcuts").expect("the placeholder row");
    assert_fill(&grid, home_row, home_x, "home empty");

    // Home with one word, then a wrapping draft: the fill follows.
    run.write(b"kq");
    grid = run.wait_screen("home with one word", |grid| {
        boxed_row(grid, "› kq").is_some()
    });
    let (one_row, one_x) = boxed_row(&grid, "› kq").expect("one word");
    assert_fill(&grid, one_row, one_x, "home one word");
    run.write(b"w".repeat(150).as_slice());
    grid = run.wait_screen("home wrapping", |grid| {
        boxed_row(grid, "› kq").is_some()
            && grid.rows.iter().filter(|row| row.contains('w')).count() >= 2
    });
    let (wrap, wrap_x) = boxed_row(&grid, "› kq").expect("the wrapping draft");
    assert_fill(&grid, wrap, wrap_x, "home wrapping");

    // The prompt starts the turn; the draft is empty again.
    let from = run.output().len();
    run.write(b"\r");
    // The empty session box is asserted in
    // `the_empty_session_box_fills_while_the_turn_streams`: the sleep
    // call's approval can arrive before any frame shows the empty box.

    // The call asks approval: Esc puts it aside behind its badge, and
    // the input box comes back with the turn still waiting. Steers typed
    // now queue visibly while the approval waits. The queue's marks tell
    // its rows from the draft they were typed in.
    aside_approval(&mut run);
    run.write(b"first\r");
    run.write(b"second\r");
    grid = run.wait_screen("both queued rows", |grid| {
        grid.rows.iter().any(|row| row.contains("↳ first"))
            && grid.rows.iter().any(|row| row.contains("↳ second"))
    });
    let heading = find_row(&grid.rows, "Steering, joins the turn").expect("the heading");
    assert_dim_span(
        &grid,
        at(heading),
        0,
        2 + width("• Steering, joins the turn at the next step"),
        "heading",
    );
    for needle in ["↳ first", "↳ second"] {
        let y = find_row(&grid.rows, needle).expect(needle);
        assert!(grid.rows[y].contains('✕'), "{needle}");
        assert_dim_span(&grid, at(y), 0, 2 + width(needle) + 3, needle);
    }
    let footer = find_row(&grid.rows, "click a row to edit").expect("the footer");
    assert_dim_span(
        &grid,
        at(footer),
        0,
        2 + width("⌥↑ edit · ⌥↓ next · ⌥x drop · click a row to edit, ✕ to drop"),
        "footer",
    );

    // Reopening the approval and answering it starts the held command:
    // the panel goes for good with the turn still running, until the test
    // removes the marker.
    answer_approval(&mut run);
    run.wait_screen("the tool running", |grid| {
        !grid.rows.iter().any(|row| row.contains("allow once")) && working(grid).is_some()
    });

    // Frame A: the band shows on the running tool's line.
    grid = run.wait_screen("the glimmer band", |grid| working(grid).is_some());
    let first = working(&grid).expect("frame A");
    assert!(
        SPINNER.contains(&first.spinner.1.as_str()),
        "{}",
        first.spinner.1
    );
    assert_eq!(grid.cell(first.spinner.0, first.row).fg, ATTENTION);
    // Frame B: the band shifted.
    grid = run.wait_screen("the shifted band", |grid| {
        working(grid).is_some_and(|frame| frame.band != first.band)
    });
    let frame = working(&grid).expect("frame B");
    assert!(
        SPINNER.contains(&frame.spinner.1.as_str()),
        "{}",
        frame.spinner.1
    );
    assert_eq!(grid.cell(frame.spinner.0, frame.row).fg, ATTENTION);
    // The band is at most three attention cells with a bold centre; the
    // rest of the word, the elapsed time and the tail stay dim.
    assert!((1..=3).contains(&frame.band.len()), "{:?}", frame.band);
    for pair in frame.band.windows(2) {
        assert_eq!(pair[0] + 1, pair[1], "{:?}", frame.band);
    }
    let centre = frame.band[frame.band.len() / 2];
    for x in frame.word..frame.word + 7 {
        let cell = grid.cell(x, frame.row);
        if x == centre {
            assert_eq!(cell.fg, ATTENTION, "centre {x}");
            assert!(cell.bold, "centre {x}");
        } else if frame.band.contains(&x) {
            assert_eq!(cell.fg, ATTENTION, "side {x}");
            assert!(!cell.bold, "side {x}");
        } else {
            assert!(cell.dim, "word {x}");
        }
    }
    // The line ends with the tail: past it sits whatever shares the row,
    // such as the panel, which the tail asserts leave alone.
    let tail_end = grid.rows[frame.row as usize]
        .find("interrupt")
        .map(|at| grid.rows[frame.row as usize][..at].chars().count() + 9)
        .expect("the tail");
    for x in frame.word + 7..u16::try_from(tail_end).unwrap_or(u16::MAX) {
        assert!(grid.cell(x, frame.row).dim, "tail {x}");
    }

    // ⌥↑ selects the newest row: its mark in attention, its text default,
    // the box's stripe in attention, and the dim hint at its right end.
    run.write(ALT_UP);
    run.wait_screen("the selected row", |grid| {
        grid.rows.iter().any(|row| row.contains("▸ second"))
    });
    // The hint is right-aligned to the box's width, which settles as
    // the panel fills in: wait for it whole, not just started.
    grid = run.wait_screen("the full hint", |grid| {
        grid.rows.iter().any(|row| row.contains("esc stops"))
    });
    let selected = find_row(&grid.rows, "▸ second").expect("the selected row");
    let mark = col_of(&grid.rows[selected], '▸').expect("the mark");
    assert_eq!(grid.cell(mark, at(selected)).fg, ATTENTION);
    let text_col = col_of(&grid.rows[selected], 's').expect("the text");
    for x in text_col..text_col + 6 {
        assert_eq!(grid.cell(x, at(selected)).fg, Colour::Default, "text {x}");
    }
    let box_row = input_row(&grid).expect("the box");
    assert_eq!(grid.cell(0, box_row).symbol.as_str(), "▌");
    assert_eq!(grid.cell(0, box_row).fg, ATTENTION);
    let hint = find_row(&grid.rows, "editing a queued message").expect("the hint");
    assert!(
        grid.rows[hint].trim_end().ends_with("esc stops"),
        "{}",
        grid.rows[hint]
    );
    // The hint ends at the box's right end: only its own span is dim,
    // past the stripe, the prompt, the draft and the cursor.
    let hint_row = at(hint);
    let hint_start = grid.rows[hint]
        .find("editing a queued message")
        .map(|at| grid.rows[hint][..at].chars().count())
        .expect("the hint start");
    for x in hint_start..usize::from(width(&grid.rows[hint])) {
        let x = u16::try_from(x).unwrap_or(u16::MAX);
        if !is_blank(&grid, x, hint_row) {
            assert!(grid.cell(x, hint_row).dim, "hint {x}");
        }
    }

    // Esc stops the edit and ⌥x drops the queue; the session box fills
    // for one word and for a wrapping draft.
    run.write(ESC);
    run.write(ALT_X);
    run.write(b"kq");
    grid = run.wait_screen("one word in the session", |grid| {
        session_box(grid).is_some_and(|(y, _)| grid.rows[y as usize].contains("kq"))
    });
    let (one_box, one_box_x) = session_box(&grid).expect("the box");
    assert_fill(&grid, one_box, one_box_x, "session one word");
    run.write(b"w".repeat(150).as_slice());
    grid = run.wait_screen("wrapping in the session", |grid| {
        session_box(grid).is_some_and(|(y, _)| {
            grid.rows.iter().filter(|row| row.contains('w')).count() >= 2
                && grid.rows[y as usize].contains('w')
        })
    });
    let (wrap_box, wrap_box_x) = session_box(&grid).expect("the box");
    assert_fill(&grid, wrap_box, wrap_box_x, "session wrapping");

    // The draft clears and the tool is released: the turn ends.
    clear_draft(&mut run);
    std::fs::remove_file(&hold).expect("the hold marker");
    run.turn_finished(from);
    quit(run);
}

/// The empty session box at 160x48 fills edge to edge and shows its
/// info-coloured prompt mark. A shell call held on a marker file holds the
/// turn open, so the state asserted is the one held.
#[test]
fn the_empty_session_box_fills_while_the_turn_streams() {
    let setup = Setup::new();
    let hold = held_tool(&setup);
    config(&setup, false);
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"kq");
    run.wait_screen("home with one word", |grid| {
        boxed_row(grid, "› kq").is_some()
    });
    let from = run.output().len();
    run.write(b"\r");
    run_held_tool(&mut run);
    let grid = run.wait_screen("the empty session box", |grid| session_box(grid).is_some());
    let (empty_box, empty_x) = session_box(&grid).expect("the box");
    assert_fill(&grid, empty_box, empty_x, "session empty");
    assert_eq!(grid.cell(empty_x + 2, empty_box).symbol.as_str(), "›");
    assert_eq!(grid.cell(empty_x + 2, empty_box).fg, INFO);
    std::fs::remove_file(&hold).expect("the hold marker");
    run.turn_finished(from);
    quit(run);
}

/// Under `tui.reduced_motion` the working line stays still at 160x48:
/// the spinner is ● and no cell of the line is bold, across frames.
#[test]
fn the_reduced_working_line_stays_still() {
    let setup = Setup::new();
    script(&setup, 30, 400);
    config(&setup, true);
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let mut from = run.output().len();
    run.write(b"go\r");
    // Two frames pages apart: the spinner is still ● and the word plain.
    // The paced fragments mark the frames' distance in wall time.
    for needle in ["frag08", "frag20"] {
        from = run.wait_bytes(from, needle.as_bytes(), "the paced reply");
        let grid = run.screen();
        let row = find_row(&grid.rows, "Working").expect("the working line");
        let word = col_of(&grid.rows[row], 'W').expect("the word");
        let spinner_col = word.saturating_sub(2);
        assert_eq!(
            grid.cell(spinner_col, at(row)).symbol.as_str(),
            "●",
            "{needle}"
        );
        assert_eq!(grid.cell(spinner_col, at(row)).fg, ATTENTION, "{needle}");
        for x in word..word + 7 {
            assert!(grid.cell(x, at(row)).dim, "{needle} word {x}");
        }
        for x in 0..width(grid.rows[row].trim_end()) {
            assert!(!grid.cell(x, at(row)).bold, "{needle} cell {x}");
        }
    }
    run.turn_finished(from);
    quit(run);
}

/// The input box fills edge to edge at 100x40, on home and in a session,
/// for empty, one-word and wrapping drafts.
#[test]
fn the_input_box_fills_at_100x40() {
    let setup = Setup::new();
    let hold = held_tool(&setup);
    config(&setup, false);
    let mut run = Run::spawn(&setup, 100, 40, &[], &TRUECOLOUR);
    run.ready();
    let mut grid = run.wait_screen("home with its box", |grid| {
        boxed_row(grid, "? for shortcuts").is_some()
    });
    let (empty_100, empty_100_x) = boxed_row(&grid, "? for shortcuts").expect("empty");
    assert_fill(&grid, empty_100, empty_100_x, "home empty");
    run.write(b"kq");
    grid = run.wait_screen("home with one word", |grid| {
        boxed_row(grid, "› kq").is_some()
    });
    let (one_100, one_100_x) = boxed_row(&grid, "› kq").expect("one word");
    assert_fill(&grid, one_100, one_100_x, "home one word");
    run.write(b"w".repeat(150).as_slice());
    grid = run.wait_screen("home wrapping", |grid| {
        boxed_row(grid, "› kq").is_some()
            && grid.rows.iter().filter(|row| row.contains('w')).count() >= 2
    });
    let (wrap_100, wrap_100_x) = boxed_row(&grid, "› kq").expect("wrapping");
    assert_fill(&grid, wrap_100, wrap_100_x, "home wrapping");
    let from = run.output().len();
    run.write(b"\r");
    run_held_tool(&mut run);
    grid = run.wait_screen("the empty session box", |grid| session_box(grid).is_some());
    let (empty_s, empty_s_x) = session_box(&grid).expect("the box");
    assert_fill(&grid, empty_s, empty_s_x, "session empty");
    run.write(b"kq");
    grid = run.wait_screen("one word in the session", |grid| {
        session_box(grid).is_some_and(|(y, _)| grid.rows[y as usize].contains("kq"))
    });
    let (one_s, one_s_x) = session_box(&grid).expect("the box");
    assert_fill(&grid, one_s, one_s_x, "session one word");
    run.write(b"w".repeat(150).as_slice());
    grid = run.wait_screen("wrapping in the session", |grid| {
        session_box(grid).is_some_and(|(y, _)| {
            grid.rows.iter().filter(|row| row.contains('w')).count() >= 2
                && grid.rows[y as usize].contains('w')
        })
    });
    let (wrap_s, wrap_s_x) = session_box(&grid).expect("the box");
    assert_fill(&grid, wrap_s, wrap_s_x, "session wrapping");
    clear_draft(&mut run);
    std::fs::remove_file(&hold).expect("the hold marker");
    run.turn_finished(from);
    quit(run);
}
