//! Binary-level tests of the look (`docs/tui.md`, "Look"): the real
//! binary under a pseudo-terminal at 160x48, asserting the `vt100` grid
//! the shared driver rebuilds: the input box's striped surface in
//! truecolour, at 256 colours, with no colour, and inside tmux.

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
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::clock::FakeClock;
use support::Deadline;
use support::Setup;
use support::pty::{
    Colour, FINISHED_TITLE, Grid, HOME_TITLE, MOTION, Reader, Run, Shared, Writer, contains,
    exact_end, query_replies, sgr_params,
};

/// Feeds `bytes` into an attached run's terminal side and returns the
/// grid once the bytes are published: the raw wait proves the reader
/// took them, and the publish snapshots the grid before waking the
/// test.
fn feed(run: &mut Run, terminal: &mut fs::File, bytes: &[u8]) -> Grid {
    let from = run.output().len();
    terminal.write_all(bytes).unwrap();
    run.wait_bytes(from, bytes, "the fed bytes");
    run.screen()
}

/// An attached run with no child over a sized pty, with the terminal
/// side the test writes to.
fn grid_run(cols: u16, rows: u16) -> (Run, fs::File) {
    let (main, terminal) = pair_sized(cols, rows);
    (Run::attach(main, cols, rows, Deadline::start()), terminal)
}

#[test]
fn the_grid_moves_and_writes() {
    let (mut run, mut terminal) = grid_run(10, 6);
    let grid = feed(&mut run, &mut terminal, b"\x1b[2;3Hab");
    assert_eq!(grid.cell(2, 1).symbol.as_str(), "a");
    assert_eq!(grid.cell(3, 1).symbol.as_str(), "b");
}

#[test]
fn the_grid_reads_combined_sgr() {
    let (mut run, mut terminal) = grid_run(10, 4);
    let grid = feed(
        &mut run,
        &mut terminal,
        "\x1b[38;2;26;26;34;49m▄".as_bytes(),
    );
    let edge = grid.cell(0, 0);
    assert_eq!(edge.symbol.as_str(), "▄");
    assert_eq!(edge.fg, Colour::Rgb(26, 26, 34));
    assert_eq!(edge.bg, Colour::Default);
    let grid = feed(
        &mut run,
        &mut terminal,
        "\x1b[38;5;75;48;5;234m▌".as_bytes(),
    );
    let stripe = grid.cell(1, 0);
    assert_eq!(stripe.symbol.as_str(), "▌");
    assert_eq!(stripe.fg, Colour::Indexed(75));
    assert_eq!(stripe.bg, Colour::Indexed(234));
    let grid = feed(&mut run, &mut terminal, b"\x1b[0m ");
    let reset = grid.cell(2, 0);
    assert_eq!(reset.bg, Colour::Default);
    assert_eq!(reset.fg, Colour::Default);
    assert!(!reset.dim);
    let grid = feed(&mut run, &mut terminal, b"\x1b[m ");
    assert_eq!(grid.cell(3, 0).bg, Colour::Default);
    let grid = feed(&mut run, &mut terminal, b"\x1b[2m ");
    assert!(grid.cell(4, 0).dim);
    let grid = feed(&mut run, &mut terminal, b"\x1b[22m ");
    assert!(!grid.cell(5, 0).dim);
}

#[test]
fn the_grid_skips_other_sequences() {
    let (mut run, mut terminal) = grid_run(10, 4);
    // A bare bell writes no cell: the finished turn rings one where
    // no desktop notification goes (`docs/tui.md`, "Getting the
    // person's attention").
    for bytes in [
        b"\x1b[?25l".as_slice(),
        b"\x1b[>1u".as_slice(),
        b"\x07".as_slice(),
        "\x1b]9;Fiber: x\x07".as_bytes(),
        b"\x1b]0;t\x1b\\".as_slice(),
        "\x1bP…\x1b\\".as_bytes(),
    ] {
        feed(&mut run, &mut terminal, bytes);
    }
    let grid = run.screen();
    for y in 0..4 {
        for x in 0..10 {
            assert_eq!(grid.cell(x, y).symbol.as_str(), " ", "({x}, {y})");
        }
    }
}

#[test]
fn erase_display_fills_with_the_pen_background() {
    let (mut run, mut terminal) = grid_run(4, 3);
    feed(&mut run, &mut terminal, b"ab");
    let grid = feed(&mut run, &mut terminal, b"\x1b[48;5;234m\x1b[2J");
    for y in 0..3 {
        for x in 0..4 {
            assert_eq!(grid.cell(x, y).symbol.as_str(), " ", "({x}, {y})");
            assert_eq!(grid.cell(x, y).bg, Colour::Indexed(234), "({x}, {y})");
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
    let (mut run, mut terminal) = grid_run(10, 6);
    let from = run.output().len();
    terminal.write_all(b"\x1b[2;").unwrap();
    run.wait_bytes(from, b"\x1b[2;", "the split sequence's first half");
    let grid = feed(&mut run, &mut terminal, b"3Ha");
    assert_eq!(grid.cell(2, 1).symbol.as_str(), "a");
}

#[test]
fn a_character_split_across_feeds_completes() {
    // A fresh grid, as above: the cursor from an earlier write would
    // take the completed char elsewhere.
    let (mut run, mut terminal) = grid_run(10, 6);
    let from = run.output().len();
    terminal.write_all(&[0xE2]).unwrap();
    run.wait_bytes(from, &[0xE2], "the split character's first byte");
    let grid = feed(&mut run, &mut terminal, &[0x96, 0x8C]);
    assert_eq!(grid.cell(0, 0).symbol.as_str(), "▌");
}

/// `vt100` gives a wide char two cells: the char and a blank
/// continuation, so `x` lands at column 2.
#[test]
fn a_wide_char_takes_two_cells() {
    let (mut run, mut terminal) = grid_run(10, 4);
    let grid = feed(&mut run, &mut terminal, "\x1b[1;1H漢\x1b[1;3Hx".as_bytes());
    assert_eq!(grid.cell(0, 0).symbol.as_str(), "漢");
    assert_eq!(grid.cell(1, 0).symbol.as_str(), " ");
    assert_eq!(grid.cell(2, 0).symbol.as_str(), "x");
}

/// A pty pair with no child: the master the reader drains, and the
/// terminal side the test holds open and writes to.
fn pair() -> (fs::File, fs::File) {
    pair_sized(80, 24)
}

/// [`pair`], sized `cols` by `rows`.
fn pair_sized(cols: u16, rows: u16) -> (fs::File, fs::File) {
    let terminal = support::pty::open(cols, rows);
    (fs::File::from(terminal.main), terminal.terminal)
}

/// Whether any screen row holds `needle` in consecutive cells.
fn shows(screen: &Grid, needle: &str) -> bool {
    (0..48).any(|y| {
        let row: String = (0..160).map(|x| screen.cell(x, y).symbol).collect();
        row.contains(needle)
    })
}

/// Drains `master` into `shared` through `writer`: the reader test's
/// own state, so it can read the output without a run.
fn drained(
    master: fs::File,
    shared: Arc<Mutex<Shared>>,
    writer: Writer,
    deadline: Deadline,
) -> (Reader, mpsc::Receiver<()>) {
    Reader::start(master, shared, writer, deadline)
}

#[test]
fn a_reader_stops_while_the_terminal_side_stays_open() {
    let deadline = Deadline::start();
    let (main, mut terminal) = pair();
    let shared = Arc::new(Mutex::new(Shared::new(80, 24)));
    let writer: Writer = Arc::new(Mutex::new(main.try_clone().unwrap()));
    let (reader, wakes) = drained(main, Arc::clone(&shared), writer, deadline);
    // A retained terminal side holds end of file off: the reader answers
    // the stop pipe instead.
    terminal.write_all(b"ab").unwrap();
    wakes.recv_timeout(deadline.left()).unwrap();
    assert!(contains(&shared.lock().unwrap().output(), b"ab"));
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
    let shared = Arc::new(Mutex::new(Shared::new(80, 24)));
    let writer: Writer = Arc::new(Mutex::new(main.try_clone().unwrap()));
    let (reader, _) = drained(main, shared, writer, deadline);
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
    let shared = Arc::new(Mutex::new(Shared::new(80, 24)));
    let writer: Writer = Arc::new(Mutex::new(main.try_clone().unwrap()));
    let (reader, _) = drained(main, shared, writer, deadline);
    drop(terminal);
    // End of file ends the thread, before any stop signal.
    assert!(reader.ended(Duration::from_secs(10)));
    assert!(reader.stop());
}

#[test]
fn exact_end_matches_only_at_or_after_from() {
    assert_eq!(exact_end(b"xxabyy", 2, b"ab"), Some(4));
    assert_eq!(exact_end(b"xxabyy", 3, b"ab"), None);
    assert_eq!(exact_end(b"xxab", 0, b"ab"), Some(4));
    assert_eq!(exact_end(b"ab", 2, b"ab"), None);
    assert_eq!(exact_end(b"ab", 3, b"ab"), None);
}

#[test]
fn query_replies_answers_in_stream_order_and_keeps_its_tail() {
    // Two queries in one chunk are answered in stream order.
    let mut pending = b"\x1b[c\x1b[?u".to_vec();
    assert_eq!(query_replies(&mut pending), b"\x1b[?0c\x1b[?1u");
    assert!(pending.is_empty());
    // A query split across two reads is answered once whole.
    let mut pending = b"\x1b]11;".to_vec();
    assert!(query_replies(&mut pending).is_empty());
    pending.extend_from_slice(b"?\x1b\\");
    assert_eq!(
        query_replies(&mut pending),
        b"\x1b]11;rgb:0000/0000/0000\x1b\\"
    );
    assert!(pending.is_empty());
    // A 16-byte non-query tail is kept whole for the next read, and a
    // 17-byte one is cut to its last 16.
    let mut pending = vec![b'x'; 16];
    assert!(query_replies(&mut pending).is_empty());
    assert_eq!(pending.len(), 16);
    let mut pending = vec![b'x'; 17];
    assert!(query_replies(&mut pending).is_empty());
    assert_eq!(pending, vec![b'x'; 16]);
}

#[test]
fn a_pending_resize_lands_before_its_chunk_parses() {
    let (mut run, mut terminal) = grid_run(120, 32);
    run.resize(40, 10);
    // Fifty cells at 40 columns wrap onto two rows; at 120 they would
    // sit on one. The cursor tells which size parsed the chunk.
    let from = run.output().len();
    terminal.write_all(&[b'x'; 50]).unwrap();
    run.wait_bytes(from, &[b'x'; 50], "the fifty cells");
    let grid = run.screen();
    assert_eq!(grid.rows.len(), 10);
    assert_eq!(grid.cursor, (1, 10));
}

/// A chunk is published only after its replies are written: the pause
/// point is the `Writer` lock, which the test owns (`docs/testing.md`,
/// "Waits and timeouts").
#[test]
fn a_query_is_not_published_before_its_reply_is_written() {
    let deadline = Deadline::start();
    let (main, mut terminal) = pair_sized(80, 24);
    let run = Run::attach(main, 80, 24, deadline);
    // Slave reads return without a newline only outside canonical
    // mode. Echo stays on so the reader sees the query, and ECHOCTL
    // goes off so the echo is raw bytes.
    let mut attrs = rustix::termios::tcgetattr(&terminal).unwrap();
    attrs
        .local_modes
        .remove(rustix::termios::LocalModes::ICANON | rustix::termios::LocalModes::ECHOCTL);
    rustix::termios::tcsetattr(&terminal, rustix::termios::OptionalActions::Now, &attrs).unwrap();
    // The test owns the pause point: the reader's reply write blocks
    // on it.
    let held = run.writer();
    let guard = held.lock().unwrap();
    terminal.write_all(b"\x1b[?u").unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut run = run;
        let end = run.wait_bytes(0, b"\x1b[?u", "the query");
        done.send((run, end)).unwrap();
    });
    // The wait has no probe interval of its own, waking per chunk, so
    // 100 ms bounds the proof that it has not answered yet.
    assert!(
        finished.recv_timeout(Duration::from_millis(100)).is_err(),
        "the query waited for its reply"
    );
    drop(guard);
    let (run, _) = finished
        .recv_timeout(deadline.left())
        .expect("the run back after the release");
    // The reply was on the master before the query was published.
    let reply = support::bounded(deadline, "the reply on the master", move || {
        use std::io::Read;
        let mut reply = [0u8; 5];
        terminal.read_exact(&mut reply).unwrap();
        reply
    });
    assert_eq!(&reply, b"\x1b[?1u");
    drop(run);
}

#[test]
#[should_panic(expected = "the terminal ended")]
fn a_failed_reply_never_publishes_its_query() {
    let deadline = Deadline::start();
    let (main, mut terminal) = pair_sized(80, 24);
    let mut run = Run::attach(main, 80, 24, deadline);
    // A read-only file, so the reply write fails: the chunk is never
    // published and the wait fails on the terminal ending.
    *run.writer().lock().unwrap() = fs::File::open("/dev/null").unwrap();
    terminal.write_all(b"\x1b[?u").unwrap();
    run.wait_bytes(0, b"\x1b[?u", "the query");
}

#[test]
fn grid_cell_reads_the_last_cell() {
    let (run, _terminal) = grid_run(10, 6);
    assert_eq!(run.screen().cell(9, 5).symbol.as_str(), " ");
}

#[test]
#[should_panic]
fn grid_cell_past_the_edges_panics() {
    let (run, _terminal) = grid_run(10, 6);
    let _ = run.screen().cell(10, 0);
}

/// The journey's waits finish when the finished title arrives before the
/// completed paint: no wait depends on the order of the two markers
/// (see #1775).
#[test]
fn the_finished_title_before_the_completed_paint_still_finishes_the_turn() {
    let deadline = Deadline::start();
    let (main, mut terminal) = pair_sized(160, 48);
    let mut run = Run::attach(main, 160, 48, deadline);
    terminal.write_all(FINISHED_TITLE).unwrap();
    terminal
        .write_all("\x1b[1;1HHel\x1b[2;1H▣ completed".as_bytes())
        .unwrap();
    drop(terminal);
    run.turn_finished(0);
}

/// The same waits finish when the pair arrives in the other order.
#[test]
fn the_completed_paint_before_the_finished_title_still_finishes_the_turn() {
    let deadline = Deadline::start();
    let (main, mut terminal) = pair_sized(160, 48);
    let mut run = Run::attach(main, 160, 48, deadline);
    terminal
        .write_all("\x1b[1;1HHel\x1b[2;1H\u{25a3} completed".as_bytes())
        .unwrap();
    terminal.write_all(FINISHED_TITLE).unwrap();
    drop(terminal);
    run.turn_finished(0);
}

/// Whether the grid shows a finished turn with everything `one_turn`'s
/// assertions read: the `completed` status, the prompt row at the box's
/// left, the box rows uniform, and the reply's `Hel` on its card's
/// tint. A partial redraw can't satisfy it: a `vt100` cell takes its
/// pen with its symbol, so these are every property the assertions
/// read.
fn turn_drawn(grid: &Grid) -> bool {
    if !grid.contents.contains("completed") {
        return false;
    }
    let stripe = grid.cell(BOX_LEFT, INPUT_ROW).symbol;
    if stripe != "▌" && stripe != " " {
        return false;
    }
    if grid.cell(BOX_LEFT + 1, INPUT_ROW).symbol != " " {
        return false;
    }
    if grid.cell(BOX_LEFT + 2, INPUT_ROW).symbol != "›" {
        return false;
    }
    for y in [EDGE_TOP, INPUT_ROW, EDGE_BOTTOM] {
        let first = grid.cell(BOX_LEFT, y);
        for x in BOX_LEFT..BOX_RIGHT {
            let cell = grid.cell(x, y);
            if cell.bg != first.bg {
                return false;
            }
            if y != INPUT_ROW && (cell.symbol != first.symbol || cell.fg != first.fg) {
                return false;
            }
        }
    }
    (0..48).any(|y| {
        (0..157).any(|x| {
            grid.cell(x, y).symbol == "H"
                && grid.cell(x + 1, y).symbol == "e"
                && grid.cell(x + 2, y).symbol == "l"
                && (0..3).all(|dx| {
                    let cell = grid.cell(x + dx, y);
                    cell.fg == Colour::Default && !cell.dim && cell.bg == grid.cell(x + 3, y).bg
                })
        })
    })
}

/// One turn at 160x48 with `env`: the scripted provider answers "Hello.",
/// the journey types a prompt, sees the answer and quits. Returns the
/// drawn grid and the whole output.
fn one_turn(env: &[(&str, &str)]) -> (Grid, Vec<u8>) {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Hel", "lo."]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 1000}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], env);
    // The end of the first frame proves the input reader runs before
    // the prompt goes out; the drawn turn proves the quit lands
    // anywhere, since quitting is taken in any state.
    run.ready();
    run.write(b"say hi\r");
    let screen = run.wait_screen("the drawn turn", turn_drawn);
    let output = run.output();
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    (screen, output)
}

/// The bottom-most row holding `>` at `x` with `x >= 2`, a space before
/// it and a stripe or a space before that: the input box's prompt row.
/// Returns the stripe's cell.
fn input_row(screen: &Grid) -> (u16, u16) {
    for y in (0..48).rev() {
        for x in 2..160 {
            if screen.cell(x, y).symbol.as_str() != "›" {
                continue;
            }
            if screen.cell(x - 1, y).symbol.as_str() != " " {
                continue;
            }
            let stripe = screen.cell(x - 2, y).symbol;
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
            let word: String = (0..3).map(|dx| screen.cell(x + dx, y).symbol).collect();
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

#[derive(Clone, Copy, Debug)]
enum SettledScreen {
    Conversation,
    ActivePanelEdge,
    IdlePanelEdge,
    Home,
    Rail,
    Grip,
}

/// Whether the screen has every property its layout assertion uses.
fn settled(screen: &Grid, layout: SettledScreen) -> bool {
    let reply = (0..48).find_map(|y| {
        (0..157).find_map(|x| {
            let word: String = (0..3).map(|dx| screen.cell(x + dx, y).symbol).collect();
            (word == "Hel").then_some((x, y))
        })
    });
    let reply_card = |left, right, left_gutter, right_gutter| {
        let Some((reply_x, y)) = reply else {
            return false;
        };
        let tint = screen.cell(left, y).bg;
        tint != Colour::Default
            && (left..=right).all(|x| screen.cell(x, y).bg == tint)
            && [left_gutter, right_gutter].into_iter().all(|x| {
                let gutter = screen.cell(x, y);
                gutter.symbol == " " && gutter.bg == Colour::Default
            })
            && (0..3).all(|dx| {
                let cell = screen.cell(reply_x + dx, y);
                cell.fg == Colour::Default && !cell.dim && cell.bg == tint
            })
    };
    let panel = |left| {
        (0..48).all(|y| {
            (left..160).all(|x| screen.cell(x, y).bg != Colour::Default)
                && screen.cell(left, y).bg == PANEL_RGB
        }) && (left..160).all(|x| screen.cell(x, 0).bg == PANEL_RGB)
    };
    match layout {
        SettledScreen::Conversation => {
            (0..126).all(|x| screen.cell(x, 0).symbol == " ")
                && reply_card(1, 124, 0, 125)
                && panel(126)
                && (23..=25).all(|y| {
                    let grip = screen.cell(126, y);
                    grip.symbol == "⋮" && grip.dim
                })
        }
        SettledScreen::ActivePanelEdge => {
            (0..48).all(|y| screen.cell(126, y).bg == RULE_RGB)
                && (23..=25).all(|y| {
                    let grip = screen.cell(126, y);
                    grip.symbol == "⋮" && grip.fg == ACCENT_RGB && grip.bold && !grip.dim
                })
        }
        SettledScreen::IdlePanelEdge => {
            (0..48).all(|y| screen.cell(126, y).bg == PANEL_RGB)
                && (23..=25).all(|y| {
                    let grip = screen.cell(126, y);
                    grip.symbol == "⋮" && grip.dim && !grip.bold
                })
        }
        SettledScreen::Home => screen.cell(150, 20).bg == Colour::Default,
        SettledScreen::Rail => {
            (0..48).all(|y| (0..24).all(|x| screen.cell(x, y).bg != Colour::Default))
                && screen.cell(23, 0).bg == PANEL_RGB
                && panel(126)
                && (23..=25).all(|y| screen.cell(23, y).symbol == "⋮")
                && reply_card(25, 124, 24, 125)
        }
        SettledScreen::Grip => {
            (23..=25).all(|y| screen.cell(0, y).symbol == "⋮") && reply_card(2, 124, 1, 125)
        }
    }
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
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    // The end of the first frame proves the input reader runs before
    // the prompt goes out; the reply stays drawn, so the grid check
    // before the finished title is order-free.
    run.ready();
    let from = run.output().len();
    run.write(b"say hi\r");
    run.wait_screen("the reply", |screen| shows(screen, "Hel"));
    run.turn_finished(from);
    (setup, run)
}

fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn one_session_has_no_header_row_and_a_blank_column_each_side() {
    let (_setup, mut run) = started_run();
    let screen = run.wait_screen("the conversation", |s| {
        settled(s, SettledScreen::Conversation)
    });
    assert!(
        settled(&screen, SettledScreen::Conversation),
        "incomplete conversation screen"
    );
    quit(run);
}

#[test]
fn hovering_the_panel_edge_tints_its_column_and_brightens_the_grip() {
    let (_setup, mut run) = started_run();
    // The motion enable proves the terminal takes mouse reports; the
    // finished turn the starter waited proves the session is idle.
    run.wait_bytes(0, MOTION, "mouse motion enabled");
    run.write(b"\x1b[<35;127;21M");
    let screen = run.wait_screen("the active panel edge", |s| {
        settled(s, SettledScreen::ActivePanelEdge)
    });
    assert!(
        settled(&screen, SettledScreen::ActivePanelEdge),
        "incomplete active panel edge"
    );
    run.write(b"\x1b[<35;61;21M");
    let screen = run.wait_screen("the idle panel edge", |s| {
        settled(s, SettledScreen::IdlePanelEdge)
    });
    assert!(
        settled(&screen, SettledScreen::IdlePanelEdge),
        "incomplete idle panel edge"
    );
    quit(run);
}

#[test]
fn two_sessions_show_the_rail_on_panel_and_hiding_it_leaves_the_grip() {
    let (_setup, mut run) = started_run();
    // The finished turn the starter waited proves the session is idle
    // before the home key goes out.
    let home_from = run.output().len();
    run.write(b"\x0e");
    let screen = run.wait_screen("home", |s| settled(s, SettledScreen::Home));
    assert!(
        settled(&screen, SettledScreen::Home),
        "incomplete home screen"
    );
    // The home title from before the home key proves the home drew
    // before the next prompt goes out.
    run.wait_bytes(home_from, HOME_TITLE, "the home title");
    let from = run.output().len();
    run.write(b"again\r");
    // The reply stays drawn, so the grid check before the finished
    // title is order-free; the rail holds the reply card.
    run.wait_screen("the reply", |screen| shows(screen, "Hel"));
    run.turn_finished(from);
    let screen = run.wait_screen("the rail", |s| settled(s, SettledScreen::Rail));
    assert!(
        settled(&screen, SettledScreen::Rail),
        "incomplete rail screen"
    );
    // The finished title from before the second prompt proves the turn
    // closed before the rail key goes out.
    run.wait_bytes(from, FINISHED_TITLE, "the finished second turn");
    run.write(b"\x1br");
    let screen = run.wait_screen("the grip", |s| settled(s, SettledScreen::Grip));
    assert!(
        settled(&screen, SettledScreen::Grip),
        "incomplete grip screen"
    );
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
    assert_eq!(screen.cell(BOX_LEFT + 2, INPUT_ROW).symbol.as_str(), "›");
}
