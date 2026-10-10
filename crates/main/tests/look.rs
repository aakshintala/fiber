//! Binary-level tests of the look (`docs/tui.md`, "Look"): the real
//! binary under a pseudo-terminal at 160x48, parsing the SGR stream for
//! the input box's striped surface in truecolour, at 256 colours, with no
//! colour, and inside tmux.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::Write;
use std::time::Duration;

use fakes::clock::FakeClock;
use support::Deadline;
use support::Setup;
use support::pty::{Colour, Reader, Run, Screen, sgr_params};

#[test]
fn the_screen_moves_and_writes() {
    let mut screen = Screen::new(10, 6);
    screen.feed(b"\x1b[2;3Hab");
    assert_eq!(screen.cell(2, 1).symbol.as_str(), "a");
    assert_eq!(screen.cell(3, 1).symbol.as_str(), "b");
}

#[test]
fn the_screen_reads_combined_sgr() {
    let mut screen = Screen::new(10, 4);
    screen.feed("\x1b[38;2;26;26;34;49m▄".as_bytes());
    let edge = screen.cell(0, 0);
    assert_eq!(edge.symbol.as_str(), "▄");
    assert_eq!(edge.fg, Colour::Rgb(26, 26, 34));
    assert_eq!(edge.bg, Colour::Default);
    screen.feed("\x1b[38;5;75;48;5;234m▌".as_bytes());
    let stripe = screen.cell(1, 0);
    assert_eq!(stripe.symbol.as_str(), "▌");
    assert_eq!(stripe.fg, Colour::Indexed(75));
    assert_eq!(stripe.bg, Colour::Indexed(234));
    screen.feed(b"\x1b[0m ");
    let reset = screen.cell(2, 0);
    assert_eq!(reset.bg, Colour::Default);
    assert_eq!(reset.fg, Colour::Default);
    assert!(!reset.dim);
    screen.feed(b"\x1b[m ");
    assert_eq!(screen.cell(3, 0).bg, Colour::Default);
    screen.feed(b"\x1b[2m ");
    assert!(screen.cell(4, 0).dim);
    screen.feed(b"\x1b[22m ");
    assert!(!screen.cell(5, 0).dim);
}

#[test]
fn the_screen_skips_other_sequences() {
    let mut screen = Screen::new(10, 4);
    screen.feed(b"\x1b[?25l");
    screen.feed(b"\x1b[>1u");
    // A bare bell writes no cell: the finished turn rings one where
    // no desktop notification goes (`docs/tui.md`, "Getting the
    // person's attention").
    screen.feed(b"\x07");
    screen.feed("\x1b]9;Fiber: x\x07".as_bytes());
    screen.feed(b"\x1b]0;t\x1b\\");
    screen.feed("\x1bP…\x1b\\".as_bytes());
    for y in 0..4 {
        for x in 0..10 {
            assert_eq!(screen.cell(x, y).symbol.as_str(), " ", "({x}, {y})");
        }
    }
}

#[test]
fn erase_display_fills_with_the_pen_background() {
    let mut screen = Screen::new(4, 3);
    screen.feed(b"ab");
    screen.feed(b"\x1b[48;5;234m\x1b[2J");
    for y in 0..3 {
        for x in 0..4 {
            assert_eq!(screen.cell(x, y).symbol.as_str(), " ", "({x}, {y})");
            assert_eq!(screen.cell(x, y).bg, Colour::Indexed(234), "({x}, {y})");
        }
    }
}

#[test]
fn sgr_params_lists_every_sequence() {
    assert_eq!(
        sgr_params(b"\x1b[38;2;26;26;34mX\x1b[m"),
        vec![vec![38, 2, 26, 26, 34], vec![0]]
    );
}

#[test]
fn a_sequence_split_across_feeds_completes() {
    let mut screen = Screen::new(10, 6);
    screen.feed(b"\x1b[2;");
    screen.feed(b"3Ha");
    assert_eq!(screen.cell(2, 1).symbol.as_str(), "a");
    let mut split = Screen::new(10, 6);
    split.feed(&[0xE2]);
    split.feed(&[0x96, 0x8C]);
    assert_eq!(split.cell(0, 0).symbol.as_str(), "▌");
}

#[test]
fn a_wide_char_misplaces_only_its_own_cell() {
    let mut screen = Screen::new(10, 4);
    screen.feed("\x1b[1;1H漢\x1b[1;3Hx".as_bytes());
    assert_eq!(screen.cell(0, 0).symbol.as_str(), "漢");
    assert_eq!(screen.cell(2, 0).symbol.as_str(), "x");
}

/// A pty pair with no child: the master the reader drains, and the
/// terminal side the test holds open and writes to.
fn pair() -> (fs::File, fs::File) {
    let terminal = support::pty::open(80, 24);
    (fs::File::from(terminal.main), terminal.terminal)
}

/// Whether `haystack` holds `needle` as bytes.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn a_reader_stops_while_the_terminal_side_stays_open() {
    let deadline = Deadline::start();
    let (main, mut terminal) = pair();
    let (reader, output, wakes) = Reader::start(main, deadline);
    // A retained terminal side holds end of file off: the reader answers
    // the stop pipe instead.
    terminal.write_all(b"ab").unwrap();
    wakes.recv_timeout(deadline.left()).unwrap();
    assert!(contains(&output.lock().unwrap(), b"ab"));
    // Still draining: only the stop signal ends it.
    assert!(!reader.ended(Duration::ZERO));
    assert!(reader.stop());
}

#[test]
fn a_reader_past_its_deadline_ends() {
    let clock = Box::leak(Box::new(FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    clock.advance(support::WAITS + Duration::from_secs(1));
    assert!(deadline.left().is_zero());
    let (main, terminal) = pair();
    let (reader, _, _) = Reader::start(main, deadline);
    // The terminal side stays open and nothing is ever written: only the
    // poll timeout ends the thread, before any stop signal.
    assert!(reader.ended(Duration::from_secs(10)));
    assert!(reader.stop());
    drop(terminal);
}

#[test]
fn a_reader_ends_at_end_of_file() {
    let deadline = Deadline::start();
    let (main, terminal) = pair();
    let (reader, _, _) = Reader::start(main, deadline);
    drop(terminal);
    // End of file ends the thread, before any stop signal.
    assert!(reader.ended(Duration::from_secs(10)));
    assert!(reader.stop());
}

/// One turn at 160x48 with `env`: the scripted provider answers "Hello.",
/// the journey types a prompt, sees the answer and quits. Returns the
/// SGR stream read into a screen, and the whole output.
fn one_turn(env: &[(&str, &str)]) -> (Screen, Vec<u8>) {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Hel", "lo."]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, env);
    run.read_until(">");
    run.write(b"say hi\r");
    run.read_until("Hel");
    run.read_until("completed");
    // The turn's attention lands after its close: quitting with the
    // teardown still in flight wedges the run, so the journey waits
    // for it (`docs/tui.md`, "Getting the person's attention").
    run.read_until("finished");
    let output = run.output();
    let mut screen = Screen::new(160, 48);
    screen.feed(&output);
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    (screen, output)
}

/// The bottom-most row holding `>` at `x` with `x >= 2`, a space before
/// it and a stripe or a space before that: the input box's prompt row.
/// Returns the stripe's cell.
fn input_row(screen: &Screen) -> (u16, u16) {
    for y in (0..48).rev() {
        for x in 2..160 {
            if screen.cell(x, y).symbol.as_str() != ">" {
                continue;
            }
            if screen.cell(x - 1, y).symbol.as_str() != " " {
                continue;
            }
            let stripe = screen.cell(x - 2, y).symbol.as_str();
            if stripe == "▌" || stripe == " " {
                return (x - 2, y);
            }
        }
    }
    panic!("no input row in the screen");
}

/// The input box's stripe cell and edge run, read once from the first
/// run: with no rail the box spans the 126-column conversation, the
/// panel grip sitting at column 126.
const BOX_LEFT: u16 = 0;
/// The edge run's exclusive end: the panel grip's column.
const BOX_RIGHT: u16 = 126;
/// The box's rows at 160x48: its ▄ edge, its text, its ▀ edge.
const EDGE_TOP: u16 = 45;
const INPUT_ROW: u16 = 46;
const EDGE_BOTTOM: u16 = 47;

/// The dark theme's surface and accent, and its 256-colour entries.
const SURFACE_RGB: Colour = Colour::Rgb(26, 26, 34);
const ACCENT_RGB: Colour = Colour::Rgb(110, 170, 254);
const SURFACE_256: Colour = Colour::Indexed(234);
const ACCENT_256: Colour = Colour::Indexed(75);

#[test]
fn truecolour_draws_the_input_box_as_a_striped_surface() {
    let (screen, _) = one_turn(&[
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "ghostty"),
    ]);
    assert_eq!(input_row(&screen), (BOX_LEFT, INPUT_ROW));
    // The box's row sits on surface, striped in accent.
    for x in BOX_LEFT..BOX_RIGHT {
        assert_eq!(
            screen.cell(x, INPUT_ROW).bg,
            SURFACE_RGB,
            "box cell ({x}, {INPUT_ROW})"
        );
    }
    let stripe = screen.cell(BOX_LEFT, INPUT_ROW);
    assert_eq!(stripe.symbol.as_str(), "▌");
    assert_eq!(stripe.fg, ACCENT_RGB);
    // Its edges: surface half blocks over the default background.
    for (y, edge) in [(EDGE_TOP, "▄"), (EDGE_BOTTOM, "▀")] {
        for x in BOX_LEFT..BOX_RIGHT {
            let cell = screen.cell(x, y);
            assert_eq!(cell.symbol.as_str(), edge, "edge cell ({x}, {y})");
            assert_eq!(cell.fg, SURFACE_RGB, "edge cell ({x}, {y})");
            assert_eq!(cell.bg, Colour::Default, "edge cell ({x}, {y})");
        }
    }
    // The conversation above is blank cells on the default background.
    let content = (1..EDGE_TOP)
        .find(|y| (BOX_LEFT..BOX_RIGHT).any(|x| screen.cell(x, *y).symbol.as_str() != " "));
    let first = content.expect("a content row above the input box");
    assert!(first > 1, "a blank conversation row above the input box");
    for y in 1..first {
        for x in BOX_LEFT..BOX_RIGHT {
            assert_eq!(screen.cell(x, y).symbol.as_str(), " ");
            assert_eq!(screen.cell(x, y).bg, Colour::Default);
        }
    }
    // The reply draws in the default colour, not dim, on a surface card.
    let mut reply = false;
    for y in 0..48 {
        for x in 0..158 {
            let word: String = (0..3)
                .map(|dx| screen.cell(x + dx, y).symbol.as_str())
                .collect();
            if word == "Hel" {
                for dx in 0..3 {
                    let cell = screen.cell(x + dx, y);
                    assert_eq!(cell.fg, Colour::Default);
                    assert!(!cell.dim);
                    assert_eq!(cell.bg, SURFACE_RGB);
                }
                reply = true;
            }
        }
    }
    assert!(reply, "the reply on a surface card");
}

#[test]
fn at_256_colours_every_colour_is_a_palette_entry() {
    let (screen, output) = one_turn(&[("TERM", "xterm-256color"), ("TERM_PROGRAM", "ghostty")]);
    for list in sgr_params(&output) {
        let mut i = 0;
        while i < list.len() {
            match list[i] {
                38 | 48 | 58 => {
                    assert_eq!(list.get(i + 1), Some(&5), "a palette entry in {list:?}");
                    i += 3;
                }
                param => {
                    assert!(
                        !matches!(param, 30..=37 | 40..=47 | 90..=97 | 100..=107),
                        "no terminal colour in {list:?}"
                    );
                    i += 1;
                }
            }
        }
    }
    // The surface takes the grey ramp and the accent its palette entry.
    for x in BOX_LEFT..BOX_RIGHT {
        let bg = screen.cell(x, INPUT_ROW).bg;
        assert_eq!(bg, SURFACE_256);
        assert!(matches!(bg, Colour::Indexed(232..=255)));
    }
    assert_eq!(screen.cell(BOX_LEFT, INPUT_ROW).fg, ACCENT_256);
    for x in BOX_LEFT..BOX_RIGHT {
        let edge = screen.cell(x, EDGE_TOP);
        assert_eq!(edge.symbol.as_str(), "▄");
        assert_eq!(edge.fg, SURFACE_256);
        assert_eq!(edge.bg, Colour::Default);
    }
}

#[test]
fn no_color_sends_no_colour_and_blank_edges_keep_their_rows() {
    let (screen, output) = one_turn(&[
        ("NO_COLOR", "1"),
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "ghostty"),
    ]);
    for list in sgr_params(&output) {
        for &param in &list {
            assert!(
                !matches!(
                    param,
                    30..=37 | 40..=47 | 90..=97 | 100..=107 | 38 | 48 | 58
                ),
                "no colour in {list:?}"
            );
        }
    }
    assert_eq!(input_row(&screen), (BOX_LEFT, INPUT_ROW));
    for y in [EDGE_TOP, EDGE_BOTTOM] {
        for x in BOX_LEFT..BOX_RIGHT {
            let cell = screen.cell(x, y);
            assert_eq!(cell.symbol.as_str(), " ", "edge cell ({x}, {y})");
            assert_eq!(cell.bg, Colour::Default, "edge cell ({x}, {y})");
        }
    }
}

/// Truecolour env for the layout runs.
const TRUECOLOUR: [(&str, &str); 3] = [
    ("TERM", "xterm-256color"),
    ("COLORTERM", "truecolor"),
    ("TERM_PROGRAM", "ghostty"),
];

const PANEL_RGB: Colour = Colour::Rgb(12, 12, 17);
const RULE_RGB: Colour = Colour::Rgb(58, 58, 74);

/// The first row holding `Hel` on screen.
fn hel_row(screen: &Screen) -> u16 {
    for y in 0..48 {
        for x in 0..157 {
            let word: String = (0..3)
                .map(|dx| screen.cell(x + dx, y).symbol.as_str())
                .collect();
            if word == "Hel" {
                return y;
            }
        }
    }
    panic!("no Hel on the screen");
}

/// Spawns 160x48 with the scripted provider, runs one turn to `finished`
/// and returns the run.
fn started_run() -> (Setup, Run) {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Hel", "lo."]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &TRUECOLOUR);
    run.read_until(">");
    run.write(b"say hi\r");
    run.read_until("Hel");
    run.read_until("completed");
    run.read_until("finished");
    (setup, run)
}

fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    run.read_until("\x1b[?25h");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn one_session_has_no_header_row_and_a_blank_column_each_side() {
    let (_setup, mut run) = started_run();
    let screen = run.screen_until(160, 48, "the conversation", |s| {
        s.cell(126, 0).bg == PANEL_RGB
            && (0..48).any(|y| {
                (0..157).any(|x| {
                    (0..3)
                        .map(|dx| s.cell(x + dx, y).symbol.as_str())
                        .collect::<String>()
                        == "Hel"
                })
            })
    });
    for x in 0..126 {
        assert_eq!(screen.cell(x, 0).symbol.as_str(), " ", "row 0 col {x}");
    }
    let y = hel_row(&screen);
    let tint = screen.cell(1, y).bg;
    assert_ne!(tint, Colour::Default, "the reply's tint");
    assert_eq!(screen.cell(0, y).bg, Colour::Default);
    assert_eq!(screen.cell(1, y).bg, tint);
    assert_eq!(screen.cell(124, y).bg, tint);
    assert_eq!(screen.cell(125, y).bg, Colour::Default);
    for y in 0..48 {
        for x in 126..160 {
            assert_ne!(
                screen.cell(x, y).bg,
                Colour::Default,
                "panel cell ({x}, {y})"
            );
        }
    }
    for y in 0..48 {
        assert_eq!(screen.cell(126, y).bg, PANEL_RGB, "panel edge row {y}");
    }
    for x in 126..160 {
        assert_eq!(screen.cell(x, 0).bg, PANEL_RGB, "panel top col {x}");
    }
    for y in 23..=25 {
        let grip = screen.cell(126, y);
        assert_eq!(grip.symbol.as_str(), "⋮", "grip row {y}");
        assert!(grip.dim, "grip row {y} dim");
    }
    quit(run);
}

#[test]
fn hovering_the_panel_edge_tints_its_column_and_brightens_the_grip() {
    let (_setup, mut run) = started_run();
    run.write(b"\x1b[<35;127;21M");
    let screen = run.screen_until(160, 48, "the active panel edge", |s| {
        (0..48).all(|y| s.cell(126, y).bg == RULE_RGB)
            && (23..=25).all(|y| {
                let grip = s.cell(126, y);
                grip.symbol.as_str() == "⋮" && grip.fg == ACCENT_RGB && grip.bold && !grip.dim
            })
    });
    for y in 0..48 {
        assert_eq!(screen.cell(126, y).bg, RULE_RGB, "edge row {y}");
    }
    for y in 23..=25 {
        let grip = screen.cell(126, y);
        assert_eq!(grip.symbol.as_str(), "⋮");
        assert_eq!(grip.fg, ACCENT_RGB);
        assert!(grip.bold);
        assert!(!grip.dim);
    }
    run.write(b"\x1b[<35;61;21M");
    let screen = run.screen_until(160, 48, "the idle panel edge", |s| {
        (0..48).all(|y| s.cell(126, y).bg == PANEL_RGB)
            && (23..=25).all(|y| {
                let grip = s.cell(126, y);
                grip.symbol.as_str() == "⋮" && grip.dim && !grip.bold
            })
    });
    for y in 0..48 {
        assert_eq!(screen.cell(126, y).bg, PANEL_RGB, "edge row {y}");
    }
    for y in 23..=25 {
        let grip = screen.cell(126, y);
        assert!(grip.dim);
        assert!(!grip.bold);
    }
    quit(run);
}

#[test]
fn two_sessions_show_the_rail_on_panel_and_hiding_it_leaves_the_grip() {
    let (_setup, mut run) = started_run();
    run.write(b"\x0e");
    run.screen_until(160, 48, "home", |s| s.cell(150, 20).bg == Colour::Default);
    run.write(b"again\r");
    run.read_until("Hel");
    run.read_until("finished");
    let screen = run.screen_until(160, 48, "the rail", |s| {
        (23..=25).all(|y| s.cell(23, y).symbol.as_str() == "⋮") && s.cell(23, 0).bg == PANEL_RGB
    });
    for y in 0..48 {
        for x in 0..24 {
            assert_ne!(
                screen.cell(x, y).bg,
                Colour::Default,
                "rail cell ({x}, {y})"
            );
        }
    }
    assert_eq!(screen.cell(23, 0).bg, PANEL_RGB);
    let y = hel_row(&screen);
    let tint = screen.cell(25, y).bg;
    assert_ne!(tint, Colour::Default);
    assert_eq!(screen.cell(24, y).bg, Colour::Default);
    assert_eq!(screen.cell(25, y).bg, tint);
    run.write(b"\x1br");
    let screen = run.screen_until(160, 48, "the grip", |s| {
        (23..=25).all(|y| s.cell(0, y).symbol.as_str() == "⋮")
    });
    for y in 23..=25 {
        assert_eq!(screen.cell(0, y).symbol.as_str(), "⋮", "grip row {y}");
    }
    let y = hel_row(&screen);
    let tint = screen.cell(2, y).bg;
    assert_ne!(tint, Colour::Default);
    assert_eq!(screen.cell(1, y).bg, Colour::Default);
    assert_eq!(screen.cell(2, y).bg, tint);
    assert_eq!(screen.cell(124, y).bg, tint);
    assert_eq!(screen.cell(125, y).bg, Colour::Default);
    quit(run);
}

#[test]
fn inside_tmux_no_stripe_draws_and_its_cell_keeps_the_tint() {
    let (screen, output) = one_turn(&[
        ("TMUX", "/tmp/tmux-501/default,1,0"),
        ("TERM", "xterm-256color"),
        ("COLORTERM", "truecolor"),
        ("TERM_PROGRAM", "ghostty"),
    ]);
    let text = String::from_utf8_lossy(&output);
    assert!(!text.contains("▌"), "no left stripe under tmux");
    assert!(!text.contains("▐"), "no right stripe under tmux");
    let stripe = screen.cell(BOX_LEFT, INPUT_ROW);
    assert_eq!(stripe.symbol.as_str(), " ");
    assert_eq!(stripe.bg, SURFACE_RGB);
    assert_eq!(screen.cell(BOX_LEFT + 2, INPUT_ROW).symbol.as_str(), ">");
}
