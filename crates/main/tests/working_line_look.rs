//! Binary-level tests of the working line, the steering queue and the
//! input box (`docs/tui.md`, "The working line", "Steering", "The input
//! box"): the real binary on a sized pty in truecolour, with a scripted
//! provider whose paced reply holds the turn open while the journey
//! queues steering messages, selects one, and fills the input box.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use support::Setup;
use support::pty::{Colour, Run, Screen};

/// Truecolour on a terminal that draws stripes, as look.rs passes it.
const TRUECOLOUR: &[(&str, &str)] = &[
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

/// A scripted provider holding each turn open: `fragments` paced every
/// `every_ms`, then one short reply for a turn a queued message starts.
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

/// A scripted provider whose turn calls `sleep`, then says done: the
/// call asks approval, and answering it starts the sleep. Steers sent
/// while the approval waits queue visibly; answering it runs the tool
/// with the input box back (`docs/tui.md`, "Steering").
fn sleep_script(setup: &Setup, secs: u64) {
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [
            {"tool_calls": [{"name": "shell", "arguments": {"command": format!("sleep {secs}")}}]},
            {"text": ["done."]},
            {"text": ["done."]},
        ]}),
    );
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

/// Every screen row as text, one cell per symbol.
fn text_rows(screen: &Screen, cols: u16, rows: u16) -> Vec<String> {
    (0..rows)
        .map(|y| {
            (0..cols)
                .map(|x| screen.cell(x, y).symbol.as_str())
                .collect()
        })
        .collect()
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
fn is_blank(screen: &Screen, x: u16, y: u16) -> bool {
    screen.cell(x, y).symbol.as_str() == " "
}

/// The working line's row: its word's column, its spinner's column and
/// symbol, and the attention columns inside its word.
struct Working {
    row: u16,
    word: u16,
    spinner: (u16, String),
    band: Vec<u16>,
}

/// The working line on `screen`, if its word shows with a glimmer band.
fn working(screen: &Screen, cols: u16, rows: u16) -> Option<Working> {
    let text = text_rows(screen, cols, rows);
    let row = at(find_row(&text, "Working")?);
    let word = col_of(&text[row as usize], 'W')?;
    let spinner_col = word.saturating_sub(2);
    let spinner = screen.cell(spinner_col, row).symbol.clone();
    let mut band = Vec::new();
    for x in word..word.saturating_add(7) {
        if screen.cell(x, row).fg == ATTENTION {
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
fn input_row(screen: &Screen, cols: u16, rows: u16) -> Option<u16> {
    let text = text_rows(screen, cols, rows);
    (0..rows).rev().find(|y| text[*y as usize].contains('▌'))
}

/// A text row holding `needle` whose box edges both show: a frame may
/// arrive with the row but before its edges.
fn boxed_row(screen: &Screen, cols: u16, rows: u16, needle: &str) -> Option<(u16, u16)> {
    let text = text_rows(screen, cols, rows);
    let y = find_row(&text, needle)?;
    let x = col_of(&text[y], needle.chars().next()?)?;
    box_around(&text, at(y), x)?;
    Some((at(y), x))
}

/// The session box's row once its edges both show.
fn session_box(screen: &Screen, cols: u16, rows: u16) -> Option<(u16, u16)> {
    let text = text_rows(screen, cols, rows);
    let y = input_row(screen, cols, rows)?;
    let x = col_of(&text[y as usize], '▌')?;
    box_around(&text, y, x)?;
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
fn assert_fill(screen: &Screen, text: &[String], y: u16, x: u16, what: &str) {
    let (top, bottom, left, right) = box_around(text, y, x).expect(what);
    for edge in [top, bottom] {
        for x in left..=right {
            let cell = screen.cell(x, edge);
            assert_eq!(cell.fg, SURFACE, "{what} edge ({x}, {edge})");
            assert_eq!(cell.bg, Colour::Default, "{what} edge ({x}, {edge})");
        }
    }
    for row in top + 1..bottom {
        for x in left..=right {
            let cell = screen.cell(x, row);
            assert_eq!(cell.bg, SURFACE, "{what} fill ({x}, {row})");
        }
    }
}

/// Every content cell from `start` to `end` on row `y` is dim: the
/// span covers the chrome alone, never the panel beside it.
fn assert_dim_span(screen: &Screen, y: u16, start: u16, end: u16, what: &str) {
    for x in start..end {
        if !is_blank(screen, x, y) {
            assert!(screen.cell(x, y).dim, "{what} cell {x}");
        }
    }
}

/// Quits an idle run with an empty draft, as look.rs does.
fn quit(run: Run) {
    let mut run = run;
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
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
    sleep_script(&setup, 8);
    config(&setup, false);
    let mut run = Run::spawn(&setup, 160, 48, TRUECOLOUR);
    run.read_until("›");

    // Home with an empty draft: the box fills edge to edge.
    let mut screen = run.screen_until(160, 48, "home with its box", |screen| {
        boxed_row(screen, 160, 48, "? for shortcuts").is_some()
    });
    let mut text = text_rows(&screen, 160, 48);
    let (home_row, home_x) =
        boxed_row(&screen, 160, 48, "? for shortcuts").expect("the placeholder row");
    assert_fill(&screen, &text, home_row, home_x, "home empty");

    // Home with one word, then a wrapping draft: the fill follows.
    run.write(b"kq");
    screen = run.screen_until(160, 48, "home with one word", |screen| {
        boxed_row(screen, 160, 48, "› kq").is_some()
    });
    text = text_rows(&screen, 160, 48);
    let (one_row, one_x) = boxed_row(&screen, 160, 48, "› kq").expect("one word");
    assert_fill(&screen, &text, one_row, one_x, "home one word");
    run.write(b"w".repeat(150).as_slice());
    screen = run.screen_until(160, 48, "home wrapping", |screen| {
        boxed_row(screen, 160, 48, "› kq").is_some()
            && text_rows(screen, 160, 48)
                .iter()
                .filter(|row| row.contains('w'))
                .count()
                >= 2
    });
    text = text_rows(&screen, 160, 48);
    let (wrap, wrap_x) = boxed_row(&screen, 160, 48, "› kq").expect("the wrapping draft");
    assert_fill(&screen, &text, wrap, wrap_x, "home wrapping");

    // The prompt starts the turn; the draft is empty again. The
    // working line's word never arrives as one byte run under the
    // glimmer's repaints, so the frame waits below do the waiting.
    run.write(b"\r");
    screen = run.screen_until(160, 48, "the empty session box", |screen| {
        session_box(screen, 160, 48).is_some()
    });
    text = text_rows(&screen, 160, 48);
    let (empty_box, empty_x) = session_box(&screen, 160, 48).expect("the box");
    assert_fill(&screen, &text, empty_box, empty_x, "session empty");
    assert_eq!(screen.cell(empty_x + 2, empty_box).symbol.as_str(), "›");
    assert_eq!(screen.cell(empty_x + 2, empty_box).fg, INFO);

    // The call asks approval: Esc puts it aside behind its badge, and
    // the input box comes back with the turn still waiting. Steers typed
    // now queue visibly while the approval waits. The queue's marks tell
    // its rows from the draft they were typed in.
    run.screen_until(160, 48, "the approval", |screen| {
        text_rows(screen, 160, 48)
            .iter()
            .any(|row| row.contains("allow once"))
    });
    run.write(b"\x1b");
    run.screen_until(160, 48, "the box behind the badge", |screen| {
        session_box(screen, 160, 48).is_some()
    });
    run.write(b"first\r");
    run.write(b"second\r");
    screen = run.screen_until(160, 48, "both queued rows", |screen| {
        let text = text_rows(screen, 160, 48);
        text.iter().any(|row| row.contains("↳ first"))
            && text.iter().any(|row| row.contains("↳ second"))
    });
    text = text_rows(&screen, 160, 48);
    let heading = find_row(&text, "Steering, joins the turn").expect("the heading");
    assert_dim_span(
        &screen,
        at(heading),
        0,
        2 + width("• Steering, joins the turn at the next step"),
        "heading",
    );
    for needle in ["↳ first", "↳ second"] {
        let y = find_row(&text, needle).expect(needle);
        assert!(text[y].contains('✕'), "{needle}");
        assert_dim_span(&screen, at(y), 0, 2 + width(needle) + 3, needle);
    }
    let footer = find_row(&text, "click a row to edit").expect("the footer");
    assert_dim_span(
        &screen,
        at(footer),
        0,
        2 + width("⌥↑ edit · ⌥↓ next · ⌥x drop · click a row to edit, ✕ to drop"),
        "footer",
    );

    // Reopening the approval and answering it starts the sleep: the
    // panel goes for good with the turn still running.
    run.write(b"\x1ba");
    run.screen_until(160, 48, "the reopened approval", |screen| {
        text_rows(screen, 160, 48)
            .iter()
            .any(|row| row.contains("allow once"))
    });
    run.write(b"\r");
    run.screen_until(160, 48, "the tool running", |screen| {
        let text = text_rows(screen, 160, 48);
        !text.iter().any(|row| row.contains("allow once")) && working(screen, 160, 48).is_some()
    });

    // Frame A: the band shows on the running tool's line.
    screen = run.screen_until(160, 48, "the glimmer band", |screen| {
        working(screen, 160, 48).is_some()
    });
    let first = working(&screen, 160, 48).expect("frame A");
    assert!(
        SPINNER.contains(&first.spinner.1.as_str()),
        "{}",
        first.spinner.1
    );
    assert_eq!(screen.cell(first.spinner.0, first.row).fg, ATTENTION);
    // Frame B: the band shifted.
    screen = run.screen_until(160, 48, "the shifted band", |screen| {
        working(screen, 160, 48).is_some_and(|frame| frame.band != first.band)
    });
    let text_b = text_rows(&screen, 160, 48);
    let frame = working(&screen, 160, 48).expect("frame B");
    assert!(
        SPINNER.contains(&frame.spinner.1.as_str()),
        "{}",
        frame.spinner.1
    );
    assert_eq!(screen.cell(frame.spinner.0, frame.row).fg, ATTENTION);
    // The band is at most three attention cells with a bold centre; the
    // rest of the word, the elapsed time and the tail stay dim.
    assert!((1..=3).contains(&frame.band.len()), "{:?}", frame.band);
    for pair in frame.band.windows(2) {
        assert_eq!(pair[0] + 1, pair[1], "{:?}", frame.band);
    }
    let centre = frame.band[frame.band.len() / 2];
    for x in frame.word..frame.word + 7 {
        let cell = screen.cell(x, frame.row);
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
    let tail_end = text_b[frame.row as usize]
        .find("interrupt")
        .map(|at| text_b[frame.row as usize][..at].chars().count() + 9)
        .expect("the tail");
    for x in frame.word + 7..u16::try_from(tail_end).unwrap_or(u16::MAX) {
        assert!(screen.cell(x, frame.row).dim, "tail {x}");
    }

    // ⌥↑ selects the newest row: its mark in attention, its text default,
    // the box's stripe in attention, and the dim hint at its right end.
    run.write(b"\x1b[1;3A");
    run.screen_until(160, 48, "the selected row", |screen| {
        text_rows(screen, 160, 48)
            .iter()
            .any(|row| row.contains("▸ second"))
    });
    // The hint is right-aligned to the box's width, which settles as
    // the panel fills in: wait for it whole, not just started.
    screen = run.screen_until(160, 48, "the full hint", |screen| {
        text_rows(screen, 160, 48)
            .iter()
            .any(|row| row.contains("esc stops"))
    });
    text = text_rows(&screen, 160, 48);
    let selected = find_row(&text, "▸ second").expect("the selected row");
    let mark = col_of(&text[selected], '▸').expect("the mark");
    assert_eq!(screen.cell(mark, at(selected)).fg, ATTENTION);
    let text_col = col_of(&text[selected], 's').expect("the text");
    for x in text_col..text_col + 6 {
        assert_eq!(screen.cell(x, at(selected)).fg, Colour::Default, "text {x}");
    }
    let box_row = input_row(&screen, 160, 48).expect("the box");
    assert_eq!(screen.cell(0, box_row).symbol.as_str(), "▌");
    assert_eq!(screen.cell(0, box_row).fg, ATTENTION);
    let hint = find_row(&text, "editing a queued message").expect("the hint");
    assert!(
        text[hint].trim_end().ends_with("esc stops"),
        "{}",
        text[hint]
    );
    // The hint ends at the box's right end: only its own span is dim,
    // past the stripe, the prompt, the draft and the cursor.
    let hint_row = at(hint);
    let hint_start = text[hint]
        .find("editing a queued message")
        .map(|at| text[hint][..at].chars().count())
        .expect("the hint start");
    for x in hint_start..usize::from(width(&text[hint])) {
        let x = u16::try_from(x).unwrap_or(u16::MAX);
        if !is_blank(&screen, x, hint_row) {
            assert!(screen.cell(x, hint_row).dim, "hint {x}");
        }
    }

    // Esc stops the edit and ⌥x drops the queue; the session box fills
    // for one word and for a wrapping draft.
    run.write(b"\x1b");
    run.write(b"\x1bx");
    run.write(b"kq");
    screen = run.screen_until(160, 48, "one word in the session", |screen| {
        session_box(screen, 160, 48)
            .is_some_and(|(y, _)| text_rows(screen, 160, 48)[y as usize].contains("kq"))
    });
    text = text_rows(&screen, 160, 48);
    let (one_box, one_box_x) = session_box(&screen, 160, 48).expect("the box");
    assert_fill(&screen, &text, one_box, one_box_x, "session one word");
    run.write(b"w".repeat(150).as_slice());
    screen = run.screen_until(160, 48, "wrapping in the session", |screen| {
        let text = text_rows(screen, 160, 48);
        session_box(screen, 160, 48).is_some_and(|(y, _)| {
            text.iter().filter(|row| row.contains('w')).count() >= 2
                && text[y as usize].contains('w')
        })
    });
    text = text_rows(&screen, 160, 48);
    let (wrap_box, wrap_box_x) = session_box(&screen, 160, 48).expect("the box");
    assert_fill(&screen, &text, wrap_box, wrap_box_x, "session wrapping");

    // The draft clears, the turn ends on its own, and the run quits.
    run.write(&[0x7f].repeat(200));
    run.read_until("completed");
    run.read_until("finished");
    quit(run);
}

/// Under `tui.reduced_motion` the working line stays still at 160x48:
/// the spinner is ● and no cell of the line is bold, across frames.
#[test]
fn the_reduced_working_line_stays_still() {
    let setup = Setup::new();
    script(&setup, 30, 400);
    config(&setup, true);
    let mut run = Run::spawn(&setup, 160, 48, TRUECOLOUR);
    run.read_until("›");
    run.write(b"go\r");
    run.read_until("frag00");
    // Two frames pages apart: the spinner is still ● and the word plain.
    for needle in ["frag08", "frag20"] {
        run.read_until(needle);
        let fed = Screen::new(160, 48);
        let mut screen = fed;
        screen.feed(&run.output());
        let text = text_rows(&screen, 160, 48);
        let row = find_row(&text, "Working").expect("the working line");
        let word = col_of(&text[row], 'W').expect("the word");
        let spinner_col = word.saturating_sub(2);
        assert_eq!(
            screen.cell(spinner_col, at(row)).symbol.as_str(),
            "●",
            "{needle}"
        );
        assert_eq!(screen.cell(spinner_col, at(row)).fg, ATTENTION, "{needle}");
        for x in word..word + 7 {
            assert!(screen.cell(x, at(row)).dim, "{needle} word {x}");
        }
        for x in 0..width(text[row].trim_end()) {
            assert!(!screen.cell(x, at(row)).bold, "{needle} cell {x}");
        }
    }
    run.read_until("completed");
    run.read_until("finished");
    quit(run);
}

/// The input box fills edge to edge at 100x40, on home and in a session,
/// for empty, one-word and wrapping drafts.
#[test]
fn the_input_box_fills_at_100x40() {
    let setup = Setup::new();
    script(&setup, 10, 400);
    config(&setup, false);
    let mut run = Run::spawn(&setup, 100, 40, TRUECOLOUR);
    run.read_until("›");
    let mut screen = run.screen_until(100, 40, "home with its box", |screen| {
        boxed_row(screen, 100, 40, "? for shortcuts").is_some()
    });
    let mut text = text_rows(&screen, 100, 40);
    let (empty_100, empty_100_x) = boxed_row(&screen, 100, 40, "? for shortcuts").expect("empty");
    assert_fill(&screen, &text, empty_100, empty_100_x, "home empty");
    run.write(b"kq");
    screen = run.screen_until(100, 40, "home with one word", |screen| {
        boxed_row(screen, 100, 40, "› kq").is_some()
    });
    text = text_rows(&screen, 100, 40);
    let (one_100, one_100_x) = boxed_row(&screen, 100, 40, "› kq").expect("one word");
    assert_fill(&screen, &text, one_100, one_100_x, "home one word");
    run.write(b"w".repeat(150).as_slice());
    screen = run.screen_until(100, 40, "home wrapping", |screen| {
        boxed_row(screen, 100, 40, "› kq").is_some()
            && text_rows(screen, 100, 40)
                .iter()
                .filter(|row| row.contains('w'))
                .count()
                >= 2
    });
    text = text_rows(&screen, 100, 40);
    let (wrap_100, wrap_100_x) = boxed_row(&screen, 100, 40, "› kq").expect("wrapping");
    assert_fill(&screen, &text, wrap_100, wrap_100_x, "home wrapping");
    run.write(b"\r");
    screen = run.screen_until(100, 40, "the empty session box", |screen| {
        session_box(screen, 100, 40).is_some()
    });
    text = text_rows(&screen, 100, 40);
    let (empty_s, empty_s_x) = session_box(&screen, 100, 40).expect("the box");
    assert_fill(&screen, &text, empty_s, empty_s_x, "session empty");
    run.write(b"kq");
    screen = run.screen_until(100, 40, "one word in the session", |screen| {
        session_box(screen, 100, 40)
            .is_some_and(|(y, _)| text_rows(screen, 100, 40)[y as usize].contains("kq"))
    });
    text = text_rows(&screen, 100, 40);
    let (one_s, one_s_x) = session_box(&screen, 100, 40).expect("the box");
    assert_fill(&screen, &text, one_s, one_s_x, "session one word");
    run.write(b"w".repeat(150).as_slice());
    screen = run.screen_until(100, 40, "wrapping in the session", |screen| {
        session_box(screen, 100, 40).is_some_and(|(y, _)| {
            let text = text_rows(screen, 100, 40);
            text.iter().filter(|row| row.contains('w')).count() >= 2
                && text[y as usize].contains('w')
        })
    });
    text = text_rows(&screen, 100, 40);
    let (wrap_s, wrap_s_x) = session_box(&screen, 100, 40).expect("the box");
    assert_fill(&screen, &text, wrap_s, wrap_s_x, "session wrapping");
    run.write(&[0x7f].repeat(200));
    run.read_until("completed");
    run.read_until("finished");
    quit(run);
}
