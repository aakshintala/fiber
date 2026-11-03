//! A wheel scroll over a variation-selector emoji keeps every row's
//! columns (`docs/tui.md`, "Turns"): after scrolling down to the bottom
//! again, the diff-painted grid matches a full repaint of the same frame.
//! The replay models a terminal giving each seeded cluster two cells, as
//! `unicode-width` does; the raw harness emulator gives them one, where
//! today's bytes are already right. The repro is a modelled terminal built
//! from ratatui's own code (the diff yield order and ratatui-crossterm's
//! cursor-move rule), and it is not evidence about #4058 or about any real
//! emulator.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use serde_json::json;
use support::pty::{FINISHED_TITLE, Run};
use support::{Setup, write_json};

/// The reply item number in `row`: items read `N` plus four digits, as
/// `wheel_scroll`'s `output_line` does for `T`. `None` on a row without a
/// marker.
fn item_number(row: &str) -> Option<usize> {
    let at = row.find('N')?;
    row.get(at + 1..at + 5)?.parse().ok()
}

/// `count` lowercase letters from `seed`: the letter runs that fill the
/// list items, so the row's first `N` is always the item marker.
fn letters(count: usize, seed: usize) -> String {
    (0..count)
        .map(|i| (b'a' + u8::try_from((seed + i) % 26).unwrap()) as char)
        .collect()
}

/// The final reply: 200 one-row list items, then one emoji line, then
/// `Done.` Odd items hold a warning emoji, even items letters of the same
/// width; the nine special items after the strict run seed the other
/// grapheme shapes the ticket names.
fn reply() -> String {
    let mut lines: Vec<String> = Vec::new();
    for n in 1..=190 {
        let marker = format!("N{n:04}");
        if n % 2 == 1 {
            lines.push(format!("- {marker} ⚠️ {}", letters(40, n)));
        } else {
            lines.push(format!("- {marker} {}", letters(43, n)));
        }
    }
    lines.push(format!("- N0191 ✔️ {}", letters(30, 191)));
    lines.push(format!("- N0192 ❤️ {}", letters(30, 192)));
    lines.push(format!("- N0193 界隈 {}", letters(28, 193)));
    lines.push(format!("- N0194 Ａ {}", letters(30, 194)));
    lines.push(format!("- N0195 é́ {}", letters(30, 195)));
    lines.push(format!("- N0196 👨‍👩‍👧 {}", letters(28, 196)));
    lines.push(format!("- N0197 👍🏽 {}", letters(30, 197)));
    lines.push(format!("- N0198 1️⃣ {}", letters(30, 198)));
    lines.push(format!("- N0199 ｶﾞ {}", letters(30, 199)));
    lines.push(format!("- N0200 {}", letters(43, 200)));
    lines.push("Stay ⚠️ sharp.".to_owned());
    lines.push("Done.".to_owned());
    lines.join("\n")
}

/// The wide-terminal model of the seeded clusters: each cluster the
/// harness's emulator draws in one cell becomes one double-width
/// stand-in, so the replay advances the two cells `unicode-width` gives it
/// (`unicode-width-0.2.2/src/lib.rs:59-62`). Halfwidth katakana, CJK,
/// fullwidth and combining text go through unchanged: the model and the
/// transcript list the same clusters once, here, so they cannot drift.
fn wide(bytes: &[u8]) -> Vec<u8> {
    let table = [
        ("⚠️", "亜"),
        ("✔️", "唖"),
        ("❤️", "娃"),
        ("1️⃣", "阿"),
        ("👨‍👩‍👧", "哀"),
        ("👍🏽", "愛"),
    ];
    let mut text = String::from_utf8(bytes.to_vec()).expect("terminal output is utf-8");
    for (cluster, stand_in) in table {
        text = text.replace(cluster, stand_in);
    }
    text.into_bytes()
}

/// The grid `bytes` draw on the wide-terminal model.
fn replay(bytes: &[u8]) -> vt100::Screen {
    let mut parser = vt100::Parser::new(30, 100, 0);
    parser.process(&wide(bytes));
    // settled: the caller passes bytes ending at a 2026l frame boundary its grid wait shows, no input since
    parser.screen().clone()
}

/// What the terminal shows in `cell`: a never-written cell reads as a
/// blank, the way the harness's own grid reads one (`support/pty.rs`,
/// `Grid::of`), so the repaint skipping a blank the frame holds still
/// matches a scrolled frame that once wrote it.
fn shown(cell: &vt100::Cell) -> &str {
    // settled: the caller passes a cell from bytes ending at a 2026l frame boundary its grid wait shows, no input since
    let contents = cell.contents();
    if contents.is_empty() { " " } else { contents }
}

/// Asserts the two wide-model grids show the same rows: the
/// trailing-trimmed row text first, then every cell's shown contents and
/// wide continuation, naming the first differing row, column and both rows.
fn assert_same_grid(scrolled: &vt100::Screen, repainted: &vt100::Screen) {
    // settled: the caller passes bytes ending at a 2026l frame boundary its grid wait shows, no input since
    let left: Vec<String> = scrolled.rows(0, 100).collect();
    // settled: the caller passes bytes ending at a 2026l frame boundary its grid wait shows, no input since
    let right: Vec<String> = repainted.rows(0, 100).collect();
    for y in 0..30u16 {
        let left_row = left.get(usize::from(y)).cloned().unwrap_or_default();
        let right_row = right.get(usize::from(y)).cloned().unwrap_or_default();
        // Trailing blanks are not shown: a scrolled frame that once wrote
        // a cell the repaint skips still draws the same row.
        assert_eq!(left_row.trim_end(), right_row.trim_end(), "row {y} differs");
        for x in 0..100u16 {
            // settled: the caller passes bytes ending at a 2026l frame boundary its grid wait shows, no input since
            let before = scrolled.cell(y, x).unwrap();
            // settled: the caller passes bytes ending at a 2026l frame boundary its grid wait shows, no input since
            let after = repainted.cell(y, x).unwrap();
            let same_contents = shown(before) == shown(after);
            // settled: the caller passes bytes ending at a 2026l frame boundary its grid wait shows, no input since
            let same_wide = before.is_wide_continuation() == after.is_wide_continuation();
            assert!(
                same_contents && same_wide,
                "cell ({x}, {y}) differs: scrolled {left_row:?}, repainted {right_row:?}"
            );
        }
    }
}

#[test]
fn scrolling_back_down_leaves_the_screen_a_full_repaint_would_draw() {
    let setup = Setup::new();
    fs::write(
        setup.workspace().join("notes40.txt"),
        (1..=40).map(|n| format!("L{n:04}\n")).collect::<String>(),
    )
    .unwrap();
    let step_one = "# Session notes\n\n```text\n".to_owned()
        + &"abcdefghijklmnopqrstuvwxyz".repeat(8)
        + "\n```\n\n"
        + &"A paragraph of long wrapped prose follows the code block. ".repeat(12)
        + "\n";
    write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"text": [step_one],
             "tool_calls": [{"name": "read", "arguments": {"path": "notes40.txt"}}]},
            {"text": [reply()]},
        ]}),
    );
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200},
            "tui": {"rail": {"shown": false}}}),
    );
    let mut run = Run::spawn(&setup, 100, 30, &[], &[("FIBER_TEST_FAKE_KEY", "sk-test")]);
    run.ready();
    run.write(b"go\r");
    run.wait_screen("the final reply", |grid| grid.contents.contains("Done."));
    run.wait_bytes(0, FINISHED_TITLE, "the turn's finished title");
    let end = run.wait_screen("the finished turn drawn", |grid| {
        grid.contents.contains("Done.")
            && !grid.contents.contains("Esc to interrupt")
            && item_number(&grid.rows[0]).is_some()
    });
    // settled: the finished-turn wait just above, after the turn's last output (its title), no input since
    let b = item_number(&end.rows[0]).unwrap();
    // Test precondition, not a product count: 40 ups of 3 rows need b - 120 to exist,
    // and the nine special rows after N0190 need the top at or below N0185 to stay in view.
    // reply() builds 200 items for a 30-row screen, as wheel_scroll's first >= 90 guards its 90 rows.
    assert!(b > 130, "top row holds N{b:04}, too few rows above");
    assert!(
        b <= 185,
        "top row holds N{b:04}, the screen must hold run rows above N0191"
    );
    run.write("\x1b[<64;21;11M".repeat(40).as_bytes());
    run.wait_screen(
        &format!("top row N{:04} after 40 wheel-ups of 3 rows", b - 120),
        |grid| {
            grid.rows
                .first()
                // 3 rows a step (docs/tui.md, "Turns").
                .is_some_and(|row| item_number(row) == Some(b - 120))
        },
    );
    let down39 = run.output().len();
    run.write("\x1b[<65;21;11M".repeat(39).as_bytes());
    run.wait_bytes(down39, b"\x1b[?2026l", "the wheel-downs' frame end");
    run.wait_screen(
        &format!("top row N{:04} after 39 wheel-downs", b - 3),
        |grid| {
            grid.rows
                .first()
                // 3 rows a step (docs/tui.md, "Turns").
                .is_some_and(|row| item_number(row) == Some(b - 3))
        },
    );
    let down1 = run.output().len();
    run.write(b"\x1b[<65;21;11M");
    run.wait_bytes(down1, b"\x1b[?2026l", "the wheel-down's frame end");
    run.wait_screen(&format!("the bottom again, top row N{b:04}"), |grid| {
        grid.rows
            .first()
            .is_some_and(|row| item_number(row) == Some(b))
            && grid.contents.contains("Done.")
    });
    // settled: the grid wait just above and no input since
    let scrolled = run.output().len();
    let from = run.output().len();
    run.resize(100, 30);
    let clear = run.wait_bytes(from, b"\x1b[J", "the resize's clear");
    let redraw = run.wait_bytes(clear, b"\x1b[?2026l", "the redraw's end");
    // The redraw paints the same bottom frame, so the top row and Done.
    // still show: the grid wait below sees the redraw's effect.
    run.wait_screen(&format!("the repainted bottom, top row N{b:04}"), |grid| {
        grid.rows
            .first()
            .is_some_and(|row| item_number(row) == Some(b))
            && grid.contents.contains("Done.")
    });
    // settled: the redraw's end marker above, shown by the grid wait just above, no input since
    let repainted = run.output()[..redraw].to_vec();
    // settled: scrolled bytes end at the bottom-again frame the grid wait showed; repainted bytes end at the redraw's end marker the repainted-bottom wait shows
    assert_same_grid(&replay(&run.output()[..scrolled]), &replay(&repainted));
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
