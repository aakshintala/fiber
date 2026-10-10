//! Tests for the loop, and for `run` through a pseudo-terminal.

use super::{Input, Loop, Screen};
use crate::app::App;
use crate::keys::Event;
use crate::link::Line;
use crate::pty_watch::{watch, watched};
use crate::stroke::{Code, Mods, Stroke};
use contract::clock::Clock;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// The launch description `run` tests start from: `/w`, outside git.
pub(super) fn launch() -> super::Launch {
    super::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: Some("p/m".to_owned()),
        thinking: None,
        logo_glyph: "⌇".to_owned(),
        keys: crate::KeysSetup::default(),
        rail_share: 15.0,
        panel_share: 21.0,
        panel_cards: Vec::new(),
        ..Default::default()
    }
}

/// One named wall-clock deadline for every blocking wait.
pub(super) const DEADLINE: Duration = Duration::from_secs(10);

/// A pty pair: the main side and the slave as a file.
pub(super) struct Pair {
    /// The main side.
    pub(super) main: File,
    /// The slave side, the injected tty.
    pub(super) slave: File,
}

/// Opens a pty pair with a 60x12 window.
pub(super) fn open() -> Pair {
    let main =
        rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
            .unwrap_or_else(|err| panic!("openpt: {err}"));
    rustix::pty::grantpt(&main).unwrap_or_else(|err| panic!("grantpt: {err}"));
    rustix::pty::unlockpt(&main).unwrap_or_else(|err| panic!("unlockpt: {err}"));
    let name =
        rustix::pty::ptsname(&main, Vec::new()).unwrap_or_else(|err| panic!("ptsname: {err}"));
    let path = PathBuf::from(std::ffi::OsStr::from_bytes(name.as_bytes()));
    let slave = File::options()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap_or_else(|err| panic!("open slave: {err}"));
    rustix::termios::tcsetwinsize(
        &slave,
        rustix::termios::Winsize {
            ws_col: 60,
            ws_row: 12,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap_or_else(|err| panic!("winsize: {err}"));
    Pair {
        main: File::from(main),
        slave,
    }
}

/// Whether canonical mode, echo and signals are on. Compared flag by flag:
/// the kernel may set `PENDIN` on its own, so a whole-struct equality would
/// be brittle.
fn is_cooked(termios: &rustix::termios::Termios) -> bool {
    use rustix::termios::LocalModes;
    termios.local_modes.contains(LocalModes::ICANON)
        && termios.local_modes.contains(LocalModes::ECHO)
        && termios.local_modes.contains(LocalModes::ISIG)
}

/// Every byte the backend wrote, shared with the test.
#[derive(Clone, Default)]
pub(super) struct Sink(Arc<Mutex<Vec<u8>>>);

impl Sink {
    pub(super) fn len(&self) -> usize {
        self.0.lock().map_or(0, |bytes| bytes.len())
    }
}

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if let Ok(mut held) = self.0.lock() {
            held.extend_from_slice(bytes);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A loop at 60x12 on `backend`, with no tty, that records attaches.
pub(super) fn new_loop<B: Backend + crate::screen::SyncEmit>(
    backend: B,
    tty: Option<File>,
) -> (Loop<B>, Arc<Mutex<Vec<String>>>) {
    let attached = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&attached);
    let mut app = App::new(PathBuf::from("/w"));
    app.set_size(60, 12);
    let screen = Screen::new(backend, 60, 12).unwrap_or_else(|err| panic!("screen: {err}"));
    let lp = Loop {
        app,
        parser: crate::keys::Parser::default(),
        screen,
        hub: None,
        tty,
        on_attach: Box::new(move |session| {
            if let Ok(mut held) = seen.lock() {
                held.push(session.0.clone());
            }
        }),
        clock: fakes::clock::FakeClock::new(),
        wakeups: 0,
        files_out: None,
        search: None,
        stash: std::collections::VecDeque::new(),
        reader: None,
        model_reader: crate::catalogue::Reader::new(None),
        paste_reader: None,
        pointer: crate::mouse::Pointer::default(),
        hover: true,
        var: Box::new(|_| None),
        copy_command: None,
        open_command: None,
        viewer: Vec::new(),
        images_dir: PathBuf::new(),
        title: crate::osc::Title::default(),
        shape: crate::osc::Shape::default(),
        retry: None,
        tick: crate::tick::TickThread::idle(),
    };
    (lp, attached)
}

/// Runs `lp` over `inputs`, then with every sender gone. With no inputs it
/// returns before drawing, so a test that captures a frame feeds at least one
/// input first.
pub(super) fn feed<B: Backend>(lp: &mut Loop<B>, inputs: Vec<Input>) -> i32 {
    let (tx, rx) = mpsc::channel();
    for input in inputs {
        tx.send(input).unwrap_or_else(|err| panic!("send: {err}"));
    }
    drop(tx);
    lp.run(&rx)
}

pub(super) fn hello() -> contract::HubLine {
    contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::Map::new(),
    }
}

/// A connect that hands out `hub` and `hello` once, and fails after.
pub(super) fn once(hub: UnixStream, hello: contract::HubLine) -> crate::Connect {
    let mut held = Some((hub, hello));
    Box::new(move || {
        held.take()
            .ok_or_else(|| io::Error::other("the test's one connection is used"))
    })
}

/// Runs `work` on a thread and returns its result, failing after
/// [`DEADLINE`] with `what`: one deadline however many reads it makes.
fn within<T: Send + 'static>(what: &str, work: impl FnOnce() -> T + Send + 'static) -> T {
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-within".to_owned())
        .spawn(move || done.send(work()).unwrap_or(()))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for {what}: {err}"))
}

/// Reads one command line the loop wrote to the hub, with one deadline,
/// and hands the reader back.
pub(super) fn command(
    mut reader: BufReader<UnixStream>,
    what: &str,
) -> (BufReader<UnixStream>, serde_json::Value) {
    let (reader, line) = within(what, move || {
        let mut line = String::new();
        let read = reader.read_line(&mut line);
        (reader, read.map(|_| line))
    });
    let line = line.unwrap_or_else(|err| panic!("{what}: {err}"));
    let value = serde_json::from_str(&line).unwrap_or_else(|err| panic!("{what}: {err}: {line:?}"));
    (reader, value)
}

#[test]
fn inputs_wake_the_loop_once_each_and_no_ops_write_nothing() {
    let sink = Sink::default();
    let (mut lp, _) = new_loop(CrosstermBackend::new(sink.clone()), None);
    lp.screen
        .draw(&mut lp.app, None)
        .unwrap_or_else(|err| panic!("draw: {err}"));
    let first = sink.len();
    assert!(first > 0);
    // Unknown sequences, which the parser drops, and an Enter on an empty
    // draft: three wakeups, no visible change, no byte.
    let quiet = vec![
        Input::Bytes(b"\x1b[99X".to_vec()),
        Input::Bytes(b"\r".to_vec()),
        Input::Bytes(b"\x1b[?62;22c".to_vec()),
    ];
    // Every sender gone, the loop's `recv` returns: it was parked there.
    assert_eq!(feed(&mut lp, quiet), 0);
    assert_eq!(lp.wakeups, 3);
    assert_eq!(sink.len(), first);
    // A key that changes a row writes it.
    assert_eq!(feed(&mut lp, vec![Input::Bytes(b"a".to_vec())]), 0);
    assert_eq!(lp.wakeups, 4);
    assert!(sink.len() > first);
}

#[test]
fn a_kitty_reply_is_recorded() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    feed(&mut lp, vec![Input::Bytes(b"\x1b[?5u".to_vec())]);
    assert!(lp.app.kitty());
    // No tty took the push, so a lone ESC ending a read is still Esc.
    assert_eq!(
        lp.parser.feed(b"\x1b"),
        vec![Event::Stroke(Stroke {
            code: Code::Esc,
            mods: Mods::NONE,
        })]
    );
}

#[test]
fn the_first_kitty_reply_pushes_the_flags_once() {
    let pair = open();
    let frames = watch(&pair.main, vec![b"END" as &[u8]]);
    let tty = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    // A second reply pushes nothing more: after the push and the first
    // frame's title, the next bytes on the tty are the marker written
    // after it.
    feed(
        &mut lp,
        vec![
            Input::Bytes(b"\x1b[?0u".to_vec()),
            Input::Bytes(b"\x1b[?1u".to_vec()),
        ],
    );
    (&pair.slave)
        .write_all(b"END")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert_eq!(
        watched(&frames, "the kitty push"),
        [
            b"\x1b[>1u".to_vec(),
            crate::osc::title("fiber"),
            b"END".to_vec()
        ]
        .concat()
    );
    assert_eq!(crate::term::KITTY_PUSH, b"\x1b[>1u");
    // Once pushed, Esc is `CSI 27u`: a lone ESC ending a read is held.
    assert!(lp.parser.feed(b"\x1b").is_empty());
    assert_eq!(
        lp.parser.feed(b"[27u"),
        vec![Event::Stroke(Stroke {
            code: Code::Esc,
            mods: Mods::NONE,
        })]
    );
}

#[test]
fn ctrl_c_twice_quits_with_zero() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let (tx, rx) = mpsc::channel();
    tx.send(Input::Bytes(vec![0x03]))
        .unwrap_or_else(|err| panic!("send: {err}"));
    tx.send(Input::Bytes(vec![0x03]))
        .unwrap_or_else(|err| panic!("send: {err}"));
    // The sender stays: the loop returns on the quit, not on a closed
    // channel.
    assert_eq!(lp.run(&rx), 0);
    assert_eq!(lp.wakeups, 2);
    drop(tx);
}

#[test]
fn a_rebound_new_session_answers_its_new_key() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let mut user = serde_json::Map::new();
    user.insert("new_session".to_owned(), serde_json::json!("ctrl+t"));
    lp.app.set_keys(crate::KeysSetup { user });
    let (_, rx) = mpsc::channel();
    // `0x0e` is Ctrl+N: with `new_session` rebound it does nothing.
    assert_eq!(lp.step(Input::Bytes(vec![0x0e]), &rx), None);
    assert!(lp.app.session().is_some());
    // `0x14` is Ctrl+T: it goes home.
    assert_eq!(lp.step(Input::Bytes(vec![0x14]), &rx), None);
    assert!(lp.app.session().is_none());
}

#[test]
fn the_hub_connection_starts_attaches_and_subscribes() {
    let (mut lp, attached) = new_loop(TestBackend::new(60, 12), None);
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let reader = BufReader::new(theirs);
    // Enter before the hub connects: `start` goes out with `hub_hello`.
    feed(
        &mut lp,
        vec![
            Input::Bytes(b"hi\r".to_vec()),
            Input::Connected(ours, hello()),
        ],
    );
    let (reader, start) = command(reader, "the start command");
    assert_eq!(start["command"], "start");
    assert_eq!(start["args"]["workspace"], "/w");
    assert!(start["args"].get("content").is_none());
    let accepted = contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"command_id": start["id"], "result": {"session_id": "s_aaaaaaaaaaaaaaaa"}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    let seen = || attached.lock().map(|held| held.clone()).unwrap_or_default();
    // The attach is reported when `start` is accepted, once.
    feed(&mut lp, vec![Input::Hub(Line::Hub(accepted.clone()))]);
    assert_eq!(seen(), vec!["s_aaaaaaaaaaaaaaaa".to_owned()]);
    feed(&mut lp, vec![Input::Hub(Line::Hub(accepted))]);
    assert_eq!(seen(), vec!["s_aaaaaaaaaaaaaaaa".to_owned()]);
    let (reader, subscribe) = command(reader, "the subscribe command");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(subscribe["args"]["level"], "full");
    let (reader, asked) = command(reader, "the commands command");
    assert_eq!(asked["command"], "commands");
    let (_, prompt) = command(reader, "the first prompt");
    assert_eq!(prompt["command"], "prompt");
    assert_eq!(prompt["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(prompt["args"]["content"][0]["text"], "hi");
}

#[test]
fn connect_failure_and_disconnect_are_notices() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    feed(&mut lp, vec![Input::ConnectFailed("refused".to_owned())]);
    assert_eq!(lp.app.notice(), Some("Could not reach the hub: refused"));
    let (ours, _theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    feed(
        &mut lp,
        vec![Input::Connected(ours, hello()), Input::Disconnected],
    );
    assert_eq!(lp.app.notice(), Some("Connection lost."));
    assert!(lp.hub.is_none());
}

/// A clone of `ours`, as the hub reader thread holds one, that waits
/// at most [`DEADLINE`] per read.
fn reader_of(ours: &UnixStream) -> UnixStream {
    let reader = ours
        .try_clone()
        .unwrap_or_else(|err| panic!("clone: {err}"));
    reader
        .set_read_timeout(Some(DEADLINE))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    reader
}

/// Whether `reader` sees the end at once: its stream was shut down for
/// reading. A stream still open waits out [`DEADLINE`] and fails.
fn sees_the_end(mut reader: UnixStream, what: &str) {
    let mut byte = [0u8; 1];
    match reader.read(&mut byte) {
        Ok(0) => {}
        Ok(_) => panic!("{what}: a byte instead of the end"),
        Err(err) => panic!("waited {DEADLINE:?} for {what}: {err}"),
    }
}

#[test]
fn a_failed_write_hangs_up_and_returns_the_draft() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    // The hub end stays open: only the write side fails, so the reader
    // ends only if the loop shuts its stream down.
    let (ours, _theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let reader = reader_of(&ours);
    ours.shutdown(std::net::Shutdown::Write)
        .unwrap_or_else(|err| panic!("shutdown: {err}"));
    feed(&mut lp, vec![Input::Connected(ours, hello())]);
    assert!(lp.hub.is_some());
    feed(&mut lp, vec![Input::Bytes(b"hi\r".to_vec())]);
    assert!(lp.hub.is_none());
    sees_the_end(reader, "the reader to see the hang-up");
    assert_eq!(lp.app.notice(), Some("Connection lost."));
    assert_eq!(lp.app.draft(), "hi");
    // Enter on a lost connection keeps the draft.
    feed(
        &mut lp,
        vec![Input::Bytes(b"\r".to_vec()), Input::Disconnected],
    );
    assert_eq!(lp.app.draft(), "hi");
}

#[test]
fn a_schema_mismatch_says_so_and_hangs_up() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let (ours, _theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let reader = reader_of(&ours);
    let mut newer = hello();
    newer.schema_version = contract::SCHEMA_VERSION + 1;
    feed(&mut lp, vec![Input::Connected(ours, newer)]);
    assert!(lp.hub.is_none());
    sees_the_end(reader, "the reader to see the hang-up");
    // The reader's end arrives next; the version notice stays.
    feed(&mut lp, vec![Input::Disconnected]);
    assert!(
        lp.app
            .notice()
            .is_some_and(|notice| notice.contains("schema version"))
    );
}

#[test]
fn restore_puts_back_what_setup_changed() {
    let pair = open();
    let start = "\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
    let end = "\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";
    let frames = watch(&pair.main, vec![start.as_bytes(), end.as_bytes()]);
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    assert_eq!(watched(&frames, "the start bytes"), start.as_bytes());
    crate::restore();
    assert_eq!(watched(&frames, "the restore bytes"), end.as_bytes());
    assert!(is_cooked(
        &rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"))
    ));
}

#[test]
fn resize_redraws_at_the_new_size() {
    let pair = open();
    // The test never reads the pty: the watcher drains it to end of file.
    let _frames = watch(&pair.main, Vec::new());
    rustix::termios::tcsetwinsize(
        &pair.slave,
        rustix::termios::Winsize {
            ws_col: 40,
            ws_row: 10,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap_or_else(|err| panic!("winsize: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(pair.slave));
    feed(&mut lp, vec![Input::Bytes(b"hi".to_vec()), Input::Resize]);
    // The input line draws one row above the new last row, under its top
    // edge; the test backend keeps its 60x12 buffer, cleared by the
    // resize.
    let shown = crate::view::text(lp.screen.backend().buffer());
    let rows: Vec<&str> = shown.lines().collect();
    assert_eq!(rows.get(8).copied(), Some("▌ › hi█"));
    assert!(
        rows.get(9)
            .is_some_and(|row| row.chars().all(|ch| ch == '▀')),
        "{rows:?}"
    );
    assert!(rows.iter().skip(10).all(|row| row.is_empty()));
}

#[test]
fn run_quits_on_double_ctrl_c_with_the_reader_blocked() {
    let mut pair = open();
    let before =
        rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let clock = fakes::clock::FakeClock::new();
    let (hub, _held) =
        std::os::unix::net::UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let hello = contract::HubLine {
        kind: "hub_hello".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: Default::default(),
    };
    // The watcher starts before `run`, and is the only reader from the
    // first frame to end of file.
    let frames = watch(
        &pair.main,
        vec![b"shortcuts", b"Press", b"Ctrl+C", b"again to", RESTORE],
    );
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-run".to_owned())
        .spawn(move || {
            let code = super::run(slave, launch(), once(hub, hello), Box::new(|_| {}), clock);
            match done.send(code) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // On home the input line is not on the last row: the first frame is
    // read through the placeholder, whose letters are written together.
    // The start bytes are checked inside the first marker's chunk.
    let first = watched(&frames, "the first frame");
    assert!(
        first.starts_with(START),
        "the first chunk starts with the start bytes: {first:?}"
    );
    // The slave is in raw mode while running.
    let raw = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&before));
    assert!(!is_cooked(&raw));
    // "Reader blocked" means the terminal's input thread: the test writes only two 0x03 bytes, and the return wait below bounds the block. The watcher keeps draining the output, so the test's own reader adds no second block.
    // The first press must visibly arm first: waiting for its hint proves the
    // reader delivered a byte and the loop drew again.
    pair.main
        .write_all(&[0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    // The armed frame foots the quit hint, which no earlier frame
    // holds. Its fragments are watched in order: the incremental
    // redraw splits the hint around the cells the unarmed foot already
    // holds, so the whole hint never arrives in one run, but every
    // fragment does.
    watched(&frames, "the armed quit hint's Press");
    watched(&frames, "the armed quit hint's Ctrl+C");
    watched(&frames, "the armed quit hint's again to");
    pair.main
        .write_all(&[0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
    let after = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&after));
    // After the last frame the output holds the restore bytes.
    let tail = watched(&frames, "the restore bytes");
    assert_eq!(
        tail.get(tail.len().saturating_sub(RESTORE.len())..),
        Some(RESTORE)
    );
}

#[test]
fn run_shows_a_failed_connect_and_still_quits_restored() {
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    // The watcher starts before `run`, and is the only reader from the
    // first frame to end of file.
    let frames = watch(&pair.main, vec![b"refused" as &[u8], b"mark" as &[u8]]);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-run".to_owned())
        .spawn(move || {
            let code = super::run(
                slave,
                launch(),
                Box::new(|| Err(io::Error::other("refused"))),
                Box::new(|_| {}),
                fakes::clock::FakeClock::new(),
            );
            done.send(code).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // Unchanged cells are skipped, spaces included, so one word is matched.
    // The start bytes are checked inside the first marker's chunk.
    let notice = watched(&frames, "the connect notice");
    assert!(
        notice.starts_with(START),
        "the first chunk starts with the start bytes: {notice:?}"
    );
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
    assert!(is_cooked(
        &rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"))
    ));
    // The screen's drop shows the cursor inside a closed synchronized
    // block after the restore; a second restore then writes nothing, so
    // the next bytes are the test's own.
    crate::restore();
    (&pair.slave)
        .write_all(b"mark")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let tail = watched(&frames, "the mark");
    let dropped: &[u8] = b"\x1b[?2026h\x1b[?25h\x1b[?2026l";
    let expected = [RESTORE, dropped, b"mark".as_slice()].concat();
    assert_eq!(
        tail.get(tail.len().saturating_sub(expected.len())..),
        Some(expected.as_slice())
    );
}

#[test]
fn run_redraws_on_sigwinch_at_the_new_size() {
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    // The watcher starts before `run`, and is the only reader from the
    // first frame to end of file.
    let frames = watch(
        &pair.main,
        vec![b"refused" as &[u8], b"\x1b[10;1H" as &[u8], "↓".as_bytes()],
    );
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-run".to_owned())
        .spawn(move || {
            let code = super::run(
                slave,
                launch(),
                Box::new(|| Err(io::Error::other("refused"))),
                Box::new(|_| {}),
                fakes::clock::FakeClock::new(),
            );
            done.send(code).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The notice is drawn after the first frame, so SIGWINCH is caught.
    // The start bytes are checked inside the first marker's chunk.
    let notice = watched(&frames, "the connect notice");
    assert!(
        notice.starts_with(START),
        "the first chunk starts with the start bytes: {notice:?}"
    );
    rustix::termios::tcsetwinsize(
        &pair.slave,
        rustix::termios::Winsize {
            ws_col: 40,
            ws_row: 10,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap_or_else(|err| panic!("winsize: {err}"));
    // Each test runs in its own process, so the signal reaches only this
    // test's `run`.
    signal_hook::low_level::raise(signal_hook::consts::SIGWINCH)
        .unwrap_or_else(|err| panic!("raise: {err}"));
    // The foot hint sits on the new last row, wider than the screen and
    // cut: the cursor goes to column 1, and the next character printed,
    // after any colour change, is its `↓`.
    watched(&frames, "the move to row 10");
    let next = watched(&frames, "the foot hint on row 10");
    let between = next
        .get(..next.len().saturating_sub("↓".len()))
        .unwrap_or_default();
    assert!(
        between
            .iter()
            .all(|byte| *byte == 0x1b || b"[;m0123456789".contains(byte)),
        "{:?}",
        String::from_utf8_lossy(&next)
    );
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
}

/// A review request offering the rule `npm test`, as the hub relays it.
pub(super) fn offering(request: &str) -> Input {
    let payload = serde_json::json!({
        "request_id": request, "effects": ["executes"], "reversible": true, "step": "review",
        "rule": {"subject": "npm test --watch", "prefix": "npm test"},
    });
    Input::Hub(Line::Session(contract::Envelope {
        kind: "permission_requested".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: Some(contract::ActionId(format!("a_{request}"))),
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }))
}

fn one_question(payload: serde_json::Value) -> Input {
    Input::Hub(Line::Session(contract::Envelope {
        kind: "interaction_requested".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: payload.as_object().cloned().unwrap_or_default(),
    }))
}

#[test]
fn each_approval_choice_goes_to_the_hub_as_a_reply() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let reader = BufReader::new(theirs);
    feed(
        &mut lp,
        vec![
            Input::Connected(ours, hello()),
            Input::Bytes(b"hi\r".to_vec()),
        ],
    );
    let (reader, start) = command(reader, "the start command");
    let accepted = contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"command_id": start["id"], "result": {"session_id": "s_aaaaaaaaaaaaaaaa"}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    };
    feed(&mut lp, vec![Input::Hub(Line::Hub(accepted))]);
    let (reader, _) = command(reader, "the subscribe command");
    let (reader, asked) = command(reader, "the commands command");
    assert_eq!(asked["command"], "commands");
    let (mut reader, _) = command(reader, "the first prompt");
    feed(
        &mut lp,
        ["r_1", "r_2", "r_3", "r_4"]
            .map(offering)
            .into_iter()
            .collect(),
    );
    // Allow once; for this session; in this project; deny with feedback.
    let keys: [&[u8]; 4] = [b"\r", b"\x1b[B\r", b"\x1b[B\x1b[B\r", b"no\r"];
    feed(
        &mut lp,
        keys.iter()
            .map(|bytes| Input::Bytes(bytes.to_vec()))
            .collect(),
    );
    let remember = |scope: &str| serde_json::json!({"scope": scope, "prefix": "npm test"});
    let expected = [
        serde_json::json!({"request_id": "r_1", "decision": "allow"}),
        serde_json::json!({"request_id": "r_2", "decision": "allow", "remember": remember("session")}),
        serde_json::json!({"request_id": "r_3", "decision": "allow", "remember": remember("project")}),
        serde_json::json!({"request_id": "r_4", "decision": "deny", "feedback": "no"}),
    ];
    for args in expected {
        let (next, reply) = command(reader, "a reply");
        reader = next;
        assert_eq!(reply["command"], "reply");
        assert_eq!(reply["session_id"], "s_aaaaaaaaaaaaaaaa");
        assert_eq!(reply["args"], args);
    }
    assert!(lp.app.panel().is_none());
}

fn connected_loop() -> (Loop<TestBackend>, BufReader<UnixStream>) {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let reader = BufReader::new(theirs);
    feed(
        &mut lp,
        vec![
            Input::Connected(ours, hello()),
            Input::Bytes(b"hi\r".to_vec()),
        ],
    );
    let (reader, start) = command(reader, "the start command");
    let accepted = contract::HubLine {
        kind: "command_accepted".to_owned(),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        payload: serde_json::json!({"command_id": start["id"],
            "result": {"session_id": "s_aaaaaaaaaaaaaaaa"}})
        .as_object()
        .cloned()
        .unwrap_or_default(),
    };
    feed(&mut lp, vec![Input::Hub(Line::Hub(accepted))]);
    let (reader, _) = command(reader, "the subscribe command");
    let (reader, commands) = command(reader, "the commands command");
    assert_eq!(commands["command"], "commands");
    let (reader, prompt) = command(reader, "the first prompt");
    assert_eq!(prompt["command"], "prompt");
    (lp, reader)
}

#[test]
fn each_one_question_kind_goes_to_the_hub_as_a_reply() {
    let (mut lp, mut reader) = connected_loop();
    feed(
        &mut lp,
        vec![
            one_question(serde_json::json!({"request_id": "r_c", "kind": "confirm",
                "prompt": "Continue?"})),
            one_question(serde_json::json!({"request_id": "r_s", "kind": "select",
                "prompt": "Pick one?", "options": [{"label": "a"}, {"label": "b"}]})),
            one_question(
                serde_json::json!({"request_id": "r_m", "kind": "multi_select",
                "prompt": "Pick some?", "options": [{"label": "a"}, {"label": "b"},
                    {"label": "c"}]}),
            ),
            one_question(
                serde_json::json!({"request_id": "r_t", "kind": "text_input",
                "prompt": "What?"}),
            ),
        ],
    );
    feed(
        &mut lp,
        [
            b"\r".as_slice(),
            b"\x1b[B\r".as_slice(),
            b" \x1b[B\x1b[B \r".as_slice(),
            b"hi\r".as_slice(),
        ]
        .map(|bytes| Input::Bytes(bytes.to_vec()))
        .into_iter()
        .collect(),
    );
    let expected = [
        serde_json::json!({"request_id": "r_c", "confirmed": true}),
        serde_json::json!({"request_id": "r_s", "labels": ["b"]}),
        serde_json::json!({"request_id": "r_m", "labels": ["a", "c"]}),
        serde_json::json!({"request_id": "r_t", "text": "hi"}),
    ];
    for args in expected {
        let (next, reply) = command(reader, "a one-question reply");
        reader = next;
        assert_eq!(reply["command"], "reply");
        assert_eq!(reply["session_id"], "s_aaaaaaaaaaaaaaaa");
        assert_eq!(reply["args"], args);
    }
    assert!(lp.app.panel().is_none());
}

#[test]
fn esc_on_each_one_question_kind_declines_to_the_hub() {
    let (mut lp, mut reader) = connected_loop();
    feed(
        &mut lp,
        vec![
            one_question(serde_json::json!({"request_id": "r_c", "kind": "confirm",
                "prompt": "Continue?"})),
            one_question(serde_json::json!({"request_id": "r_s", "kind": "select",
                "prompt": "Pick one?", "options": [{"label": "a"}, {"label": "b"}]})),
            one_question(
                serde_json::json!({"request_id": "r_m", "kind": "multi_select",
                "prompt": "Pick some?", "options": [{"label": "a"}, {"label": "b"},
                    {"label": "c"}]}),
            ),
            one_question(
                serde_json::json!({"request_id": "r_t", "kind": "text_input",
                "prompt": "What?"}),
            ),
        ],
    );
    feed(
        &mut lp,
        (0..4).map(|_| Input::Bytes(b"\x1b".to_vec())).collect(),
    );
    for request_id in ["r_c", "r_s", "r_m", "r_t"] {
        let (next, reply) = command(reader, "a declined one-question reply");
        reader = next;
        assert_eq!(reply["command"], "reply");
        assert_eq!(reply["session_id"], "s_aaaaaaaaaaaaaaaa");
        assert_eq!(
            reply["args"],
            serde_json::json!({"request_id": request_id,
            "declined": true})
        );
    }
    assert!(lp.app.panel().is_none());
}

/// The hub accepting command `id` from `s_aaaaaaaaaaaaaaaa`.
fn session_accepted(id: &serde_json::Value) -> Input {
    Input::Hub(Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": id})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    }))
}

/// The two-question form `r_c`, plus `extra` keys.
fn form_with(extra: &serde_json::Value) -> serde_json::Value {
    let mut payload = serde_json::json!({"request_id": "r_c", "kind": "form",
        "fields": [
            {"header": "Base", "question": "Which branch?", "options": [
                {"label": "main"}, {"label": "dev"}]},
            {"header": "Name", "question": "What name?"}]});
    merge(&mut payload, extra);
    payload
}

/// The `confirm` `r_c`, plus `extra` keys.
fn confirm_with(extra: &serde_json::Value) -> serde_json::Value {
    let mut payload = serde_json::json!({"request_id": "r_c", "kind": "confirm",
        "prompt": "Continue?"});
    merge(&mut payload, extra);
    payload
}

/// Adds the keys of `extra` to `payload`.
fn merge(payload: &mut serde_json::Value, extra: &serde_json::Value) {
    if let (Some(map), Some(more)) = (payload.as_object_mut(), extra.as_object()) {
        map.extend(more.clone());
    }
}

/// Esc on a `confirm`, and Esc and "Chat about this" on a form, each with
/// `extra` keys: the request and the bytes that decline it.
fn declines(extra: &serde_json::Value) -> Vec<(serde_json::Value, Vec<u8>)> {
    vec![
        (confirm_with(extra), b"\x1b".to_vec()),
        (form_with(extra), b"\x1b".to_vec()),
        (form_with(extra), b"\x1b[B\x1b[B\x1b[B\x1b[B\r".to_vec()),
    ]
}

/// Declining `payload` with `keys`, with a turn running or not: the
/// declined reply, its accept, then a prompt typed after it. The command
/// that follows the reply on the hub's socket.
fn command_after_decline(
    running: bool,
    payload: serde_json::Value,
    keys: &[u8],
) -> serde_json::Value {
    let (mut lp, reader) = connected_loop();
    if running {
        feed(
            &mut lp,
            vec![Input::Hub(Line::Session(contract::Envelope {
                kind: "turn_started".to_owned(),
                session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
                ts: 0,
                schema_version: contract::SCHEMA_VERSION,
                turn_id: None,
                action_id: None,
                seq: None,
                payload: serde_json::json!({"input": [{"type": "message",
                    "source": "driver",
                    "content": [{"type": "text", "text": "go"}]}]})
                .as_object()
                .cloned()
                .unwrap_or_default(),
            }))],
        );
    }
    feed(&mut lp, vec![one_question(payload)]);
    feed(&mut lp, vec![Input::Bytes(keys.to_vec())]);
    let (reader, reply) = command(reader, "the declined reply");
    assert_eq!(reply["command"], "reply");
    assert_eq!(
        reply["args"],
        serde_json::json!({"request_id": "r_c", "declined": true})
    );
    feed(&mut lp, vec![session_accepted(&reply["id"])]);
    feed(&mut lp, vec![Input::Bytes(b"next\r".to_vec())]);
    let (_, after) = command(reader, "the command after the decline");
    after
}

#[test]
fn a_decline_of_a_tool_calls_question_is_followed_by_a_cancel() {
    let extra = serde_json::json!({"action_ids": ["a_1"]});
    for running in [false, true] {
        for (payload, keys) in declines(&extra) {
            let after = command_after_decline(running, payload, &keys);
            assert_eq!(after["command"], "cancel", "running {running}");
            assert_eq!(after["session_id"], "s_aaaaaaaaaaaaaaaa");
        }
    }
}

#[test]
fn a_decline_of_a_question_no_tool_call_raised_sends_no_cancel() {
    let extra = serde_json::json!({});
    for running in [false, true] {
        for (payload, keys) in declines(&extra) {
            let after = command_after_decline(running, payload, &keys);
            assert_ne!(after["command"], "cancel", "running {running}");
        }
    }
}

#[test]
fn the_screen_shows_the_cursor_at_the_draft() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    feed(&mut lp, vec![Input::Bytes(b"ab\x1b[D".to_vec())]);
    let backend = lp.screen.backend_mut();
    assert!(backend.cursor_visible());
    backend.assert_cursor_position((5, 10));
}

#[test]
fn a_cursor_move_alone_writes_and_a_still_frame_writes_nothing() {
    let sink = Sink::default();
    let (mut lp, _) = new_loop(CrosstermBackend::new(sink.clone()), None);
    feed(&mut lp, vec![Input::Bytes(b"ab".to_vec())]);
    let typed = sink.len();
    // ← changes no cell, only the cursor.
    feed(&mut lp, vec![Input::Bytes(b"\x1b[D".to_vec())]);
    let moved = sink.len();
    assert!(moved > typed);
    // ← at the start changes nothing: no byte.
    feed(&mut lp, vec![Input::Bytes(b"\x1b[D\x1b[D".to_vec())]);
    let start = sink.len();
    feed(&mut lp, vec![Input::Bytes(b"\x1b[D".to_vec())]);
    assert_eq!(sink.len(), start);
}

/// The next bytes the reader sends, with one deadline.
fn next_bytes(rx: &mpsc::Receiver<Input>, what: &str) -> Vec<u8> {
    match rx.recv_timeout(DEADLINE) {
        Ok(Input::Bytes(bytes)) => bytes,
        Ok(_) => panic!("{what}: not bytes"),
        Err(err) => panic!("waited {DEADLINE:?} for {what}: {err}"),
    }
}

#[test]
fn hand_over_gives_the_terminal_and_its_input_to_the_program() {
    let mut pair = open();
    let start = "\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
    let restore = "\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";
    let frames = watch(&pair.main, vec![restore.as_bytes(), b"ENDMARK" as &[u8]]);
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    let tty = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    let (tx, rx) = mpsc::channel();
    lp.reader = super::Reader::spawn(&pair.slave, tx);
    lp.parser.set_kitty();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let mut main = pair
        .main
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    // The pause inside blocks: the loop runs on a thread, with a deadline.
    let (lp, code, seen) = within("the hand-over", move || {
        let mut seen = None;
        lp.write_title();
        let code = lp.hand_over(|| {
            let cooked =
                rustix::termios::tcgetattr(&slave).unwrap_or_else(|err| panic!("attr: {err}"));
            // A line typed now goes to the program, not to the paused reader.
            main.write_all(b"typed\n")
                .unwrap_or_else(|err| panic!("write: {err}"));
            let line = within("the program's read", move || {
                let mut line = String::new();
                BufReader::new(slave).read_line(&mut line).map(|_| line)
            });
            seen = Some((is_cooked(&cooked), line.unwrap_or_default()));
        });
        // The same title is written again after the program.
        lp.write_title();
        (lp, code, seen)
    });
    drop(lp);
    assert_eq!(code, None);
    assert_eq!(seen, Some((true, "typed\n".to_owned())));
    assert!(!is_cooked(
        &rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"))
    ));
    // The terminal was restored, then set up again with hover and kitty's
    // flags. The mark is written after the hand-over returns.
    (&pair.slave)
        .write_all(b"ENDMARK")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let suspended = watched(&frames, "the restore bytes");
    assert!(
        suspended.starts_with(start.as_bytes()),
        "the restore chunk starts with the start bytes: {suspended:?}"
    );
    // In between, the cooked terminal echoed the program's line.
    let resumed = "typed\r\n\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[>1u\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n";
    let title = crate::osc::title("fiber");
    assert_eq!(
        watched(&frames, "the mark"),
        [resumed.as_bytes(), title.as_slice(), b"ENDMARK".as_slice()].concat()
    );
    // The reader runs again.
    pair.main
        .write_all(b"k")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert_eq!(next_bytes(&rx, "a key after the program"), b"k");
    crate::term::restore();
}

#[test]
fn hand_over_that_cannot_take_the_terminal_back_quits_with_one() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let mut ran = false;
    assert_eq!(lp.hand_over(|| ran = true), Some(1));
    assert!(ran);
}

#[test]
fn ctrl_c_or_ctrl_backslash_in_a_cooked_terminal_leaves_fiber_running() {
    // Each test runs in its own process, so the signals reach only this
    // test; uncaught, either would end it.
    super::catch_interrupts();
    signal_hook::low_level::raise(signal_hook::consts::SIGINT)
        .unwrap_or_else(|err| panic!("raise: {err}"));
    signal_hook::low_level::raise(signal_hook::consts::SIGQUIT)
        .unwrap_or_else(|err| panic!("raise: {err}"));
}

/// A fake editor and what keeps it in check.
struct FakeEditor {
    /// Holds the script.
    _dir: fakes::TempDir,
    /// The script's path: every editor process's command line names it.
    script: String,
    /// Kills any editor process left when the test ends or dies.
    _watchdog: fakes::Watchdog,
}

impl FakeEditor {
    /// Asserts no editor process is left.
    fn assert_gone(&self) {
        let left = fakes::matching(&self.script).unwrap_or_else(|err| panic!("ps: {err}"));
        assert!(left.is_empty(), "editor processes left: {left:?}");
    }
}

/// A fake editor whose script is `body`, run as `/bin/sh <script>`, under a
/// watchdog matching its path; and an environment reader naming it as
/// `$EDITOR`.
fn fake_editor(body: &str) -> (FakeEditor, super::Var) {
    let dir = fakes::TempDir::new("editor");
    let script = dir.path().join("editor.sh");
    std::fs::write(&script, body).unwrap_or_else(|err| panic!("script: {err}"));
    let command = format!("/bin/sh {}", script.display());
    let script = script.display().to_string();
    let watchdog = fakes::Watchdog::matching(&script);
    (
        FakeEditor {
            _dir: dir,
            script,
            _watchdog: watchdog,
        },
        Box::new(move |name| (name == "EDITOR").then(|| command.clone())),
    )
}

/// Runs `lp` over `inputs` on a thread with one deadline: the exit code
/// and the draft.
fn feed_within(mut lp: Loop<TestBackend>, inputs: Vec<Input>) -> (i32, String) {
    within("the loop", move || {
        let code = feed(&mut lp, inputs);
        (code, lp.app.draft())
    })
}

#[test]
fn ctrl_g_puts_the_editors_text_in_the_draft_and_the_loop_reads_on() {
    let pair = open();
    // The watcher drains what the terminal is sent, so no write blocks.
    let _frames = watch(&pair.main, Vec::new());
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    let tty = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    let (editor, var) = fake_editor("printf 'edited' > \"$1\"\n");
    lp.var = var;
    let inputs = ["a", "\x07", "!"].map(|bytes| Input::Bytes(bytes.as_bytes().to_vec()));
    assert_eq!(feed_within(lp, inputs.into()), (0, "edited!".to_owned()));
    editor.assert_gone();
    crate::term::restore();
}

#[test]
fn ctrl_g_that_cannot_take_the_terminal_back_quits_with_one() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let (editor, var) = fake_editor("printf 'edited' > \"$1\"\n");
    lp.var = var;
    let inputs = ["a", "\x07", "!"].map(|bytes| Input::Bytes(bytes.as_bytes().to_vec()));
    assert_eq!(feed_within(lp, inputs.into()), (1, "a".to_owned()));
    editor.assert_gone();
}

#[test]
fn ctrl_g_with_no_editor_says_so_and_the_loop_reads_on() {
    let (lp, _) = new_loop(TestBackend::new(60, 12), None);
    let inputs = ["a", "\x07", "!"].map(|bytes| Input::Bytes(bytes.as_bytes().to_vec()));
    let (lp, code) = within("the loop", move || {
        let mut lp = lp;
        let code = feed(&mut lp, inputs.into());
        (lp, code)
    });
    assert_eq!(code, 0);
    assert_eq!(lp.app.draft(), "a!");
    assert_eq!(lp.app.notice(), Some(crate::editor::NO_EDITOR));
}

#[test]
fn a_copy_writes_osc_52_and_pipes_the_code_to_the_command() {
    let pair = open();
    let frames = watch(&pair.main, vec![b"\x1b]52;c;bGV0IGEgPSAxOw==\x07" as &[u8]]);
    let dir = fakes::TempDir::new("tui-copy");
    let out = dir.path().join("copied").display().to_string();
    let ready = fakes::children::Ready::new(dir.path());
    let watchdog = fakes::Watchdog::matching(&out);
    let tty = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    let ready_path = ready.path().display().to_string();
    lp.copy_command = Some(
        [
            "/bin/sh",
            "-c",
            "cat > \"$0\"; echo $$ > \"$1\"",
            &out,
            &ready_path,
        ]
        .map(str::to_owned)
        .to_vec(),
    );
    let session = contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    lp.app.attach(session.clone());
    let line = |kind: &str, payload: serde_json::Value, action: Option<&str>| {
        Input::Hub(Line::Session(contract::Envelope {
            kind: kind.to_owned(),
            session_id: session.clone(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|id| contract::ActionId(id.to_owned())),
            seq: None,
            payload: payload.as_object().cloned().unwrap_or_default(),
        }))
    };
    let started = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "hi"}]}]});
    let reply = serde_json::json!({"text": "```rust\nlet a = 1;\n```"});
    feed(
        &mut lp,
        vec![
            line("turn_started", started, None),
            line("assistant_message_delta", reply, Some("a_1")),
        ],
    );
    let shown = crate::view::text(lp.screen.backend().buffer());
    let row = shown
        .lines()
        .position(|line| line.ends_with("copy"))
        .and_then(|row| u16::try_from(row).ok())
        .unwrap_or_else(|| panic!("no copy target on\n{shown}"));
    // SGR reports are 1-based. A wheel, a right click, hover, a left click
    // off `copy` and a press on `copy` released elsewhere copy nothing.
    let at = |button: u8, col: u16, row: u16, end: char| {
        Input::Bytes(format!("\x1b[<{button};{};{}{end}", col + 1, row + 1).into_bytes())
    };
    feed(
        &mut lp,
        vec![
            at(64, 57, row, 'M'),
            at(2, 57, row, 'M'),
            at(2, 57, row, 'm'),
            at(35, 57, row, 'M'),
            at(0, 10, row, 'M'),
            at(0, 10, row, 'm'),
            at(0, 57, row, 'M'),
            at(0, 57, row + 1, 'm'),
        ],
    );
    assert!(
        !lp.app.copied(),
        "a report other than a click on copy copied"
    );
    feed(&mut lp, vec![at(0, 57, row, 'M'), at(0, 57, row, 'm')]);
    assert!(lp.app.copied());
    // A left click off any target, here on blank cells, clears "Copied".
    feed(&mut lp, vec![at(0, 2, 0, 'M'), at(0, 2, 0, 'm')]);
    assert!(!lp.app.copied());
    // The first frame's title, then the copy.
    let osc = [
        crate::osc::title("fiber"),
        b"\x1b]52;c;bGV0IGEgPSAxOw==\x07".to_vec(),
    ]
    .concat();
    assert_eq!(watched(&frames, "the OSC 52 bytes"), osc);
    ready.wait(DEADLINE);
    assert_eq!(
        std::fs::read_to_string(&out).ok().as_deref(),
        Some("let a = 1;")
    );
    watchdog.stand_down(DEADLINE);
}

#[test]
fn a_non_left_press_keeps_copied_and_a_left_press_clears_it() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let session = contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    lp.app.attach(session.clone());
    let line = |kind: &str, payload: serde_json::Value, action: Option<&str>| {
        Input::Hub(Line::Session(contract::Envelope {
            kind: kind.to_owned(),
            session_id: session.clone(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|id| contract::ActionId(id.to_owned())),
            seq: None,
            payload: payload.as_object().cloned().unwrap_or_default(),
        }))
    };
    let started = serde_json::json!({"input": [{"type": "message", "source": "driver",
        "content": [{"type": "text", "text": "hi"}]}]});
    let reply = serde_json::json!({"text": "```rust\nlet a = 1;\n```"});
    feed(
        &mut lp,
        vec![
            line("turn_started", started, None),
            line("assistant_message_delta", reply, Some("a_1")),
        ],
    );
    let shown = crate::view::text(lp.screen.backend().buffer());
    let row = shown
        .lines()
        .position(|line| line.ends_with("copy"))
        .and_then(|row| u16::try_from(row).ok())
        .unwrap_or_else(|| panic!("no copy target on\n{shown}"));
    // SGR reports are 1-based.
    let at = |button: u8, col: u16, row: u16, end: char| {
        Input::Bytes(format!("\x1b[<{button};{};{}{end}", col + 1, row + 1).into_bytes())
    };
    // A left click on `copy` shows "Copied".
    feed(&mut lp, vec![at(0, 57, row, 'M'), at(0, 57, row, 'm')]);
    assert!(lp.app.copied());
    // A right press on blank cells is not a click: "Copied" stays.
    feed(&mut lp, vec![at(2, 2, 0, 'M'), at(2, 2, 0, 'm')]);
    assert!(lp.app.copied());
    // A left press on blank cells clears "Copied".
    feed(&mut lp, vec![at(0, 2, 0, 'M'), at(0, 2, 0, 'm')]);
    assert!(!lp.app.copied());
}

#[test]
fn opening_a_row_by_key_calls_on_attach() {
    let (mut lp, attached) = new_loop(TestBackend::new(60, 12), None);
    lp.app.set_home(launch());
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let reader = BufReader::new(theirs);
    feed(&mut lp, vec![Input::Connected(ours, hello())]);
    let (reader, feed_cmd) = command(reader, "the feed command");
    assert_eq!(feed_cmd["command"], "feed");
    let (reader, _) = command(reader, "the recent command");
    let status = contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "state": "streaming",
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    };
    feed(&mut lp, vec![Input::Hub(Line::Session(status))]);
    // The first row draws at the second column of the tenth row: a press
    // then a release on it opens the session.
    feed(
        &mut lp,
        vec![
            Input::Bytes(b"\x1b[<0;1;10M".to_vec()),
            Input::Bytes(b"\x1b[<0;1;10m".to_vec()),
        ],
    );
    let (reader, subscribe) = command(reader, "the subscribe command");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(subscribe["args"]["level"], "full");
    let (_, commands) = command(reader, "the commands command");
    assert_eq!(commands["command"], "commands");
    assert_eq!(commands["session_id"], "s_aaaaaaaaaaaaaaaa");
    let seen = || attached.lock().map(|held| held.clone()).unwrap_or_default();
    assert_eq!(seen(), vec!["s_aaaaaaaaaaaaaaaa".to_owned()]);
}

/// A live `session_status` for `session` in `state`.
fn live_status(session: &str, state: &str) -> Line {
    Line::Session(contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "state": state,
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
}

#[test]
fn an_exit_sends_its_lines_then_quits() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.app.set_home(launch());
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let reader = BufReader::new(theirs);
    let (_tx, rx) = mpsc::channel();
    assert_eq!(lp.step(Input::Connected(ours, hello()), &rx), None);
    assert_eq!(
        lp.step(
            Input::Hub(live_status("s_aaaaaaaaaaaaaaaa", "streaming")),
            &rx
        ),
        None
    );
    // Ctrl+C twice asks while the session works, and `c` closes all.
    assert_eq!(lp.step(Input::Bytes(vec![0x03]), &rx), None);
    assert_eq!(lp.step(Input::Bytes(vec![0x03]), &rx), None);
    let (reader, feed) = command(reader, "the feed");
    assert_eq!(feed["command"], "feed");
    let (reader, recent) = command(reader, "the recent");
    assert_eq!(recent["command"], "recent");
    // The close lines reach the far end, and the step quits with 0.
    assert_eq!(lp.step(Input::Bytes(b"c".to_vec()), &rx), Some(0));
    let (reader, subscribe) = command(reader, "the subscribe");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(subscribe["args"]["level"], "summary");
    let (_, close) = command(reader, "the close");
    assert_eq!(close["command"], "close");
    assert_eq!(close["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(close["args"], serde_json::json!({"now": true}));
}

/// A hub `session_status` line for `session` in `state`.
fn hub_status(session: &str, state: &str) -> String {
    serde_json::to_string(&contract::Envelope {
        kind: "session_status".to_owned(),
        session_id: contract::SessionId(session.to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({
            "name": "fix the parser",
            "workspace": "/w",
            "project": "-w",
            "state": state,
            "since": 0,
            "spend": {"tokens": {"input": 1, "cache_read": 0,
                "cache_write": {}, "output": 2},
                "cost": 0.0, "subscription_cost": 0.0},
            "model": "test/model", "delegates": 0, "jobs": 0, "clients": 0,
        })
        .as_object()
        .cloned()
        .unwrap_or_default(),
    })
    .unwrap_or_else(|err| panic!("status: {err}"))
}

/// The terminal's start bytes, before its first frame.
const START: &[u8] =
    b"\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";

/// The terminal's restore bytes, after its last frame.
const RESTORE: &[u8] =
    b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";

/// Runs the terminal on a pty with `hub` as its hub stream: the pty pair,
/// the exit code once it quits, and the watched pty chunks. The watcher
/// starts before `run`, and is the only reader from the first frame to
/// end of file.
fn spawn_run(
    hub: UnixStream,
    markers: Vec<&'static [u8]>,
) -> (Pair, mpsc::Receiver<i32>, Receiver<Vec<u8>>) {
    spawn_run_with_launch(hub, markers, launch())
}

/// Runs the terminal on a pty from `started`, for a launch carrying the
/// person's `keys`: the pty pair, the exit code once it quits, and the
/// watched pty chunks.
fn spawn_run_with_launch(
    hub: UnixStream,
    markers: Vec<&'static [u8]>,
    started: super::Launch,
) -> (Pair, mpsc::Receiver<i32>, Receiver<Vec<u8>>) {
    let pair = open();
    let frames = watch(&pair.main, markers);
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let hello = hello();
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-run".to_owned())
        .spawn(move || {
            let code = super::run(
                slave,
                started,
                once(hub, hello),
                Box::new(|_| {}),
                fakes::clock::FakeClock::new(),
            );
            match done.send(code) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    (pair, finished, frames)
}

#[test]
fn run_home_names_the_key_maps_bound_key() {
    let mut keys = serde_json::Map::new();
    keys.insert("key_map".to_owned(), serde_json::json!("f2"));
    let mut started = launch();
    started.keys = crate::KeysSetup { user: keys };
    let (hub, held) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (mut pair, finished, frames) = spawn_run_with_launch(hub, vec![b"quit" as &[u8]], started);
    let first = watched(&frames, "the first frame");
    assert!(
        first.starts_with(START),
        "the first chunk starts with the start bytes: {first:?}"
    );
    // The renderer moves the cursor between words instead of writing the
    // blank cells, so the foot's words arrive apart, in order.
    let text = String::from_utf8_lossy(&first);
    let mut rest = &*text;
    for word in ["F2", "the", "key", "map", "Ctrl+C"] {
        let Some(at) = rest.find(word) else {
            panic!("home names the rebound key: {word:?} missing in {first:?}");
        };
        rest = &rest[at + word.len()..];
    }
    // Nothing works: Ctrl+C twice quits at once.
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    assert_eq!(
        finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}")),
        0
    );
    drop(held);
}

#[test]
fn run_prints_a_resume_line_per_live_session_after_restoring() {
    let line = b"s_aaaaaaaaaaaaaaaa  fiber resume s_aaaaaaaaaaaaaaaa";
    let (hub, held) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (mut pair, finished, frames) = spawn_run(hub, vec![b"parser" as &[u8], line as &[u8]]);
    crate::link::write_line(&held, &hub_status("s_aaaaaaaaaaaaaaaa", "idle"))
        .unwrap_or_else(|err| panic!("write: {err}"));
    // The test waits for the session's name on screen before quitting.
    // The start bytes are checked inside the first marker's chunk.
    let row = watched(&frames, "the session row");
    assert!(
        row.starts_with(START),
        "the first chunk starts with the start bytes: {row:?}"
    );
    assert!(
        row.windows(b"shortcuts".len())
            .any(|window| window == b"shortcuts"),
        "the first chunk holds the first frame: {row:?}"
    );
    // An idle session works nothing: Ctrl+C twice quits at once.
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    assert_eq!(
        finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}")),
        0
    );
    let tail = watched(&frames, "the resume line");
    let Some(at) = tail
        .windows(RESTORE.len())
        .position(|window| window == RESTORE)
    else {
        panic!("the restore bytes before the resume line: {tail:?}");
    };
    assert_eq!(
        tail.get(at + RESTORE.len()..),
        Some(line.as_slice()),
        "exactly the resume line follows the restore bytes"
    );
    drop(held);
}

#[test]
fn run_close_all_sends_summary_and_close_now_and_prints_no_line_for_it() {
    let (hub, held) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let write = held.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut pair, finished, frames) =
        spawn_run(hub, vec![b"parser" as &[u8], b"ENDMARK" as &[u8]]);
    crate::link::write_line(&write, &hub_status("s_aaaaaaaaaaaaaaaa", "streaming"))
        .unwrap_or_else(|err| panic!("write: {err}"));
    // The start bytes are checked inside the first marker's chunk.
    let row = watched(&frames, "the session row");
    assert!(
        row.starts_with(START),
        "the first chunk starts with the start bytes: {row:?}"
    );
    // Ctrl+C twice asks while the session works, and `c` closes all:
    // one write, so the loop asks before it closes.
    pair.main
        .write_all(&[0x03, 0x03, b'c'])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    // All four reads run under one deadline: the far end reads the close
    // lines the quit question sent.
    let hub = BufReader::new(held);
    let (feed, recent, subscribe, close) = within("the close commands", move || {
        let mut hub = hub;
        let mut lines = Vec::new();
        for _ in 0..4 {
            let mut line = String::new();
            hub.read_line(&mut line)
                .unwrap_or_else(|err| panic!("read: {err}"));
            lines.push(
                serde_json::from_str::<serde_json::Value>(&line)
                    .unwrap_or_else(|err| panic!("{line:?}: {err}")),
            );
        }
        let mut lines = lines.into_iter();
        (
            lines.next().expect("the feed command"),
            lines.next().expect("the recent command"),
            lines.next().expect("the subscribe command"),
            lines.next().expect("the close command"),
        )
    });
    assert_eq!(feed["command"], "feed");
    assert_eq!(recent["command"], "recent");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["args"]["level"], "summary");
    assert_eq!(close["command"], "close");
    assert_eq!(close["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(close["args"], serde_json::json!({"now": true}));
    assert_eq!(
        finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}")),
        0
    );
    // A closed session prints no resume line: the mark bounds the bytes
    // after the restore, and none names the session.
    (&pair.slave)
        .write_all(b"ENDMARK")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let tail = watched(&frames, "the mark");
    let Some(at) = tail
        .windows(RESTORE.len())
        .position(|window| window == RESTORE)
    else {
        panic!("the restore bytes before the mark: {tail:?}");
    };
    assert!(
        !tail[at..]
            .windows(12)
            .any(|window| window == b"fiber resume"),
        "no resume line follows the restore bytes: {tail:?}"
    );
}

#[test]
fn run_close_all_with_a_lost_hub_still_prints_the_resume_line() {
    let line = b"s_aaaaaaaaaaaaaaaa  fiber resume s_aaaaaaaaaaaaaaaa";
    let (hub, held) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let write = held.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut pair, finished, frames) = spawn_run(
        hub,
        vec![b"parser" as &[u8], b"lost" as &[u8], line as &[u8]],
    );
    crate::link::write_line(&write, &hub_status("s_aaaaaaaaaaaaaaaa", "streaming"))
        .unwrap_or_else(|err| panic!("write: {err}"));
    // The start bytes are checked inside the first marker's chunk.
    let row = watched(&frames, "the session row");
    assert!(
        row.starts_with(START),
        "the first chunk starts with the start bytes: {row:?}"
    );
    // The hub is lost fast: its far end drops after the status draws, and
    // the test waits for `lost` on screen.
    drop(write);
    drop(held);
    watched(&frames, "the lost notice");
    // Ctrl+C twice asks, and `c` with the link down quits and closes
    // nothing: one write, so the loop asks before it closes. The session
    // keeps its resume line.
    pair.main
        .write_all(&[0x03, 0x03, b'c'])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    assert_eq!(
        finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}")),
        0
    );
    let tail = watched(&frames, "the resume line");
    let Some(at) = tail
        .windows(RESTORE.len())
        .position(|window| window == RESTORE)
    else {
        panic!("the restore bytes before the resume line: {tail:?}");
    };
    assert_eq!(
        tail.get(tail.len().saturating_sub(line.len())..),
        Some(line.as_slice())
    );
    assert!(
        at + RESTORE.len() <= tail.len() - line.len(),
        "the restore bytes come before the resume line: {tail:?}"
    );
}

#[test]
fn run_writes_the_title_once_until_it_changes() {
    let pair = open();
    let frames = watch(&pair.main, vec![b"ENDMARK" as &[u8]]);
    let tty = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    lp.app.set_home(launch());
    let (_tx, rx) = mpsc::channel();
    // Two frames on home, one title; a status for the attached session
    // renames it, and the same status again writes nothing.
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    assert_eq!(lp.step(Input::Bytes(Vec::new()), &rx), None);
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    for _ in 0..2 {
        assert_eq!(
            lp.step(Input::Hub(live_status("s_aaaaaaaaaaaaaaaa", "idle")), &rx),
            None
        );
    }
    (&pair.slave)
        .write_all(b"ENDMARK")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let written = watched(&frames, "the mark");
    let expected = [
        crate::osc::title("fiber"),
        crate::osc::title("✓ fix the parser · fiber"),
        b"ENDMARK".to_vec(),
    ]
    .concat();
    assert_eq!(written, expected);
}

fn owned(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_owned()).collect()
}

/// Runs git with `args` in `dir`, which must succeed.
fn git(dir: &Path, args: &[&str]) {
    let dir = dir.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let what = format!("git {}", args.join(" "));
    let status = within(&what, move || {
        std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(&args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
    })
    .unwrap_or_else(|err| panic!("{what}: {err}"));
    assert!(status.success(), "{what}: {status}");
}

/// A repository with `a.txt` and `sub/b.rs` tracked and `c.txt` not.
fn repository() -> fakes::TempDir {
    let dir = fakes::TempDir::new("tui-files");
    let root = dir.path();
    std::fs::create_dir(root.join("sub")).unwrap_or_else(|err| panic!("mkdir: {err}"));
    for file in ["a.txt", "sub/b.rs", "c.txt"] {
        std::fs::write(root.join(file), "x").unwrap_or_else(|err| panic!("{file}: {err}"));
    }
    git(root, &["init", "-q"]);
    git(root, &["add", "a.txt", "sub/b.rs"]);
    dir
}

/// The next result the worker posts, within [`DEADLINE`].
fn next_result(rx: &Receiver<Input>, what: &str) -> (u64, Result<Vec<String>, String>) {
    match rx.recv_timeout(DEADLINE) {
        Ok(Input::Files { generation, result }) => (generation, result),
        Ok(_) => panic!("{what}: not a file search result"),
        Err(err) => panic!("waited {DEADLINE:?} for {what}: {err}"),
    }
}

#[test]
fn the_loop_lists_searches_and_drops_the_worker_on_close() {
    let dir = repository();
    let mut app = App::new(dir.path().to_path_buf());
    app.set_size(60, 12);
    let (out, rx) = mpsc::channel();
    let mut lp = Loop {
        app,
        parser: crate::keys::Parser::default(),
        screen: Screen::new(TestBackend::new(60, 12), 60, 12)
            .unwrap_or_else(|err| panic!("screen: {err}")),
        hub: None,
        tty: None,
        on_attach: Box::new(|_| {}),
        clock: fakes::clock::FakeClock::new(),
        wakeups: 0,
        files_out: Some(out),
        search: None,
        stash: std::collections::VecDeque::new(),
        reader: None,
        model_reader: crate::catalogue::Reader::new(None),
        paste_reader: None,
        pointer: crate::mouse::Pointer::default(),
        hover: true,
        var: Box::new(|_| None),
        copy_command: None,
        open_command: None,
        viewer: Vec::new(),
        images_dir: PathBuf::new(),
        title: crate::osc::Title::default(),
        shape: crate::osc::Shape::default(),
        retry: None,
        tick: crate::tick::TickThread::idle(),
    };
    // No hub: a frame fetches no history, so nothing arrives here.
    let (_hub, idle) = mpsc::channel();
    assert_eq!(lp.step(Input::Bytes(b"@".to_vec()), &idle), None);
    assert!(lp.search.is_some());
    let (generation, result) = next_result(&rx, "the listing's first search");
    assert_eq!(generation, lp.app.generation());
    assert_eq!(lp.step(Input::Files { generation, result }, &idle), None);
    let shown = lp.app.completions().map(|c| c.lines()).unwrap_or_default();
    assert_eq!(shown, owned(&["a.txt", "sub/b.rs"]));
    assert_eq!(lp.step(Input::Bytes(b"b".to_vec()), &idle), None);
    let (generation, result) = next_result(&rx, "the search for b");
    assert_eq!(result, Ok(owned(&["sub/b.rs"])));
    assert_eq!(lp.step(Input::Files { generation, result }, &idle), None);
    assert_eq!(lp.step(Input::Bytes(b"\t".to_vec()), &idle), None);
    assert_eq!(lp.app.draft(), "sub/b.rs ");
    assert!(lp.search.is_none());
}

/// A session of three pages with `xyzzy` on its first page, seqs from 1.
fn search_session() -> Vec<contract::Envelope> {
    let session = contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned());
    let mut lines = Vec::new();
    let mut seq = 0u64;
    let mut push = |kind: &str, action: Option<&str>, payload: serde_json::Value| {
        seq += 1;
        lines.push(contract::Envelope {
            kind: kind.to_owned(),
            session_id: session.clone(),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: action.map(|id| contract::ActionId(id.to_owned())),
            seq: Some(contract::Seq(seq)),
            payload: payload.as_object().cloned().unwrap_or_default(),
        });
    };
    let prompt = || {
        serde_json::json!({"input": [{"type": "message", "source": "driver",
            "content": [{"type": "text", "text": "go"}]}]})
    };
    let mut turn = |replies: &[&str]| {
        push("turn_started", None, prompt());
        for reply in replies {
            push(
                "text_completed",
                Some("a_m"),
                serde_json::json!({"text": reply}),
            );
        }
        push(
            "turn_completed",
            None,
            serde_json::json!({"outcome": "completed"}),
        );
    };
    turn(&["xyzzy here"]);
    let filler = ["f"; 8];
    for _ in 0..8 {
        turn(&filler);
    }
    turn(&["middle"]);
    for _ in 0..8 {
        turn(&filler);
    }
    turn(&["tail"]);
    lines
}

#[test]
fn the_pause_thread_sends_find_due_on_the_fake_clock() {
    let clock = fakes::clock::FakeClock::new();
    let origin = clock.origin();
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    lp.clock = clock.clone();
    assert!(lp.app.on_line(Line::Hub(hello())).is_empty());
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    for envelope in search_session() {
        lp.app.on_line(Line::Session(envelope));
    }
    assert!(lp.app.pages().page_count() > 2);
    assert!(lp.app.pages().part(0).is_none(), "the first page dropped");
    let (hub, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    lp.hub = Some(hub);
    let (found, due) = mpsc::channel();
    lp.files_out = Some(found);
    let (_, main) = mpsc::channel::<Input>();
    // Ctrl+F and three keystrokes in one read: three pauses, one
    // generation each.
    assert_eq!(
        lp.step(Input::Bytes(vec![0x06, b'x', b'y', b'z']), &main),
        None
    );
    // Each pause sleeps on the injected clock, then sends its generation.
    let mut generations = Vec::new();
    for _ in 0..3 {
        match due
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for FindDue: {err}"))
        {
            Input::FindDue(generation) => generations.push(generation),
            Input::Tick => panic!("a pause sent a tick"),
            Input::Bytes(_)
            | Input::Hub(_)
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::Resize
            | Input::Files { .. }
            | Input::Models(_)
            | Input::Login { .. }
            | Input::Image { .. }
            | Input::Viewed { .. } => panic!("a pause sent something else"),
        }
    }
    generations.sort();
    assert_eq!(generations, [1, 2, 3]);
    // The three sleeps moved the fake clock by three pauses. A mutant
    // that skips the sleep leaves it still.
    assert_eq!(
        clock.now(),
        origin
            .checked_add(Duration::from_millis(750))
            .expect("750ms after the origin")
    );
    // A mutant that drops the send fails above, within `DEADLINE`.
    let (_, main) = mpsc::channel::<Input>();
    for generation in generations {
        assert_eq!(lp.step(Input::FindDue(generation), &main), None);
    }
    // Only the current generation scans: exactly one `history` command
    // reaches the hub.
    theirs
        .set_read_timeout(Some(DEADLINE))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    let mut reader = BufReader::new(theirs);
    let mut text = String::new();
    reader
        .read_line(&mut text)
        .unwrap_or_else(|err| panic!("read: {err}"));
    let command: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{text:?}: {err}"));
    assert_eq!(command["command"], "history");
    assert_eq!(command["session_id"], "s_aaaaaaaaaaaaaaaa");
    let range = lp
        .app
        .pages()
        .index()
        .pages()
        .first()
        .map(|page| (page.first_seq.0, page.last_seq.0))
        .expect("a first page");
    assert_eq!(
        (
            command["args"]["from_seq"].as_u64().unwrap_or(0),
            command["args"]["to_seq"].as_u64().unwrap_or(0)
        ),
        range
    );
    // Nothing more goes out: the stale generations send nothing.
    let mut stream = reader.into_inner();
    stream
        .set_nonblocking(true)
        .unwrap_or_else(|err| panic!("nonblocking: {err}"));
    let mut byte = [0u8; 1];
    match stream.read(&mut byte) {
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {}
        read => panic!("more than one command reached the hub: {read:?}"),
    }
}

#[test]
fn keys_typed_while_a_reveal_loads_its_page_are_handled_after_in_order() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    assert!(lp.app.on_line(Line::Hub(hello())).is_empty());
    lp.app
        .attach(contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()));
    let log = search_session();
    for envelope in &log {
        lp.app.on_line(Line::Session(envelope.clone()));
    }
    assert!(lp.app.pages().page_count() > 2);
    assert!(lp.app.pages().part(0).is_none(), "the first page dropped");
    // The bar searches for the first page's word, answered from the log:
    // the current match sits on the dropped page and the reveal scrolled
    // to it.
    let now = lp.clock.now();
    assert_eq!(
        lp.app.on_key(crate::keys::Key::CtrlF, now),
        crate::app::Effect::None
    );
    let mut generation = 0u64;
    for ch in "xyz".chars() {
        generation += 1;
        assert!(matches!(
            lp.app.on_key(crate::keys::Key::Char(ch), now),
            crate::app::Effect::FindPause { .. }
        ));
    }
    let mut outgoing = lp.app.find_due(generation);
    while let Some(line) = outgoing.into_iter().next() {
        let command: serde_json::Value = serde_json::from_str(&line).expect("a command line");
        let id = command["id"].as_str().unwrap_or_default().to_owned();
        let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
        let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
        let held: Vec<contract::Envelope> = log
            .iter()
            .filter(|line| line.seq.is_some_and(|seq| from <= seq.0 && seq.0 <= to))
            .cloned()
            .collect();
        let answer = Line::Session(contract::Envelope {
            kind: "command_accepted".to_owned(),
            session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
            ts: 0,
            schema_version: contract::SCHEMA_VERSION,
            turn_id: None,
            action_id: None,
            seq: None,
            payload: serde_json::json!({"command_id": id, "result": {"lines": held}})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        });
        outgoing = lp.app.on_line(answer);
    }
    assert!(lp.app.pages().part(0).is_none());
    // The reveal scrolled to the dropped page holding the match.
    let start = lp.app.pages().index().start(0);
    assert_eq!(lp.app.top(), Some(start));
    // The loop runs on its own thread with the fake hub on a socket pair.
    let (ours, theirs) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    lp.hub = Some(ours);
    let (tx, rx) = mpsc::channel();
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-find-run".to_owned())
        .spawn(move || {
            let code = lp.run(&rx);
            done.send((code, lp)).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // Enter moves to the match again: the reveal loads its dropped page
    // inside the frame, holding the keys typed meanwhile for after.
    tx.send(Input::Bytes(b"\r".to_vec()))
        .unwrap_or_else(|err| panic!("send: {err}"));
    // The fake hub reads the reveal's request before anything else goes
    // out, within `DEADLINE`.
    theirs
        .set_read_timeout(Some(DEADLINE))
        .unwrap_or_else(|err| panic!("timeout: {err}"));
    let mut reader = BufReader::new(theirs);
    let mut text = String::new();
    reader
        .read_line(&mut text)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the reveal's request: {err}"));
    let command: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|err| panic!("{text:?}: {err}"));
    assert_eq!(command["command"], "history");
    // Only then are two characters typed: the frame holds them for after
    // the answer.
    tx.send(Input::Bytes(b"ab".to_vec()))
        .unwrap_or_else(|err| panic!("send: {err}"));
    let from = command["args"]["from_seq"].as_u64().unwrap_or(0);
    let to = command["args"]["to_seq"].as_u64().unwrap_or(u64::MAX);
    let held: Vec<contract::Envelope> = log
        .iter()
        .filter(|line| line.seq.is_some_and(|seq| from <= seq.0 && seq.0 <= to))
        .cloned()
        .collect();
    tx.send(Input::Hub(Line::Session(contract::Envelope {
        kind: "command_accepted".to_owned(),
        session_id: contract::SessionId("s_aaaaaaaaaaaaaaaa".to_owned()),
        ts: 0,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: serde_json::json!({"command_id": command["id"], "result": {"lines": held}})
            .as_object()
            .cloned()
            .unwrap_or_default(),
    })))
    .unwrap_or_else(|err| panic!("send: {err}"));
    tx.send(Input::Bytes(vec![0x03, 0x03]))
        .unwrap_or_else(|err| panic!("send: {err}"));
    let (code, lp) = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the loop to quit: {err}"));
    assert_eq!(code, 0);
    assert!(lp.app.pages().part(0).is_some(), "the revealed page loaded");
    assert_eq!(
        lp.app.find_bar().map(|bar| bar.query).unwrap_or_default(),
        "xyzab"
    );
}

#[test]
fn a_cached_read_after_the_first_frame_fills_the_catalogue() {
    let mut pair = open();
    // Each chunk is the bytes after the previous marker, up to and
    // including this one.
    let frames = watch(
        &pair.main,
        vec![
            b"shortcuts" as &[u8],
            b"the lists" as &[u8],
            b"are in" as &[u8],
        ],
    );
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in = Arc::clone(&seen);
    let (release, held) = mpsc::channel();
    let held = Arc::new(Mutex::new(held));
    let held_in = Arc::clone(&held);
    // The cached lists answer with one model and one notice, only after
    // the test releases the read following the first frame.
    let read: crate::ReadModels = Arc::new(move |refresh| {
        held_in
            .lock()
            .unwrap()
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the cached read release: {err}"));
        seen_in.lock().unwrap().push(refresh);
        Ok(crate::Catalogue {
            models: vec![crate::ModelEntry {
                reference: "acme/m1".to_owned(),
                provider: "acme".to_owned(),
                id: "m1".to_owned(),
                levels: Vec::new(),
                default_level: None,
                configured: None,
                roles: Vec::new(),
                name: None,
                price: None,
            }],
            notices: vec!["the lists are in".to_owned()],
            lists: Vec::new(),
        })
    });
    let mut started = launch();
    started.models = Some(read);
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-run".to_owned())
        .spawn(move || {
            let code = super::run(
                slave,
                started,
                Box::new(|| Err(io::Error::other("refused"))),
                Box::new(|_| {}),
                fakes::clock::FakeClock::new(),
            );
            match done.send(code) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    let first = watched(&frames, "the first frame");
    assert!(
        first.starts_with(START),
        "the first chunk starts with the start bytes: {first:?}"
    );
    assert!(
        first
            .windows(b"shortcuts".len())
            .any(|window| window == b"shortcuts"),
        "the first chunk holds the first frame: {first:?}"
    );
    // The cached read cannot answer until the first frame is seen.
    release
        .send(())
        .unwrap_or_else(|err| panic!("release the cached read: {err}"));
    // Both halves sit on one notice row, drawn in row-major order, so
    // `the lists` comes first and `are in` follows it.
    let lists = watched(&frames, "the cached read's notice text");
    assert!(
        lists.ends_with(b"the lists"),
        "the chunk ends with the notice text: {lists:?}"
    );
    let ending = watched(&frames, "the cached read's notice ending");
    assert!(
        ending.ends_with(b"are in"),
        "the chunk ends with the notice ending: {ending:?}"
    );
    assert_eq!(*seen.lock().unwrap(), [crate::Refresh::Cached]);
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
}

#[test]
fn opening_the_picker_asks_stale_through_the_loop() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_in = Arc::clone(&seen);
    let read: crate::ReadModels = Arc::new(move |refresh: crate::Refresh| {
        seen_in.lock().unwrap().push(refresh);
        Ok(crate::Catalogue::default())
    });
    lp.model_reader = crate::catalogue::Reader::new(Some(read));
    let (tx, rx) = mpsc::channel();
    lp.files_out = Some(tx);
    let (_, idle) = mpsc::channel();
    assert_eq!(lp.step(Input::Bytes(vec![0x0c]), &idle), None);
    assert!(lp.app.model_picker_open());
    let answer = rx
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the stale read: {err}"));
    assert!(matches!(answer, Input::Models(Ok(_))));
    assert_eq!(*seen.lock().unwrap(), [crate::Refresh::Stale]);
    // The answer folds with no read owed.
    assert_eq!(lp.step(answer, &idle), None);
    assert_eq!(lp.app.take_reads(), None);
}

#[path = "lib_motion_tests.rs"]
mod motion;

/// One installed model with no levels, named to stand out on the
/// terminal: no other frame draws `qq`.
fn one_model() -> crate::Catalogue {
    crate::Catalogue {
        models: vec![crate::ModelEntry {
            reference: "zz/qq".to_owned(),
            provider: "zz".to_owned(),
            id: "qq".to_owned(),
            levels: Vec::new(),
            default_level: None,
            configured: None,
            roles: Vec::new(),
            name: None,
            price: None,
        }],
        notices: Vec::new(),
        lists: Vec::new(),
    }
}

/// Quits a running terminal: Ctrl+C twice, waiting for 0 with one named
/// deadline.
fn quit(pair: &mut Pair, finished: &mpsc::Receiver<i32>) {
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
}

#[test]
fn with_no_model_the_picker_opens_at_start() {
    let mut pair = open();
    // The watcher drains the terminal past its markers, so later frames
    // never fill the pty: the picker at start, its answered row, then
    // the home chips naming the chosen model.
    let frames = watch(&pair.main, vec![b"Models", b"qq", b"[zz/qq]"]);
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let seam = std::sync::Arc::new(crate::configure_fake::Fake::new(vec![]));
    let (release, held) = mpsc::channel();
    let held = std::sync::Arc::new(std::sync::Mutex::new(held));
    let held_in = std::sync::Arc::clone(&held);
    let released = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let released_in = std::sync::Arc::clone(&released);
    // The first read answers one model once the test releases it after
    // the first frame; a wider read queued behind it answers at once,
    // so no reader outlives the test.
    let read: crate::ReadModels = std::sync::Arc::new(move |_| {
        if !released_in.swap(true, std::sync::atomic::Ordering::SeqCst) {
            held_in
                .lock()
                .unwrap_or_else(|err| panic!("lock: {err}"))
                .recv_timeout(DEADLINE)
                .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for the read release: {err}"));
        }
        Ok(one_model())
    });
    let mut launch = launch();
    launch.model = None;
    launch.models = Some(read);
    launch.configure = Some(seam.clone() as std::sync::Arc<dyn crate::Configure>);
    let (done, finished) = mpsc::channel();
    let clock = fakes::clock::FakeClock::new();
    let (hub, _held) =
        std::os::unix::net::UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    std::thread::Builder::new()
        .name("lib-run".to_owned())
        .spawn(move || {
            let code = super::run(slave, launch, once(hub, hello()), Box::new(|_| {}), clock);
            match done.send(code) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The first frame shows the picker, which opened with no keypress.
    watched(&frames, "the picker at start");
    // Opening saves nothing: the choice does, once Enter chooses it.
    assert!(seam.writes().is_empty());
    release
        .send(())
        .unwrap_or_else(|err| panic!("release: {err}"));
    // The cached read answers, and its frame lists the one model: only
    // then does one Enter choose, as an Enter before the folded answer
    // keeps the picker open, sending nothing.
    watched(&frames, "the answered catalogue");
    pair.main
        .write_all(b"\r")
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    // Choosing writes through the seam before the home chips name the
    // model, so their frame proves the write went out: one named
    // deadline for it.
    watched(&frames, "the chosen home chips");
    assert_eq!(
        seam.writes(),
        vec![(
            PathBuf::from("/w"),
            crate::configure::Layer::Global,
            "model".to_owned(),
            "zz/qq".to_owned()
        )]
    );
    quit(&mut pair, &finished);
}

#[test]
fn with_a_model_home_opens_as_before() {
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (done, finished) = mpsc::channel();
    let clock = fakes::clock::FakeClock::new();
    let (hub, _held) =
        std::os::unix::net::UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    std::thread::Builder::new()
        .name("lib-run".to_owned())
        .spawn(move || {
            let code = super::run(slave, launch(), once(hub, hello()), Box::new(|_| {}), clock);
            match done.send(code) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // Home draws with no picker over it: the first frame names the
    // shortcuts the picker would cover.
    let frames = watch(&pair.main, vec![b"shortcuts" as &[u8]]);
    watched(&frames, "home at start");
    quit(&mut pair, &finished);
}
