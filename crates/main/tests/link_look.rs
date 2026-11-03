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
/// The dark theme's `Turn` tint lifted by a hover, in truecolour.
const LIFTED_TURN: Colour = Colour::Rgb(36, 36, 43);

/// One synchronized-output frame end: the existing output proving a
/// frame drew after the key the test just wrote (the binary brackets
/// every frame in mode 2026; `crates/main/tests/support/pty.rs` only
/// snapshots whole frames).
const FRAME_END: &[u8] = b"\x1b[?2026l";

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
    // its own stop and the walk takes it without recounting. Every key
    // waits for the frame the previous key drew: the output offset is
    // taken before the write and the frame-end marker proves a frame
    // drew after that key, so no key is written on a stale grid
    // (`docs/testing/screens.md`, "Screens"). The focused link sits on
    // `surface_raised` with the dim hint under it.
    // settled: image-line wait frame; no input since.
    let mut settled = screen.contents.clone();
    let mut from = run.output().len();
    run.write(b"\x1b[Z");
    run.wait_bytes(from, FRAME_END, "the conversation frame");
    let mut screen = run.wait_screen("the walk moves a stop", |next| next.contents != settled);
    for _ in 0..48 {
        if (0..48).any(|y| row_text(&screen, y).contains("Enter opens · y copies")) {
            break;
        }
        // settled: walk wait frame; no input since.
        settled = screen.contents.clone();
        from = run.output().len();
        run.write(b"\x1b[B");
        run.wait_bytes(from, FRAME_END, "the walk frame");
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

/// Parks the pointer on the cell at `(x, y)`: an SGR motion report
/// with no button, as the look hover test sends.
fn hover(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<35;{};{}M", x + 1, y + 1).as_bytes());
}

/// The frame's rows from `lo` to `hi` with trailing blanks trimmed,
/// then each named cell with its symbol and pen.
fn frame_text(grid: &Grid, lo: u16, hi: u16, cells: &[(&str, u16, u16)]) -> String {
    let mut out = String::new();
    for y in lo..=hi {
        out.push_str(&format!("  row {y}: {}\n", row_text(grid, y).trim_end()));
    }
    for (label, x, y) in cells {
        // settled: callers pass only grids a wait_screen returned.
        let cell = grid.cell(*x, *y);
        out.push_str(&format!(
            "  {label} ({x}, {y}) sym={:?} fg={:?} bg={:?}\n",
            cell.symbol, cell.fg, cell.bg
        ));
    }
    out
}

/// Spawns the link-look reply (the markdown link, the bare URL and
/// the table link cell, with no image read), walks focus to a link
/// with the existing test's derived walk, and returns the setup (the
/// caller holds it so the tempdir outlives the run), the run, the
/// settled focus grid, the focused row and the hint row. The walk
/// must put focus on "the spec": a wrong focus fails here by name,
/// not at the hover.
fn focus_the_spec() -> (Setup, Run, Grid, u16, u16) {
    let setup = Setup::new();
    let reply = "See [the spec](https://example.com/spec) and https://example.com/plain for details.\n\n| name | link |\n|---|---|\n| spec | [site](https://example.com/site) |";
    support::write_json(
        &setup.workspace().join("s.json"),
        &json!({"steps": [
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
    let reply = run.wait_screen("the reply", |screen| {
        (0..48).any(|y| row_text(screen, y).contains("▣ completed"))
    });
    run.turn_finished(from);

    // The existing test's derived walk: Shift+Tab reaches the
    // conversation, then down the drawn stops until the focused
    // link's hint shows, at most one Down per drawn row (the 48-row
    // grid). Every key waits for the frame the previous key drew: the
    // output offset is taken before the write and the frame-end marker
    // proves a frame drew after that key, so no key is written on a
    // stale grid (`docs/testing/screens.md`, "Screens").
    // settled: reply wait frame; turn_finished wrote nothing.
    let mut settled = reply.contents.clone();
    let mut from = run.output().len();
    run.write(b"\x1b[Z");
    run.wait_bytes(from, FRAME_END, "the conversation frame");
    let mut screen = run.wait_screen("the walk moves a stop", |next| next.contents != settled);
    for _ in 0..48 {
        if (0..48).any(|y| row_text(&screen, y).contains("Enter opens · y copies")) {
            break;
        }
        // settled: walk wait frame; no input since.
        settled = screen.contents.clone();
        from = run.output().len();
        run.write(b"\x1b[B");
        run.wait_bytes(from, FRAME_END, "the walk frame");
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
    let above = row_text(&screen, hint - 1);
    let mut focused = Vec::new();
    for needle in ["the spec", "https://example.com/plain", "site"] {
        if !above.contains(needle) {
            continue;
        }
        let fx = find_col(&screen, hint - 1, needle);
        let len = u16::try_from(needle.len()).expect("the link fits its row");
        // settled: focused-link wait frame; no input since.
        if (0..len).all(|dx| screen.cell(fx + dx, hint - 1).bg == SURFACE_RAISED) {
            focused.push(needle);
        }
    }
    assert_eq!(
        focused,
        ["the spec"],
        "focus sits on the spec, in {above:?}"
    );
    (setup, run, screen, hint - 1, hint)
}

#[test]
fn a_link_on_the_focused_row_takes_the_hover_tint() {
    let (_setup, mut run, focus, md, hint) = focus_the_spec();
    // The bare URL shares the focused row.
    assert!(
        row_text(&focus, md).contains("https://example.com/plain"),
        "the bare URL shares the focused row, in {:?}",
        row_text(&focus, md)
    );
    let ux = find_col(&focus, md, "https://example.com/plain");
    let uy = md;
    let fx = find_col(&focus, md, "the spec");
    // The table link cell's row bounds the frame dump below.
    let site = (0..48)
        .find(|y| {
            let row = row_text(&focus, *y);
            row.contains("site") && !row.contains("the spec")
        })
        .expect("the table link row");
    let hx = find_col(&focus, hint, "Enter opens");
    let lo = md.saturating_sub(2);
    let hi = (site + 2).min(47);
    let cells = [
        ("focused-first", fx, md),
        ("hovered", ux, uy),
        ("hint-first", hx, hint),
    ];
    hover(&mut run, ux, uy);
    // The reporting wait is the plan's hover wait with its own
    // deadline: on expiry its panic already names the wait and shows
    // the last grid's text, and the settled focus frame rides along,
    // so the timeout keeps the colours the panic drops.
    let hovered = run.wait_screen_reporting(
        "the hovered bare URL",
        |screen| screen.cell(ux, uy).bg == LIFTED_TURN,
        || {
            format!(
                "last settled focus frame:\n{}",
                frame_text(&focus, lo, hi, &cells)
            )
        },
    );
    // The hovered bare URL takes the lifted `Turn` tint and stays in
    // accent; the focused link keeps `surface_raised` and the hint
    // row is still shown.
    assert_fg(
        &hovered,
        uy,
        "https://example.com/plain",
        ACCENT,
        "hovered bare URL",
    );
    let len = u16::try_from("the spec".len()).expect("the link fits its row");
    for dx in 0..len {
        assert_eq!(
            hovered.cell(fx + dx, md).bg, // settled: hovered wait frame; no input since.
            SURFACE_RAISED,
            "focused cell ({}, {md})",
            fx + dx
        );
        assert_eq!(
            hovered.cell(fx + dx, md).fg, // settled: hovered wait frame; no input since.
            ACCENT,
            "focused cell ({}, {md})",
            fx + dx
        );
    }
    assert!(
        row_text(&hovered, hint).contains("Enter opens · y copies"),
        "the hint stays while hovering"
    );

    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn a_hover_below_the_focused_link_keeps_its_lift() {
    let (_setup, mut run, focus, md, hint) = focus_the_spec();
    // The table link cell's row, read from the grid after the hint
    // shows, so the row is the shifted one.
    let site = (0..48)
        .find(|y| {
            let row = row_text(&focus, *y);
            row.contains("site") && !row.contains("the spec")
        })
        .expect("the table link row");
    let sx = find_col(&focus, site, "site");
    let sy = site;
    let fx = find_col(&focus, md, "the spec");
    let hx = find_col(&focus, hint, "Enter opens");
    let lo = md.saturating_sub(2);
    let hi = (sy + 2).min(47);
    let cells = [
        ("focused-first", fx, md),
        ("hovered", sx, sy),
        ("hint-first", hx, hint),
    ];
    hover(&mut run, sx, sy);
    // The reporting wait is the plan's hover wait with its own
    // deadline: on expiry its panic already names the wait and shows
    // the last grid's text, and the settled focus frame rides along,
    // so the timeout keeps the colours the panic drops.
    let hovered = run.wait_screen_reporting(
        "the hovered site link",
        |screen| screen.cell(sx, sy).bg == LIFTED_TURN,
        || {
            format!(
                "last settled focus frame:\n{}",
                frame_text(&focus, lo, hi, &cells)
            )
        },
    );
    // The hovered table link takes the lifted `Turn` tint; the hint
    // row is still shown and the focused link keeps its lift.
    assert_eq!(
        hovered.cell(sx, sy).bg, // settled: hovered wait frame; no input since.
        LIFTED_TURN,
        "hovered cell ({sx}, {sy})"
    );
    assert!(
        row_text(&hovered, hint).contains("Enter opens · y copies"),
        "the hint stays while hovering"
    );
    let len = u16::try_from("the spec".len()).expect("the link fits its row");
    for dx in 0..len {
        assert_eq!(
            hovered.cell(fx + dx, md).bg, // settled: hovered wait frame; no input since.
            SURFACE_RAISED,
            "the focus stays while hovering: focused cell ({}, {md})",
            fx + dx
        );
        assert_eq!(
            hovered.cell(fx + dx, md).fg, // settled: hovered wait frame; no input since.
            ACCENT,
            "the focus stays while hovering: focused cell ({}, {md})",
            fx + dx
        );
    }

    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
