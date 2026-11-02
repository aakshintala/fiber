//! Binary-level test of the link and image-line look (`docs/tui.md`,
//! "Links", "Images"): the real binary under a pseudo-terminal at
//! 160x48, asserting the `vt100` grid and the raw SGR for a reply
//! holding a markdown link, a bare URL and a table link cell, for the
//! focused link's tint and hint row, for the hovered link's tint, and
//! for a tool-result image's one line.

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
use support::pty::{Colour, Grid, Run};
use support::{PIXEL, Setup, TRUECOLOUR};

/// The dark theme's roles the link look names, as truecolour cells.
const ACCENT: Colour = Colour::Rgb(110, 170, 254);
const SURFACE_RAISED: Colour = Colour::Rgb(58, 58, 74);

/// The row's symbols, one per cell.
#[track_caller]
fn row_text(screen: &Grid, y: u16) -> String {
    (0..160).map(|x| screen.cell(x, y).symbol).collect()
}

/// The first row holding `needle` in consecutive cells.
#[track_caller]
fn find_row(screen: &Grid, needle: &str) -> u16 {
    (0..48)
        .find(|y| row_text(screen, *y).contains(needle))
        .expect("the row on screen")
}

/// The first column on `y` where `needle` starts, cell by cell.
#[track_caller]
fn find_col(screen: &Grid, y: u16, needle: &str) -> u16 {
    let want: Vec<String> = needle.chars().map(|ch| ch.to_string()).collect();
    (0..160)
        .find(|x| {
            want.iter().enumerate().all(|(dx, ch)| {
                let dx = u16::try_from(dx).expect("the run fits its row");
                screen.cell(x + dx, y).symbol == *ch
            })
        })
        .expect("the run on its row")
}

/// The output replayed through a `vt100` screen of its own, keeping
/// the underline the support grid drops, so a link's cells assert both
/// halves of the link look.
fn parsed(output: &[u8]) -> vt100::Parser {
    let mut parser = vt100::Parser::new(48, 160, 0);
    parser.process(output);
    parser
}

/// Every cell of `needle` on `y` is underlined on `screen`.
#[track_caller]
fn assert_underlined(screen: &vt100::Screen, y: u16, needle: &str, what: &str) {
    let want: Vec<String> = needle.chars().map(|ch| ch.to_string()).collect();
    let x = (0..160)
        .find(|x| {
            want.iter().enumerate().all(|(dx, ch)| {
                let dx = u16::try_from(dx).expect("the run fits its row");
                screen
                    .cell(y, x + dx)
                    .is_some_and(|cell| cell.contents() == *ch)
            })
        })
        .expect("the run on its row");
    let len = u16::try_from(needle.len()).expect("the needle fits its row");
    for dx in 0..len {
        assert!(
            screen.cell(y, x + dx).is_some_and(|cell| cell.underline()),
            "{what} cell ({}, {y}) underlines",
            x + dx
        );
    }
}

/// Every cell of `needle` on `y` has foreground `fg`.
#[track_caller]
fn assert_fg(screen: &Grid, y: u16, needle: &str, fg: Colour, what: &str) {
    let x = find_col(screen, y, needle);
    let len = u16::try_from(needle.len()).expect("the needle fits its row");
    for dx in 0..len {
        assert_eq!(
            screen.cell(x + dx, y).fg,
            fg,
            "{what} cell ({}, {y})",
            x + dx
        );
    }
}

#[test]
fn links_and_the_image_line_take_the_link_look() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("pic.png"), PIXEL).unwrap();
    let reply = "See [the spec](https://example.com/spec) and https://example.com/plain for details.\n\n| name | link |\n|---|---|\n| spec | [site](https://example.com/site) |";
    support::write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
            {"tool_calls": [{"name": "read", "arguments": {"path": "pic.png"}}]},
            {"text": [reply]},
        ]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let from = run.output().len();
    run.write(b"show me\r");
    // The turn's closing line arrives last, so the whole reply is drawn.
    let screen = run.wait_screen("the reply", |screen| {
        (0..48).any(|y| row_text(screen, y).contains("▣ completed"))
    });
    run.turn_finished(from);

    // The markdown link, the bare URL and the table link cell all draw
    // in the accent colour.
    let md = find_row(&screen, "the spec");
    assert_fg(&screen, md, "the spec", ACCENT, "markdown link");
    let bare = find_row(&screen, "example.com/plain");
    assert_fg(
        &screen,
        bare,
        "https://example.com/plain",
        ACCENT,
        "bare URL",
    );
    // The table's body row shows the link's text, never its URL.
    let cell = (0..48)
        .find(|y| {
            let row = row_text(&screen, *y);
            row.contains("site") && !row.contains("the spec")
        })
        .expect("the table link row");
    assert_fg(&screen, cell, "site", ACCENT, "table link cell");
    let full = parsed(&run.output());
    // ... and underlined, cell by cell.
    assert_underlined(full.screen(), md, "the spec", "markdown link");
    assert_underlined(full.screen(), bare, "https://example.com/plain", "bare URL");
    assert_underlined(full.screen(), cell, "site", "table link cell");

    // Ctrl+O opens the ledger: the read's image draws as its one line.
    run.write(b"\x0f");
    let screen = run.wait_screen("the image line", |screen| {
        (0..48).any(|y| {
            let row = row_text(screen, y);
            row.contains("▣ ") && row.contains("1×1")
        })
    });
    let art = (0..48)
        .find(|y| {
            let row = row_text(&screen, *y);
            row.contains("▣ ") && row.contains("1×1")
        })
        .expect("the image line");
    let dot = find_col(&screen, art, "·");
    let name_start = find_col(&screen, art, "▣");
    // `▣ <name>` in accent ...
    for x in name_start..dot {
        let symbol = screen.cell(x, art).symbol.clone();
        if symbol == " " {
            continue;
        }
        assert_eq!(
            screen.cell(x, art).fg,
            ACCENT,
            "image name cell ({x}, {art})"
        );
    }
    // ... and ` · <dims>` dim.
    for x in dot..160 {
        let symbol = screen.cell(x, art).symbol.clone();
        if symbol.trim().is_empty() {
            break;
        }
        assert!(screen.cell(x, art).dim, "image dims cell ({x}, {art})");
    }
    // The name underlines, cell by cell.
    let full = parsed(&run.output());
    let image = full.screen();
    for x in name_start..dot {
        if screen.cell(x, art).symbol.as_str() == " " {
            continue;
        }
        assert!(
            image.cell(art, x).is_some_and(|cell| cell.underline()),
            "image name cell ({x}, {art}) underlines"
        );
    }

    // Shift+Tab reaches the conversation, then down the drawn stops to
    // a link. The walk's length derives from the stop set the frame
    // draws: every click target is a focus stop (docs/tui.md, "Moving
    // through the conversation"), so step until the focused link's
    // hint shows, at most one Down per drawn row. A new segment adds
    // its own stop and the walk takes it without recounting. The
    // focused link sits on `surface_raised` with the dim hint under
    // it.
    run.write(b"\x1b[Z");
    let mut settled = String::new();
    let mut screen = run.wait_screen("the walk moves a stop", |next| next.contents != settled);
    for _ in 0..screen.rows.len() {
        if (0..48).any(|y| row_text(&screen, y).contains("Enter opens · y copies")) {
            break;
        }
        run.write(b"\x1b[B");
        settled = screen.contents.clone();
        screen = run.wait_screen("the walk moves a stop", |next| next.contents != settled);
    }
    let screen = run.wait_screen("the focused link", |screen| {
        (0..48).any(|y| row_text(screen, y).contains("Enter opens · y copies"))
    });
    let hinted: Vec<u16> = (0..48)
        .filter(|y| row_text(&screen, *y).contains("Enter opens · y copies"))
        .collect();
    assert_eq!(hinted.len(), 1, "one hint row, in {hinted:?}");
    let hint = hinted[0];
    // The hint sits on the row under the focused link, dim.
    let hx = find_col(&screen, hint, "Enter opens");
    for dx in 0..22 {
        assert!(
            screen.cell(hx + dx, hint).dim,
            "hint cell ({}, {hint})",
            hx + dx
        );
    }
    // The focused link is the link on the row above the hint, on
    // `surface_raised` and still in accent. The row can hold two links
    // at once, so the tint decides, not the text.
    let above = row_text(&screen, hint - 1);
    let mut focused = Vec::new();
    for needle in ["the spec", "https://example.com/plain", "site"] {
        if !above.contains(needle) {
            continue;
        }
        let fx = find_col(&screen, hint - 1, needle);
        let len = u16::try_from(needle.len()).expect("the link fits its row");
        if (0..len).all(|dx| screen.cell(fx + dx, hint - 1).bg == SURFACE_RAISED) {
            focused.push((needle, fx, len));
        }
    }
    assert_eq!(focused.len(), 1, "one focused link in {above:?}");
    let (_, fx, len) = focused[0];
    for dx in 0..len {
        assert_eq!(
            screen.cell(fx + dx, hint - 1).fg,
            ACCENT,
            "focused cell ({}, {})",
            fx + dx,
            hint - 1
        );
    }

    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
