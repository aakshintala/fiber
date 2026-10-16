//! Binary-level test of the markdown look (`docs/tui.md`, "Look", "Markdown
//! in replies"): the real binary under a pseudo-terminal at 160x48,
//! asserting the `vt100` grid for a reply holding a heading, styled
//! paragraph text, a wrapping bullet, a code block and a table.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use support::pty::{Colour, Grid, Run, sgr_params};
use support::{Setup, TRUECOLOUR};

/// The dark theme's roles the markdown look names, as truecolour cells.
const HEADING: Colour = Colour::Rgb(255, 159, 67);
const ACCENT: Colour = Colour::Rgb(110, 170, 254);
const INFO: Colour = Colour::Rgb(125, 211, 252);
const RULE: Colour = Colour::Rgb(58, 58, 74);
const COMMENT: Colour = Colour::Rgb(122, 122, 138);
/// Code blocks sit on the `code` slab.
const CODE_BG: Colour = Colour::Rgb(24, 24, 33);

/// The row's symbols, one per cell.
#[track_caller]
fn row_text(screen: &Grid, y: u16) -> String {
    (0..160).map(|x| screen.cell(x, y).symbol).collect()
}

/// The symbols of `cols` on row `y`, cell by cell so a wide character
/// never splits a byte range.
#[track_caller]
fn symbols(screen: &Grid, y: u16, cols: std::ops::Range<u16>) -> String {
    cols.map(|x| screen.cell(x, y).symbol).collect()
}

/// The first row holding `needle` in consecutive cells.
#[track_caller]
fn find_row(screen: &Grid, needle: &str) -> u16 {
    (0..48)
        .find(|y| row_text(screen, *y).contains(needle))
        .expect("the markdown row on screen")
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
        .expect("the markdown run on its row")
}

/// Whether row `y` is an edge row of `glyph` in the code colour.
#[track_caller]
fn is_code_edge(screen: &Grid, y: u16, glyph: &str) -> bool {
    let trimmed = row_text(screen, y).trim().to_owned();
    !trimmed.is_empty()
        && trimmed.chars().all(|ch| ch.to_string() == glyph)
        && (0..160).any(|x| screen.cell(x, y).fg == CODE_BG)
}

/// The reply under test: a heading, bold and inline code, a bullet long
/// enough to wrap, the doc's three-line rust block with its comment, and
/// the doc's table.
const REPLY: &str = "# Ship it\n\nThe **lock** uses `try_lock` for safety.\n\n- this bullet item is deliberately long so that it wraps onto a second line inside the turn card however wide the conversation is drawn beside the open side panel\n\n```rust\npub fn try_lock(path: &Path) -> Result<Lock> {\n    // one holder at a time\n    let file = File::create(path)?;\n}\n```\n\n| Crate | Tests | Lines |\n|---|---|---|\n| hub | 41 | 3,120 |\n| tui | 212 | 9,804 |";

#[test]
fn markdown_reply_uses_the_markdown_look() {
    let setup = Setup::new();
    // Streamed in two chunks, split at the code block on a character
    // boundary.
    let fence = REPLY.find("```rust").expect("the code block in the reply");
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": [&REPLY[..fence], &REPLY[fence..]]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000},
            "tui": {"rail": {"shown": false}}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    run.write(b"render this\r");
    // The table's last line arrives last, so the whole reply is drawn.
    let screen = run.wait_screen("the markdown reply", |screen| {
        (0..48).any(|y| row_text(screen, y).contains("9,804"))
            && (0..48).any(|y| row_text(screen, y).contains("click to copy"))
    });
    let output = run.output();

    // The heading drops its marker, in bold in the heading colour.
    let heading = find_row(&screen, "Ship it");
    let hx = find_col(&screen, heading, "Ship it");
    // Past the grip, which takes column 0.
    let head = symbols(&screen, heading, 1..hx + 7);
    assert!(!head.contains('#'), "no heading marker in {head:?}");
    assert_eq!(head.trim(), "Ship it");
    for x in hx..hx + 7 {
        let cell = screen.cell(x, heading);
        assert_eq!(cell.fg, HEADING, "heading cell ({x}, {heading})");
        // The card's own background, whatever role paints it.
        assert_eq!(
            cell.bg,
            screen.cell(hx + 7, heading).bg,
            "heading cell ({x}, {heading})"
        );
        assert!(cell.bold, "heading cell ({x}, {heading})");
    }

    // Bold carries the text colour with no colour of its own.
    let para = find_row(&screen, "for safety");
    let bx = find_col(&screen, para, "lock");
    for dx in 0..4 {
        let cell = screen.cell(bx + dx, para);
        assert!(cell.bold, "bold cell ({}, {para})", bx + dx);
        assert_eq!(cell.fg, Colour::Default, "bold cell ({}, {para})", bx + dx);
        assert_eq!(
            cell.bg,
            screen.cell(bx + 4, para).bg,
            "bold cell ({}, {para})",
            bx + dx
        );
    }

    // Inline code is in info on the card, its backticks stripped.
    let cx = find_col(&screen, para, "try_lock");
    assert_ne!(screen.cell(cx - 1, para).symbol.as_str(), "`");
    assert_ne!(screen.cell(cx + 8, para).symbol.as_str(), "`");
    for dx in 0..8 {
        let cell = screen.cell(cx + dx, para);
        assert_eq!(cell.fg, INFO, "inline code cell ({}, {para})", cx + dx);
        // The card cell next to it on the same row: no tint of its own.
        assert_eq!(
            cell.bg,
            screen.cell(cx - 1, para).bg,
            "inline code cell ({}, {para})",
            cx + dx
        );
    }

    // The bullet is "• " in accent; its wrapped line hangs two columns in.
    let (mut bullet, mut by) = (0, 0);
    'rows: for y in 0..48 {
        for x in 0..160 {
            if screen.cell(x, y).symbol.as_str() == "•" {
                (bullet, by) = (x, y);
                break 'rows;
            }
        }
    }
    assert_eq!(screen.cell(bullet, by).fg, ACCENT);
    assert_eq!(screen.cell(bullet + 1, by).symbol.as_str(), " ");
    assert_eq!(screen.cell(bullet + 1, by).fg, ACCENT);
    assert_eq!(symbols(&screen, by + 1, bullet..bullet + 2), "  ");
    assert_ne!(screen.cell(bullet + 2, by + 1).symbol.as_str(), " ");
    assert_eq!(screen.cell(bullet + 2, by + 1).fg, Colour::Default);

    // The code slab: its cells sit on the code tint.
    let header = find_row(&screen, "click to copy");
    let mut slab = 0..0u16;
    for x in 0..160 {
        if screen.cell(x, header).bg == CODE_BG {
            slab = if slab.start == slab.end {
                x..x + 1
            } else {
                slab.start..x + 1
            };
        } else if slab.start != slab.end {
            break;
        }
    }
    assert!(slab.end - slab.start >= 100, "the code slab spans the card");
    // The language sits one column in, dim; "click to copy" ends the row, dim.
    assert_eq!(screen.cell(slab.start, header).symbol.as_str(), " ");
    let rust = screen.cell(slab.start + 1, header);
    assert_eq!(rust.symbol.as_str(), "r");
    assert_eq!(rust.fg, Colour::Default);
    assert!(rust.dim, "the language is dim");
    assert_eq!(rust.bg, CODE_BG);
    assert_eq!(
        symbols(&screen, header, slab.end - 13..slab.end),
        "click to copy"
    );
    for x in slab.end - 13..slab.end {
        let cell = screen.cell(x, header);
        assert_eq!(cell.fg, Colour::Default, "copy cell ({x}, {header})");
        assert!(cell.dim, "copy cell ({x}, {header})");
        assert_eq!(cell.bg, CODE_BG, "copy cell ({x}, {header})");
    }
    // Half-block edges in the code colour open and close the slab.
    let top = (0..header)
        .find(|y| is_code_edge(&screen, *y, "▄"))
        .expect("the slab's top edge");
    let bottom = (header + 1..48)
        .find(|y| is_code_edge(&screen, *y, "▀"))
        .expect("the slab's bottom edge");
    for (ey, glyph) in [(top, "▄"), (bottom, "▀")] {
        for x in slab.clone() {
            let cell = screen.cell(x, ey);
            assert_eq!(cell.symbol.as_str(), glyph, "edge cell ({x}, {ey})");
            assert_eq!(cell.fg, CODE_BG, "edge cell ({x}, {ey})");
            // The card's own background, read off the heading's row.
            assert_eq!(
                cell.bg,
                screen.cell(hx + 7, heading).bg,
                "edge cell ({x}, {ey})"
            );
        }
    }
    // Line numbers are right-aligned in four columns in rule, then two spaces.
    let first = find_row(&screen, "pub fn");
    assert_eq!(symbols(&screen, first, slab.start..slab.start + 4), "   1");
    for x in slab.start..slab.start + 6 {
        let cell = screen.cell(x, first);
        assert_eq!(cell.fg, RULE, "number cell ({x}, {first})");
        assert_eq!(cell.bg, CODE_BG, "number cell ({x}, {first})");
    }
    assert_eq!(
        symbols(&screen, first, slab.start + 4..slab.start + 6),
        "  "
    );
    for x in slab.clone() {
        assert_eq!(
            screen.cell(x, first).bg,
            CODE_BG,
            "code cell ({x}, {first})"
        );
    }
    // The comment is in the comment colour on the code tint; SGR 3
    // carries its italic, the reply's only italic.
    let comment = find_row(&screen, "// one holder at a time");
    let ox = find_col(&screen, comment, "// one holder at a time");
    for dx in 0..22 {
        let cell = screen.cell(ox + dx, comment);
        assert_eq!(cell.fg, COMMENT, "comment cell ({}, {comment})", ox + dx);
        assert_eq!(cell.bg, CODE_BG, "comment cell ({}, {comment})", ox + dx);
    }
    assert!(
        sgr_params(&output).iter().any(|list| list.contains(&3)),
        "an italic SGR for the comment"
    );

    // The table matches the doc's mock column for column.
    let thead = find_row(&screen, "Crate");
    let tx = find_col(&screen, thead, "Crate");
    assert_eq!(
        symbols(&screen, thead, tx..tx + 21),
        "Crate   Tests   Lines"
    );
    let trule = find_row(&screen, "─────");
    let rx = find_col(&screen, trule, "─────");
    assert_eq!(
        symbols(&screen, trule, rx..rx + 21),
        "─────   ─────   ─────"
    );
    for dx in [0, 1, 2, 3, 4, 8, 9, 10, 11, 12, 16, 17, 18, 19, 20] {
        assert_eq!(
            screen.cell(rx + dx, trule).fg,
            RULE,
            "rule cell ({}, {trule})",
            rx + dx
        );
    }
    let hub = find_row(&screen, "3,120");
    let ux = find_col(&screen, hub, "hub");
    assert_eq!(symbols(&screen, hub, ux..ux + 21), "hub        41   3,120");
    let tui = find_row(&screen, "9,804");
    let vx = find_col(&screen, tui, "tui");
    assert_eq!(symbols(&screen, tui, vx..vx + 21), "tui       212   9,804");
    // Number columns are right-aligned in the text colour.
    for (y, digits) in [(hub, "41"), (hub, "3,120"), (tui, "212"), (tui, "9,804")] {
        let dx = find_col(&screen, y, digits);
        let len = u16::try_from(digits.len()).expect("the number fits its row");
        for offset in 0..len {
            assert_eq!(
                screen.cell(dx + offset, y).fg,
                Colour::Default,
                "number cell ({}, {y})",
                dx + offset
            );
        }
    }

    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}
