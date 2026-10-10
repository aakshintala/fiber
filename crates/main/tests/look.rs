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

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use fakes::clock::FakeClock;
use support::Deadline;
use support::pty::{Colour, Reader, Screen, sgr_params};

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
    let main = rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
        .unwrap();
    rustix::pty::grantpt(&main).unwrap();
    rustix::pty::unlockpt(&main).unwrap();
    let name = rustix::pty::ptsname(&main, Vec::new()).unwrap();
    let path = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
    let terminal = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    rustix::termios::tcsetwinsize(
        &terminal,
        rustix::termios::Winsize {
            ws_col: 80,
            ws_row: 24,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    (fs::File::from(main), terminal)
}

/// Whether `haystack` holds `needle` as bytes.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
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
    // poll timeout ends the thread.
    assert!(reader.stop());
    drop(terminal);
}

#[test]
fn a_reader_ends_at_end_of_file() {
    let deadline = Deadline::start();
    let (main, terminal) = pair();
    let (reader, _, _) = Reader::start(main, deadline);
    drop(terminal);
    assert!(reader.stop());
}
