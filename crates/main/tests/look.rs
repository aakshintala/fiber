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
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::clock::FakeClock;
use support::pty::{
    Colour, FINISHED_TITLE, Grid, HOME_TITLE, MOTION, Reader, Run, Shared, WAITING_TITLE, Writer,
    contains, exact_end, hub_gone, hub_pids, list_processes, query_replies, sgr_params,
    until_hub_exits, until_hub_exits_with,
};
use support::{Deadline, KillGroup, Setup, TRUECOLOUR};

/// Feeds `bytes` into an attached run's terminal side and returns the
/// grid once the bytes are published: the raw wait proves the reader
/// took them, and the publish snapshots the grid before waking the
/// test.
#[track_caller]
fn feed(run: &mut Run, terminal: &mut fs::File, bytes: &[u8]) -> Grid {
    let from = run.output().len();
    terminal.write_all(bytes).unwrap();
    run.wait_bytes(from, bytes, "the fed bytes");
    run.screen()
}

/// An attached run with no child over a sized pty, with the terminal
/// side the test writes to.
#[track_caller]
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
#[track_caller]
fn pair() -> (fs::File, fs::File) {
    pair_sized(80, 24)
}

/// [`pair`], sized `cols` by `rows`.
#[track_caller]
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
    deadline.recv(&wakes).unwrap();
    assert!(contains(&shared.lock().unwrap().output(), b"ab"));
    // Still draining: only the stop signal ends it.
    assert!(!reader.ended(Duration::ZERO));
    assert!(reader.stop());
}

#[test]
fn a_reader_past_its_deadline_ends() {
    let clock = Box::leak(Box::new(FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
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
        Deadline::after(Duration::from_millis(100))
            .recv(&finished)
            .is_err(),
        "the query waited for its reply"
    );
    drop(guard);
    let (run, _) = deadline
        .recv(&finished)
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

/// `hub_pids` lists only this run's hub from a process table: a row
/// matches when its command holds `hub serve` and its whitespace-split
/// words hold the exact token `FIBER_HOME=<home>`, the same token rule
/// `scripts/leak-scan`'s `holds_token` uses.
#[test]
fn hub_pids_lists_only_this_runs_hub() {
    let home = "/tmp/fiber-hub-home";
    let table = [
        "12345 sh -c : hub serve; rm -f /s FIBER_HOME=/tmp/fiber-hub-home",
        "12346 hub serve FIBER_HOME=/tmp/fiber-hub-home",
        "12347 sh -c : rm -f /s FIBER_HOME=/tmp/fiber-hub-home",
        "12348 sh -c : hub serve; rm -f /s FIBER_HOME=/tmp/fiber-hub-home-x",
        "12349 hub serve FIBER_HOME=/tmp/other-home",
        "pid sh -c : hub serve; rm -f /s FIBER_HOME=/tmp/fiber-hub-home",
        // The lister's own row carries no `FIBER_HOME` token: it runs
        // with a cleared environment holding only `PATH`, so it can
        // never match itself.
        "12350 ps -E -ww -o pid=,command= -U tester",
    ]
    .join("\n");
    assert_eq!(hub_pids(&table, home), vec![12345, 12346]);
}

/// `hub_gone` is false while the socket is gone but the hub is alive:
/// the pause-point repro without threads, pinning every condition (the
/// socket probe, the empty-home shortcut, the listing).
#[test]
fn a_hub_alive_without_its_socket_is_not_gone() {
    let dir = fakes::TempDir::new("hub-gone");
    let socket = dir.path().join("hub");
    fs::write(&socket, b"").unwrap();
    let home = "/tmp/fiber-hub-home";
    let hub_row = "4242 python3 -c 'import os, sys  # hub serve' FIBER_HOME=/tmp/fiber-hub-home";
    // The socket gone while the hub is listed: not gone.
    assert!(!hub_gone(&dir.path().join("missing"), home, &mut || {
        hub_row.to_owned()
    },));
    // The socket gone and nothing listed: gone.
    assert!(hub_gone(
        &dir.path().join("missing"),
        home,
        &mut String::new,
    ));
    // The socket present and nothing listed: not gone.
    assert!(!hub_gone(&socket, home, &mut String::new));
    // An attached run lists nothing: the socket alone decides, and the
    // lister never runs.
    let mut listed = false;
    let mut never = || {
        listed = true;
        String::new()
    };
    assert!(hub_gone(&dir.path().join("missing"), "", &mut never));
    assert!(!listed, "an attached run lists no processes");
}

/// `until_hub_exits_with` lists once, after the socket goes: while the
/// socket exists the wait makes paced socket polls that start no process,
/// so the lister never runs; one listing showing no hub then ends it.
#[test]
fn until_hub_exits_with_lists_once_after_the_socket_goes() {
    let deadline = Deadline::start();
    let dir = fakes::TempDir::new("hub-exits-once");
    let socket = dir.path().join("hub");
    fs::write(&socket, b"").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (done, finished) = mpsc::channel();
    thread::spawn({
        let socket = socket.clone();
        let calls = Arc::clone(&calls);
        move || {
            let mut list = || {
                calls.fetch_add(1, Ordering::SeqCst);
                String::new()
            };
            until_hub_exits_with(
                deadline,
                &socket,
                "/tmp/fiber-hub-home",
                "the fake hub to exit",
                &mut list,
            );
            done.send(()).unwrap_or(());
        }
    });
    // The wait holds while the socket exists: several poll intervals pass
    // with no answer. A wait that answered here would end before the
    // socket goes.
    assert!(
        Deadline::after(Duration::from_millis(50))
            .recv(&finished)
            .is_err(),
        "the wait holds while the socket exists"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "no listing runs while the socket exists"
    );
    fs::remove_file(&socket).unwrap();
    deadline
        .recv(&finished)
        .expect("the wait ends after the socket goes");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one listing runs after the socket goes"
    );
}

/// An attached run (`home` empty) waits on the socket alone: the lister
/// never runs, however many socket polls pass before the socket goes.
#[test]
fn until_hub_exits_with_waits_on_the_socket_alone_when_attached() {
    let deadline = Deadline::start();
    let dir = fakes::TempDir::new("hub-attached");
    let socket = dir.path().join("hub");
    fs::write(&socket, b"").unwrap();
    let listed = Arc::new(AtomicBool::new(false));
    let (done, finished) = mpsc::channel();
    thread::spawn({
        let socket = socket.clone();
        let listed = Arc::clone(&listed);
        move || {
            let mut never = || {
                listed.store(true, Ordering::SeqCst);
                String::new()
            };
            until_hub_exits_with(
                deadline,
                &socket,
                "",
                "the attached hub to exit",
                &mut never,
            );
            done.send(()).unwrap_or(());
        }
    });
    assert!(
        Deadline::after(Duration::from_millis(50))
            .recv(&finished)
            .is_err(),
        "the attached wait holds while the socket exists"
    );
    assert!(
        !listed.load(Ordering::SeqCst),
        "an attached run lists no processes"
    );
    fs::remove_file(&socket).unwrap();
    deadline
        .recv(&finished)
        .expect("the attached wait ends after the socket goes");
    assert!(
        !listed.load(Ordering::SeqCst),
        "an attached run lists no processes"
    );
}

/// `until_hub_exits_with` returns once the listed hub pid exits: while the
/// pid lives the wait holds, and releasing its stdin ends the read at end
/// of file, which the reap below proves before the wait answers.
#[test]
fn until_hub_exits_with_returns_once_the_hub_pid_exits() {
    let deadline = Deadline::start();
    let dir = fakes::TempDir::new("hub-pid-exit");
    // The socket is already gone: the wait goes straight to the pid phase.
    let socket = dir.path().join("missing");
    let home = "/tmp/fiber-hub-home";
    // A live child the fake table names as this run's hub, blocked reading
    // the stdin the test holds open.
    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = fakes::Watchdog::group(group);
    let pid = child.id();
    let stdin = child.stdin.take().unwrap();
    let table = format!("{pid} hub serve FIBER_HOME={home}");
    let calls = Arc::new(AtomicUsize::new(0));
    let (done, finished) = mpsc::channel();
    thread::spawn({
        let calls = Arc::clone(&calls);
        move || {
            until_hub_exits_with(deadline, &socket, home, "the fake hub to exit", &mut || {
                calls.fetch_add(1, Ordering::SeqCst);
                table.clone()
            });
            done.send(()).unwrap_or(());
        }
    });
    // While the hub lives the wait holds: no answer within several of its
    // own poll intervals.
    assert!(
        Deadline::after(Duration::from_millis(50))
            .recv(&finished)
            .is_err(),
        "the wait holds while the hub pid lives"
    );
    // Releasing stdin ends the read at end of file, and reaping proves
    // the exit, both bounded by the deadline.
    drop(stdin);
    let (reaped, exited) = mpsc::channel();
    thread::spawn(move || reaped.send(child.wait()).unwrap_or(()));
    let status = deadline
        .recv(&exited)
        .expect("the fake hub exits after the release")
        .expect("the fake hub reaps");
    assert!(
        status.success(),
        "the fake hub exits cleanly after the release"
    );
    deadline
        .recv(&finished)
        .expect("the wait ends after the hub pid exits");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "one listing runs for the pid wait, however many polls it makes"
    );
    // The hub exiting leaves nothing behind: kill the group, check it
    // emptied, and stand the watchdog down.
    match fakes::kill_group(group, "KILL") {
        Ok(_) | Err(_) => {}
    }
    assert!(
        fakes::group_empties(group, deadline.cleanup_phase()),
        "the fake hub's group emptied"
    );
    std::mem::forget(guard);
    watchdog.stand_down(deadline.cleanup_phase());
}

/// `until_hub_exits_with` panics naming `what` when the hub pid outlives
/// the deadline: the pid phase keeps only what the socket phase left, and
/// the live pid here leaves nothing.
#[test]
#[should_panic(expected = "waited until the deadline for the hub that never exits")]
fn until_hub_exits_with_panics_naming_what_when_the_hub_outlives_the_deadline() {
    let clock = Box::leak(Box::new(FakeClock::new()));
    let deadline = Deadline::on(&**clock);
    // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
    clock.advance(
        support::WAITS
            .checked_sub(Duration::from_millis(100))
            .unwrap(),
    );
    let dir = fakes::TempDir::new("hub-never-exits");
    // The test's own process is always alive, so the pid wait can never
    // answer: no child is started for this timeout.
    let live = std::process::id();
    let table = format!("{live} hub serve FIBER_HOME=/tmp/fiber-hub-home");
    until_hub_exits_with(
        deadline,
        &dir.path().join("missing"),
        "/tmp/fiber-hub-home",
        "the hub that never exits",
        &mut || table.clone(),
    );
}

/// `Run::wait`'s hub wait must outlive the socket: the fake hub removes
/// its socket, then blocks reading its stdin, so the socket's absence
/// alone cannot prove the hub is gone (`docs/testing.md`, "Waits and
/// timeouts": wait for the signal that proves the operation, here the
/// hub process gone).
#[test]
fn the_hub_wait_outlives_the_sockets_absence() {
    let deadline = Deadline::start();
    let dir = fakes::TempDir::new("hub-exit");
    let socket = dir.path().join("hub");
    fs::write(&socket, b"").unwrap();
    let mut child = Command::new("python3")
        .arg("-c")
        .arg("import os, sys; os.unlink(sys.argv[1]); sys.stdin.read()  # hub serve")
        .arg(&socket)
        .env_clear()
        .envs(fakes::check_run())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("FIBER_HOME", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    let group = child.id();
    let watchdog = fakes::Watchdog::group(group);
    let stdin = child.stdin.take().unwrap();
    // The pause point: the fake removed its socket and now blocks in
    // `sys.stdin.read()`. python3, not sh: macOS `ps -E` shows the
    // environment of python but not of sh or sleep
    // (`scripts/test-leak-scan`: "only a python leaker proves the scan
    // names it"), and the wait lists hubs by their `FIBER_HOME`
    // environment token.
    while socket.exists() {
        if deadline.left().is_zero() {
            panic!("waited until the deadline for the fake hub to remove its socket");
        }
        thread::yield_now();
    }
    // The old condition is already true while the hub lives.
    assert!(!socket.exists(), "the socket is gone while the hub lives");
    let home = dir.path().to_string_lossy().into_owned();
    // The hub is listed while paused, so it is not gone: a socket-only
    // wait would return here.
    assert!(
        !hub_gone(&socket, &home, &mut || list_processes(deadline.left())),
        "the hub is not gone while it lives without its socket"
    );
    // Releasing stdin ends the read at end of file, and reaping proves
    // the exit, both bounded by the deadline.
    drop(stdin);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait()).unwrap());
    let status = deadline
        .recv(&finished)
        .expect("the fake hub back after the release")
        .expect("the fake hub reaped");
    assert!(
        status.success(),
        "the fake hub exits cleanly after the release"
    );
    assert!(
        hub_gone(&socket, &home, &mut || list_processes(deadline.left())),
        "the hub is gone after its exit"
    );
    until_hub_exits(deadline, &socket, dir.path(), "the fake hub to exit");
    watchdog.stand_down(deadline.cleanup_phase());
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
    // Without colour the box's side stands where the stripe was.
    let boxed = stripe == "│";
    if stripe != "▌" && stripe != " " && !boxed {
        return false;
    }
    if boxed {
        if grid.cell(BOX_LEFT + 1, INPUT_ROW).symbol != "›" {
            return false;
        }
    } else {
        if grid.cell(BOX_LEFT + 1, INPUT_ROW).symbol != " " {
            return false;
        }
        if grid.cell(BOX_LEFT + 2, INPUT_ROW).symbol != "›" {
            return false;
        }
    }
    for y in [EDGE_TOP, INPUT_ROW, EDGE_BOTTOM] {
        let first = grid.cell(BOX_LEFT, y);
        for x in BOX_LEFT..BOX_RIGHT {
            let cell = grid.cell(x, y);
            if cell.bg != first.bg {
                return false;
            }
            if y != INPUT_ROW {
                if boxed {
                    // ┌─┐ above and └─┘ below, dim: corners at the
                    // ends, fills between (`docs/tui.md`, "Look").
                    let (left, right) = if y == EDGE_TOP {
                        ("┌", "┐")
                    } else {
                        ("└", "┘")
                    };
                    let want = if x == BOX_LEFT {
                        left
                    } else if x == BOX_RIGHT - 1 {
                        right
                    } else {
                        "─"
                    };
                    if cell.symbol != want || cell.fg != first.fg || !cell.dim {
                        return false;
                    }
                } else if cell.symbol != first.symbol || cell.fg != first.fg {
                    return false;
                }
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
/// drawn grid and the whole output. One session: the rail stays hidden,
/// so the look pinned is the conversation's own.
#[track_caller]
fn one_turn(env: &[(&str, &str)]) -> (Grid, Vec<u8>) {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Hel", "lo."]}]}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200},
            "tui": {"rail": {"shown": false}}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], env);
    // The end of the first frame proves the input reader runs before
    // the prompt goes out; the drawn turn proves the quit lands
    // anywhere, since quitting is taken in any state.
    run.ready();
    run.write(b"say hi\r");
    // A drawn-turn miss carries the hub log: the #2602 sighting showed
    // the hub never bound, and the log says whether the hub was slow or
    // failed to start. The wait still fails with its screen.
    let screen = run.wait_screen_reporting("the drawn turn", turn_drawn, || {
        let tail = support::hub_log_tail(&setup.hub_log());
        if tail.is_empty() {
            format!(
                "hub.log tail (last {} lines):\n(empty)",
                support::HUB_LOG_TAIL
            )
        } else {
            format!(
                "hub.log tail (last {} lines):\n{tail}",
                support::HUB_LOG_TAIL
            )
        }
    });
    let output = run.output();
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
    (screen, output)
}

/// The bottom-most row holding `>` at `x` with `x >= 1`: with colour a
/// space before it and a stripe or a space before that; without colour
/// the box's side before it. Returns the stripe's or side's cell.
fn input_row(screen: &Grid) -> (u16, u16) {
    for y in (0..48).rev() {
        for x in 1..160 {
            if screen.cell(x, y).symbol.as_str() != "›" {
                continue;
            }
            if screen.cell(x - 1, y).symbol.as_str() == "│" {
                return (x - 1, y);
            }
            if x < 2 || screen.cell(x - 1, y).symbol.as_str() != " " {
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
/// run: with the rail hidden the box spans the 125-column conversation
/// past the grip, the panel grip sitting at column 126.
const BOX_LEFT: u16 = 1;
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
    // The reply draws in the default colour, not dim, on a turn card.
    let mut reply = false;
    for y in 0..48 {
        for x in 0..158 {
            let word: String = (0..3).map(|dx| screen.cell(x + dx, y).symbol).collect();
            if word == "Hel" {
                for dx in 0..3 {
                    let cell = screen.cell(x + dx, y);
                    assert_eq!(cell.fg, Colour::Default);
                    assert!(!cell.dim);
                    assert_eq!(cell.bg, TURN_RGB);
                }
                reply = true;
            }
        }
    }
    assert!(reply, "the reply on a turn card");
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
fn no_color_sends_no_colour_and_boxes_keep_their_rows() {
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
    // The input box is a thin dim box: ┌─┐ above, │ on each side and
    // └─┘ below, on the terminal's default colours (`docs/tui.md`,
    // "Look"). The edge rows keep their rows.
    for (y, left, right) in [(EDGE_TOP, "┌", "┐"), (EDGE_BOTTOM, "└", "┘")] {
        for x in BOX_LEFT..BOX_RIGHT {
            let cell = screen.cell(x, y);
            let want = if x == BOX_LEFT {
                left
            } else if x == BOX_RIGHT - 1 {
                right
            } else {
                "─"
            };
            assert_eq!(cell.symbol.as_str(), want, "edge cell ({x}, {y})");
            assert_eq!(cell.fg, Colour::Default, "edge cell ({x}, {y})");
            assert_eq!(cell.bg, Colour::Default, "edge cell ({x}, {y})");
            assert!(cell.dim, "edge cell ({x}, {y})");
        }
    }
    // The prompt sits one column in, past the box's left side, between
    // │ sides.
    assert_eq!(screen.cell(BOX_LEFT, INPUT_ROW).symbol.as_str(), "│");
    assert_eq!(screen.cell(BOX_LEFT + 1, INPUT_ROW).symbol.as_str(), "›");
    assert_eq!(screen.cell(BOX_RIGHT - 1, INPUT_ROW).symbol.as_str(), "│");
}

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
                && reply_card(2, 124, 1, 125)
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
/// and returns the run. `rail_shown` sets `tui.rail.shown` for the run:
/// one-session callers hide it, so the look pinned is unchanged, while
/// the two-session caller keeps it.
#[track_caller]
fn started_run(rail_shown: bool) -> (Setup, Run) {
    let setup = Setup::new();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [{"text": ["Hel", "lo."]}]}),
    );
    let mut config = serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200}});
    if !rail_shown {
        config["tui"] = serde_json::json!({"rail": {"shown": false}});
    }
    support::write_json(&setup.home().join("config.json"), &config);
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

#[track_caller]
fn quit(mut run: Run) {
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn one_session_has_no_header_row_and_a_blank_column_each_side() {
    let (_setup, mut run) = started_run(false);
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
    let (_setup, mut run) = started_run(false);
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
    let (_setup, mut run) = started_run(true);
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

/// The dark theme's turn card, prompt bubble, attention and scroll bar in
/// truecolour (`docs/tui.md`, "Themes").
const TURN_RGB: Colour = Colour::Rgb(16, 16, 23);
const PROMPT_RGB: Colour = Colour::Rgb(52, 53, 65);
const ATTENTION_RGB: Colour = Colour::Rgb(255, 159, 67);
const SCROLL_RGB: Colour = Colour::Rgb(128, 128, 128);

/// Escape and Alt+A as the kitty keyboard protocol encodes them
/// (`crates/tui/src/keys.rs`).
const ESC: &[u8] = b"\x1b[27u";
const ALT_A: &[u8] = b"\x1b[97;3u";

/// The pill's row on `grid`, if it draws with its ends and label.
fn pill_at(grid: &Grid) -> Option<u16> {
    (0..48).find(|y| {
        let pill: String = (48..=77).map(|x| grid.cell(x, *y).symbol).collect();
        pill == "▐ ↓ New messages below · End ▌"
    })
}

/// The pill and scroll bar have every cell property checked below.
fn pill_is_complete(grid: &Grid) -> bool {
    let Some(row) = pill_at(grid) else {
        return false;
    };
    if !(48..=77).all(|x| {
        let cell = grid.cell(x, row);
        !cell.dim
            && !cell.bold
            && !cell
                .symbol
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_digit)
    }) {
        return false;
    }
    if grid.cell(48, row).fg != RULE_RGB || grid.cell(77, row).fg != RULE_RGB {
        return false;
    }
    if !(49..=76).all(|x| {
        let cell = grid.cell(x, row);
        cell.fg == ATTENTION_RGB && cell.bg == RULE_RGB
    }) {
        return false;
    }
    let mut thumbs = 0;
    for y in 0..=row {
        let cell = grid.cell(125, y);
        if cell.symbol == "┃" {
            if cell.fg != SCROLL_RGB {
                return false;
            }
            thumbs += 1;
        } else if cell.symbol != " " {
            return false;
        }
    }
    thumbs > 0
        && (0..48).all(|y| {
            let symbol = grid.cell(125, y).symbol;
            symbol != "│" && symbol != "█"
        })
}

/// Whether the followed frame has every cell property asserted below.
fn followed_end_is_complete(grid: &Grid) -> bool {
    if grid.contents.contains("New messages below")
        || !grid.contents.contains("Done.")
        || grid
            .rows
            .iter()
            .filter(|row| row.contains("▣ completed"))
            .count()
            != 2
    {
        return false;
    }
    let bubble: Vec<u16> = (0..48)
        .filter(|y| {
            (58..=124).all(|x| grid.cell(x, *y).bg == PROMPT_RGB)
                && grid.cell(124, *y).symbol == "▐"
        })
        .collect();
    if bubble.len() != 2 || bubble[0] < 2 || bubble[1] + 4 >= 48 {
        return false;
    }
    let first = bubble[0];
    let second = bubble[1];
    if !bubble
        .iter()
        .all(|y| grid.cell(57, *y).bg == Colour::Default && grid.cell(124, *y).fg == ACCENT_RGB)
    {
        return false;
    }
    if grid.cell(58, first).symbol != " "
        || grid.cell(59, first).symbol != "a"
        || grid.cell(123, first).symbol != " "
    {
        return false;
    }
    for (y, edge) in [(first - 1, "▄"), (second + 1, "▀")] {
        if !(58..=124).all(|x| {
            let cell = grid.cell(x, y);
            cell.symbol == edge && cell.fg == PROMPT_RGB && cell.bg == Colour::Default
        }) {
            return false;
        }
    }
    if !(1..=124).all(|x| {
        let cell = grid.cell(x, first - 2);
        cell.symbol == " " && cell.bg == Colour::Default
    }) {
        return false;
    }
    let time_row = second + 2;
    let time: String = (0..160).map(|x| grid.cell(x, time_row).symbol).collect();
    if !is_time_row(time.trim())
        || (0..125).rfind(|x| grid.cell(*x, time_row).symbol != " ") != Some(123)
        || !(0..125).all(|x| {
            let cell = grid.cell(x, time_row);
            cell.symbol == " " || cell.dim
        })
        || grid.cell(124, time_row).symbol != " "
    {
        return false;
    }
    if !(1..=124).all(|x| grid.cell(x, second + 3).symbol == " ") {
        return false;
    }
    let card_top = second + 4;
    if !(2..=124).all(|x| {
        let cell = grid.cell(x, card_top);
        cell.symbol == "▄" && cell.fg == TURN_RGB && cell.bg == Colour::Default
    }) {
        return false;
    }
    let Some(card_bottom) = (card_top..48).find(|y| {
        (2..=124).all(|x| grid.cell(x, *y).symbol == "▀" && grid.cell(x, *y).fg == TURN_RGB)
    }) else {
        return false;
    };
    if !(card_top + 1..card_bottom).all(|y| {
        (2..=124).all(|x| grid.cell(x, y).bg == TURN_RGB)
            && grid.cell(2, y).symbol == " "
            && grid.cell(124, y).symbol == " "
    }) {
        return false;
    }
    let row_with = |needle: &str| {
        (card_top + 1..card_bottom).find(|y| {
            (0..160)
                .map(|x| grid.cell(x, *y).symbol)
                .collect::<String>()
                .contains(needle)
        })
    };
    let (Some(group), Some(steer), Some(said), Some(done)) = (
        row_with("Ran 1 command"),
        row_with("steer"),
        row_with("use x"),
        row_with("Done."),
    ) else {
        return false;
    };
    if ![(group, '●'), (steer, 's'), (said, 'u'), (done, 'D')]
        .into_iter()
        .all(|(y, first)| {
            grid.cell(2, y).symbol == " " && grid.cell(3, y).symbol == first.to_string()
        })
        || said != steer + 1
        || [(group, steer), (said, done)]
            .into_iter()
            .any(|(first, second)| {
                second != first + 2 || !(2..=124).all(|x| grid.cell(x, first + 1).symbol == " ")
            })
    {
        return false;
    }
    if (3..=7)
        .map(|x| grid.cell(x, steer).symbol)
        .collect::<String>()
        != "steer"
        || !(3..=7).all(|x| {
            let cell = grid.cell(x, steer);
            cell.fg == ATTENTION_RGB && cell.bold
        })
        || !(8..=16).all(|x| grid.cell(x, steer).dim)
        || !(17..=123).all(|x| {
            let cell = grid.cell(x, steer);
            cell.symbol == "─" && cell.fg == RULE_RGB
        })
        || !(3..=7).all(|x| grid.cell(x, said).bold)
    {
        return false;
    }
    let Some(closing) = (card_top + 1..card_bottom).find(|y| {
        (0..160)
            .map(|x| grid.cell(x, *y).symbol)
            .collect::<String>()
            .contains("1 call")
    }) else {
        return false;
    };
    let closed: String = (0..160).map(|x| grid.cell(x, closing).symbol).collect();
    closing == done + 2
        && (1..125).all(|x| {
            let cell = grid.cell(x, closing);
            cell.symbol == " " || cell.dim
        })
        && !closed.contains("token")
        && !closed.contains('$')
}

/// Whether `text` matches `you HH:MM`.
fn is_time_row(text: &str) -> bool {
    let cells = text.as_bytes();
    cells.len() >= 9
        && cells[..4] == *b"you "
        && cells[4].is_ascii_digit()
        && cells[5].is_ascii_digit()
        && cells[6] == b':'
        && cells[7].is_ascii_digit()
        && cells[8].is_ascii_digit()
}

#[test]
fn turns_draw_bubbles_padded_turn_cards_a_thumb_and_the_pill() {
    let setup = Setup::new();
    let items: Vec<String> = (1..=60).map(|n| format!("- item {n:02}")).collect();
    support::write_json(
        &setup.workspace().join("s.json"),
        &serde_json::json!({"steps": [
            {"text": [items.join("\n")]},
            {"tool_calls": [{"name": "shell", "arguments": {"command": "echo hi"}}]},
            {"text": ["Done."]},
        ]}),
    );
    fs::write(
        setup.home().join("rules"),
        serde_json::json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"}).to_string()
            + "\n",
    )
    .unwrap();
    support::write_json(
        &setup.home().join("config.json"),
        &serde_json::json!({"model": "scripted/s.json", "hub": {"idle_exit_ms": 200},
            "tui": {"rail": {"shown": false}}}),
    );
    let mut run = Run::spawn(&setup, 160, 48, &[], &TRUECOLOUR);
    run.ready();
    let from = run.output().len();
    run.write(b"say hi\r");
    run.turn_finished(from);
    // A prompt of 42 a's, 43 b's and 20 c's: the narrower bubble holds
    // the a's on its first row, the rest on its second.
    let long = format!("{} {} {}", "a".repeat(42), "b".repeat(43), "c".repeat(20));
    let from2 = run.output().len();
    run.write(long.as_bytes());
    run.write(b"\r");
    // The shell call waits on a standing-ask approval.
    run.wait_screen("the approval panel", |grid| {
        grid.contents.contains("Allow once")
    });
    run.wait_bytes(from2, WAITING_TITLE, "the waiting turn");
    // Esc puts the request aside; the input box comes back.
    run.write(ESC);
    run.wait_screen("the input box", |grid| {
        !grid.contents.contains("Allow once") && grid.contents.contains("›")
    });
    // A steer typed into the input box queues visibly.
    run.write(b"use x\r");
    run.wait_screen("the steering queue", |grid| {
        grid.contents.contains("↳ use x")
    });
    // Alt+A reopens the request; the wheel needs motion reports first.
    run.write(ALT_A);
    run.wait_screen("the reopened approval", |grid| {
        grid.contents.contains("Allow once")
    });
    run.wait_bytes(0, MOTION, "mouse motion enabled");
    let mut wheels = Vec::new();
    for _ in 0..20 {
        wheels.extend_from_slice(b"\x1b[<64;21;11M");
    }
    run.write(&wheels);
    run.wait_screen("scrolled to item 05", |grid| {
        grid.contents.contains("item 05")
    });
    // Enter allows once; the call runs, the reply lands, the turn ends.
    run.write(b"\r");
    run.wait_bytes(from2, FINISHED_TITLE, "the finished second turn");
    // New output while scrolled up raises the pill.
    let pill = run.wait_screen("the pill", pill_is_complete);
    let row = pill_at(&pill).expect("the pill's row");
    // The pill is centred on the conversation's bottom row: ▐ at 48 and
    // ▌ at 77, both in rule, the label between them in attention on rule.
    assert_eq!(pill.cell(48, row).symbol.as_str(), "▐");
    assert_eq!(pill.cell(48, row).fg, RULE_RGB);
    assert_eq!(pill.cell(77, row).symbol.as_str(), "▌");
    assert_eq!(pill.cell(77, row).fg, RULE_RGB);
    for x in 49..=76 {
        let cell = pill.cell(x, row);
        assert_eq!(cell.fg, ATTENTION_RGB, "pill cell ({x}, {row})");
        assert_eq!(cell.bg, RULE_RGB, "pill cell ({x}, {row})");
    }
    let label: String = (49..=76).map(|x| pill.cell(x, row).symbol).collect();
    assert_eq!(label, " ↓ New messages below · End ");
    // The pill gives no count.
    for x in 48..=77 {
        assert!(
            !pill
                .cell(x, row)
                .symbol
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_digit),
            "pill cell ({x}, {row})"
        );
    }
    // The bar column holds the thumb or nothing, never a track.
    let mut thumbs = 0;
    for y in 0..=row {
        let cell = pill.cell(125, y);
        if cell.symbol.as_str() == "┃" {
            assert_eq!(cell.fg, SCROLL_RGB, "bar cell (125, {y})");
            thumbs += 1;
        } else {
            assert_eq!(cell.symbol.as_str(), " ", "bar cell (125, {y})");
        }
    }
    assert!(thumbs > 0, "a thumb shows");
    for y in 0..48 {
        let symbol = pill.cell(125, y).symbol;
        assert_ne!(symbol.as_str(), "│", "bar cell (125, {y})");
        assert_ne!(symbol.as_str(), "█", "bar cell (125, {y})");
    }
    // End follows again: the pill goes, "Done." shows, both turns closed.
    run.write(b"\x1b[F");
    let end = run.wait_screen("the followed end", followed_end_is_complete);
    assert!(!end.contents.contains("New messages below"));
    // The long prompt's bubble is 67 cells wide: its widest row (64)
    // plus pads, right-aligned past the grip.
    let bubble: Vec<u16> = (0..48)
        .filter(|y| {
            (58..=124).all(|x| end.cell(x, *y).bg == PROMPT_RGB)
                && end.cell(124, *y).symbol.as_str() == "▐"
        })
        .collect();
    assert_eq!(bubble.len(), 2, "two bubble text rows");
    for y in &bubble {
        for x in 58..=124 {
            assert_eq!(end.cell(x, *y).bg, PROMPT_RGB, "bubble cell ({x}, {y})");
        }
        assert_eq!(end.cell(57, *y).bg, Colour::Default);
        assert_eq!(end.cell(124, *y).fg, ACCENT_RGB);
    }
    let first = bubble[0];
    assert_eq!(end.cell(58, first).symbol.as_str(), " ");
    assert_eq!(end.cell(59, first).symbol.as_str(), "a");
    assert_eq!(end.cell(123, first).symbol.as_str(), " ");
    // Half-block edges above and below, on the terminal background.
    for (y, edge) in [(first - 1, "▄"), (bubble[1] + 1, "▀")] {
        for x in 58..=124 {
            let cell = end.cell(x, y);
            assert_eq!(cell.symbol.as_str(), edge, "edge cell ({x}, {y})");
            assert_eq!(cell.fg, PROMPT_RGB, "edge cell ({x}, {y})");
            assert_eq!(cell.bg, Colour::Default, "edge cell ({x}, {y})");
        }
    }
    // A blank row above the top edge.
    for x in 1..=124 {
        let cell = end.cell(x, first - 2);
        assert_eq!(cell.symbol.as_str(), " ", "blank cell ({x}, {})", first - 2);
        assert_eq!(cell.bg, Colour::Default, "blank cell ({x}, {})", first - 2);
    }
    // The time row ends one column before the stripe, dim.
    let time: String = (0..160)
        .map(|x| end.cell(x, bubble[1] + 2).symbol)
        .collect();
    assert!(is_time_row(time.trim()), "{time:?}");
    // Without the bar column the row ends at column 123.
    assert_eq!(
        (0..125).rfind(|x| end.cell(*x, bubble[1] + 2).symbol != " "),
        Some(123),
        "the time ends at column 123: {time:?}"
    );
    for x in 0..125 {
        if end.cell(x, bubble[1] + 2).symbol.as_str() != " " {
            assert!(
                end.cell(x, bubble[1] + 2).dim,
                "time cell ({x}, {})",
                bubble[1] + 2
            );
        }
    }
    assert_eq!(end.cell(124, bubble[1] + 2).symbol.as_str(), " ");
    // A blank row, then the card's top edge across the conversation.
    for x in 1..=124 {
        assert_eq!(end.cell(x, bubble[1] + 3).symbol.as_str(), " ");
    }
    for x in 2..=124 {
        let cell = end.cell(x, bubble[1] + 4);
        assert_eq!(
            cell.symbol.as_str(),
            "▄",
            "card edge ({x}, {})",
            bubble[1] + 4
        );
        assert_eq!(cell.fg, TURN_RGB, "card edge ({x}, {})", bubble[1] + 4);
        assert_eq!(
            cell.bg,
            Colour::Default,
            "card edge ({x}, {})",
            bubble[1] + 4
        );
    }
    // Every card row up to the card's bottom edge sits on the turn tint
    // with a blank column on each side.
    let card_top = bubble[1] + 4;
    let card_bottom = (card_top..48)
        .find(|y| {
            (2..=124)
                .all(|x| end.cell(x, *y).symbol.as_str() == "▀" && end.cell(x, *y).fg == TURN_RGB)
        })
        .expect("the card's bottom edge");
    for y in card_top + 1..card_bottom {
        for x in 2..=124 {
            assert_eq!(end.cell(x, y).bg, TURN_RGB, "card cell ({x}, {y})");
        }
        assert_eq!(end.cell(2, y).symbol.as_str(), " ");
        assert_eq!(end.cell(124, y).symbol.as_str(), " ");
    }
    // Each block starts at column 3 with one blank card row between.
    let row_with = |needle: &str| {
        (card_top + 1..card_bottom)
            .find(|y| {
                (0..160)
                    .map(|x| end.cell(x, *y).symbol)
                    .collect::<String>()
                    .contains(needle)
            })
            .unwrap_or_else(|| panic!("{needle} draws in the card"))
    };
    let group = row_with("Ran 1 command");
    let steer = row_with("steer");
    let said = row_with("use x");
    let done = row_with("Done.");
    for (y, first) in [(group, '●'), (steer, 's'), (said, 'u'), (done, 'D')] {
        assert_eq!(end.cell(2, y).symbol.as_str(), " ");
        assert_eq!(end.cell(3, y).symbol.as_str(), &first.to_string());
    }
    // The steered text sits directly under its rule; one blank card
    // row separates every other pair of blocks.
    assert_eq!(said, steer + 1);
    for (first, second) in [(group, steer), (said, done)] {
        assert_eq!(second - first, 2);
        for y in first + 1..second {
            assert!(
                (2..=124).all(|x| end.cell(x, y).symbol.as_str() == " "),
                "one blank card row between blocks"
            );
        }
    }
    // The steer rule: the label bold in attention, the time dim, the
    // dashes in rule to column 123.
    assert_eq!(
        (3..=7)
            .map(|x| end.cell(x, steer).symbol)
            .collect::<String>(),
        "steer"
    );
    for x in 3..=7 {
        let cell = end.cell(x, steer);
        assert_eq!(cell.fg, ATTENTION_RGB, "steer cell ({x}, {steer})");
        assert!(cell.bold, "steer cell ({x}, {steer})");
    }
    for x in 8..=16 {
        assert!(end.cell(x, steer).dim, "steer time cell ({x}, {steer})");
    }
    for x in 17..=123 {
        let cell = end.cell(x, steer);
        assert_eq!(cell.symbol.as_str(), "─", "steer cell ({x}, {steer})");
        assert_eq!(cell.fg, RULE_RGB, "steer cell ({x}, {steer})");
    }
    // The steered text is bold.
    for x in 3..=7 {
        assert!(end.cell(x, said).bold, "steered cell ({x}, {said})");
    }
    // The ▣ line is dim, counts the one call, and leaves out zero
    // figures.
    let closing = (card_top + 1..card_bottom)
        .find(|y| {
            (0..160)
                .map(|x| end.cell(x, *y).symbol)
                .collect::<String>()
                .contains("1 call")
        })
        .expect("the closing line counts one call");
    assert_eq!(
        closing,
        done + 2,
        "one blank card row before the closing line"
    );
    for x in 1..125 {
        if end.cell(x, closing).symbol.as_str() != " " {
            assert!(end.cell(x, closing).dim, "closing cell ({x}, {closing})");
        }
    }
    let closed: String = (0..160).map(|x| end.cell(x, closing).symbol).collect();
    assert!(!closed.contains("token"), "{closed:?}");
    assert!(!closed.contains('$'), "{closed:?}");
    quit(run);
}
