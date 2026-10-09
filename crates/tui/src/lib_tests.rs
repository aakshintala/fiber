//! Tests for the loop, and for `run` through a pseudo-terminal.

use super::{Input, Loop, Screen};
use crate::app::App;
use crate::keys::Event;
use crate::link::Line;
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
        model: None,
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

/// Reads exactly `n` bytes with one named deadline.
fn read_exact(main: &File, n: usize, what: &str) -> Vec<u8> {
    let mut dup = main.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-read".to_owned())
        .spawn(move || {
            let mut buf = vec![0u8; n];
            let read = dup.read_exact(&mut buf).map(|()| buf);
            match done.send(read) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match finished.recv_timeout(DEADLINE) {
        Ok(Ok(buf)) => buf,
        Ok(Err(err)) => panic!("waited {DEADLINE:?} for {what}: {err}"),
        Err(_) => panic!("waited {DEADLINE:?} for {what}"),
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

/// Reads until `marker` appears with one named deadline, returning
/// everything up to and including it.
pub(super) fn read_until(main: &File, marker: &[u8], what: &str) -> Vec<u8> {
    read_until_with_timeout(main, marker, what, DEADLINE)
}

fn read_until_with_timeout(main: &File, marker: &[u8], what: &str, timeout: Duration) -> Vec<u8> {
    let mut dup = main.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let marker = marker.to_vec();
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-read-until".to_owned())
        .spawn(move || {
            let mut buf = Vec::new();
            let mut byte = [0u8; 1];
            loop {
                use std::io::Read;
                match dup.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        buf.push(byte[0]);
                        if buf.len() >= marker.len()
                            && buf.get(buf.len() - marker.len()..) == Some(marker.as_slice())
                        {
                            break;
                        }
                    }
                }
            }
            match done.send(buf) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match finished.recv_timeout(timeout) {
        Ok(buf) => buf,
        Err(_) => panic!("waited {timeout:?} for {what}"),
    }
}

/// Watches `main` without stopping: sends cumulative bytes through each marker,
/// then keeps reading and discarding so the terminal's output cannot fill.
fn watch(main: &File, markers: Vec<&'static [u8]>) -> Receiver<Vec<u8>> {
    let mut dup = main.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let (done, finished) = mpsc::channel();
    std::thread::Builder::new()
        .name("lib-watch".to_owned())
        .spawn(move || {
            let mut buf = Vec::new();
            let mut at = 0usize;
            let mut byte = [0u8; 1];
            'markers: while at < markers.len() {
                match dup.read(&mut byte) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {
                        buf.push(byte[0]);
                        while at < markers.len()
                            && buf
                                .windows(markers[at].len())
                                .any(|window| window == markers[at])
                        {
                            if done.send(buf.clone()).is_err() {
                                break 'markers;
                            }
                            at += 1;
                        }
                    }
                }
            }
            let mut discard = [0u8; 4096];
            while dup.read(&mut discard).is_ok_and(|read| read > 0) {}
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    finished
}

fn watched_with_timeout(frames: &Receiver<Vec<u8>>, what: &str, timeout: Duration) -> Vec<u8> {
    frames
        .recv_timeout(timeout)
        .unwrap_or_else(|err| panic!("waited {timeout:?} for {what}: {err}"))
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
pub(super) fn new_loop<B: Backend>(
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
        title: crate::osc::Title::default(),
        save: None,
        shape: crate::osc::Shape::default(),
        retry: None,
    };
    (lp, attached)
}

/// Runs `lp` over `inputs`, then with every sender gone.
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
        read_until(&pair.main, b"END", "the kitty push"),
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
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    let start = "\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
    assert_eq!(
        read_exact(&pair.main, start.len(), "the start bytes"),
        start.as_bytes()
    );
    crate::restore();
    let end = "\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";
    assert_eq!(
        read_exact(&pair.main, end.len(), "the restore bytes"),
        end.as_bytes()
    );
    assert!(is_cooked(
        &rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"))
    ));
}

#[test]
fn resize_redraws_at_the_new_size() {
    let pair = open();
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
    assert_eq!(rows.get(8).copied(), Some("> hi"));
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
    // The terminal bytes at start: alternate screen, bracketed paste, the mouse
    // modes, the
    // two queries, then the first frame before anything is written to the
    // master.
    let expected =
        b"\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
    let start = read_exact(&pair.main, expected.len(), "the start bytes");
    assert_eq!(start, expected);
    // On home the input line is not on the last row: the first frame is
    // read through the placeholder, whose letters are written together.
    let frames = super::reconnect_tests::watch(&pair.main, vec![
        b"shortcuts",
        b"Press Ctrl+C again to",
        b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h",
    ]);
    super::reconnect_tests::watched(&frames, "the first frame");
    // The slave is in raw mode while running.
    let raw = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&before));
    assert!(!is_cooked(&raw));
    // Ctrl+C twice quits with 0 while the reader is still blocked: the
    // master stays open and nothing is closed to wake it. The first
    // press must visibly arm first: waiting for its hint proves the
    // reader delivered a byte and the loop drew again. The watcher is the
    // only pty reader and keeps draining while the second press quits with
    // no frame between, so the return wait cannot stall behind a full pty.
    pair.main
        .write_all(&[0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    // The armed frame foots the quit hint, which no earlier frame
    // holds. Only its first run is matched: the incremental redraw
    // splits the hint around the cells the unarmed foot already holds.
    super::reconnect_tests::watched(&frames, "the armed quit hint");
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
    let marker =
        b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";
    let tail = super::reconnect_tests::watched(&frames, "the restore bytes");
    assert_eq!(
        tail.get(tail.len().saturating_sub(marker.len())..),
        Some(marker.as_slice())
    );
}

#[test]
fn run_shows_a_failed_connect_and_still_quits_restored() {
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
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
    read_until(&pair.main, b"refused", "the connect notice");
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
    read_until(
        &pair.main,
        b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h",
        "the restore bytes",
    );
    assert!(is_cooked(
        &rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"))
    ));
    // A second restore writes nothing: the next bytes are the test's own.
    crate::restore();
    (&pair.slave)
        .write_all(b"mark")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert_eq!(read_exact(&pair.main, 4, "the mark"), b"mark");
}

#[test]
fn run_redraws_on_sigwinch_at_the_new_size() {
    let mut pair = open();
    let slave = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
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
    read_until(&pair.main, b"refused", "the connect notice");
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
    read_until(&pair.main, b"\x1b[10;1H", "the move to row 10");
    let next = read_until(&pair.main, "↓".as_bytes(), "the foot hint on row 10");
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

#[test]
fn the_screen_shows_the_cursor_at_the_draft() {
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), None);
    feed(&mut lp, vec![Input::Bytes(b"ab\x1b[D".to_vec())]);
    let backend = lp.screen.backend_mut();
    assert!(backend.cursor_visible());
    backend.assert_cursor_position((3, 10));
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
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    let start = "\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
    read_exact(&pair.main, start.len(), "the start bytes");
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
    // flags.
    let restore = "\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";
    let echoed = read_until(&pair.main, restore.as_bytes(), "the restore bytes");
    assert!(echoed.ends_with(restore.as_bytes()));
    // In between, the cooked terminal echoed the program's line.
    let resumed = "typed\r\n\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[>1u\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n";
    assert_eq!(
        read_until(&pair.main, resumed.as_bytes(), "the resume bytes"),
        resumed.as_bytes()
    );
    let title = crate::osc::title("fiber");
    assert_eq!(read_exact(&pair.main, title.len(), "the title"), title);
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
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    // Drain what the terminal is sent, so no write blocks.
    let mut main = pair
        .main
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    std::thread::Builder::new()
        .name("lib-drain".to_owned())
        .spawn(move || io::copy(&mut main, &mut io::sink()))
        .unwrap_or_else(|err| panic!("spawn: {err}"));
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
    assert_eq!(read_exact(&pair.main, osc.len(), "the OSC 52 bytes"), osc);
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

/// The terminal's restore bytes, after its last frame.
const RESTORE: &[u8] =
    b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?2031l\x1b]22;default\x1b\\\x1b[23;2t\x1b[?1049l\x1b[?25h";

/// Runs the terminal on a pty with `hub` as its hub stream: the pty pair,
/// and the exit code once it quits.
fn spawn_run(hub: UnixStream) -> (Pair, mpsc::Receiver<i32>) {
    let pair = open();
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
                launch(),
                once(hub, hello),
                Box::new(|_| {}),
                fakes::clock::FakeClock::new(),
            );
            match done.send(code) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    (pair, finished)
}

/// Reads the start bytes and the first frame.
fn first_frame(pair: &Pair) {
    let expected =
        b"\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
    assert_eq!(
        read_exact(&pair.main, expected.len(), "the start bytes"),
        expected
    );
    read_until(&pair.main, b"shortcuts", "the first frame");
}

#[test]
fn run_prints_a_resume_line_per_live_session_after_restoring() {
    let (hub, held) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let (mut pair, finished) = spawn_run(hub);
    first_frame(&pair);
    crate::link::write_line(&held, &hub_status("s_aaaaaaaaaaaaaaaa", "idle"))
        .unwrap_or_else(|err| panic!("write: {err}"));
    // The test waits for the session's name on screen before quitting.
    read_until(&pair.main, b"parser", "the session row");
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
    read_until(&pair.main, RESTORE, "the restore bytes");
    let line = b"s_aaaaaaaaaaaaaaaa  fiber resume s_aaaaaaaaaaaaaaaa";
    let tail = read_until(&pair.main, line, "the resume line");
    assert_eq!(
        tail.get(tail.len().saturating_sub(line.len())..),
        Some(line.as_slice())
    );
    drop(held);
}

#[test]
fn run_close_all_sends_summary_and_close_now_and_prints_no_line_for_it() {
    let (hub, held) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let write = held.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut pair, finished) = spawn_run(hub);
    first_frame(&pair);
    crate::link::write_line(&write, &hub_status("s_aaaaaaaaaaaaaaaa", "streaming"))
        .unwrap_or_else(|err| panic!("write: {err}"));
    read_until(&pair.main, b"parser", "the session row");
    // Ctrl+C twice asks while the session works, and `c` closes all:
    // one write, so the loop asks before it closes.
    pair.main
        .write_all(&[0x03, 0x03, b'c'])
        .unwrap_or_else(|err| panic!("write: {err}"));
    pair.main
        .flush()
        .unwrap_or_else(|err| panic!("flush: {err}"));
    // Every read runs under DEADLINE through `command`: the far end
    // reads the close lines the quit question sent.
    let hub = BufReader::new(held);
    let (hub, feed) = command(hub, "the feed command");
    assert_eq!(feed["command"], "feed");
    let (hub, recent) = command(hub, "the recent command");
    assert_eq!(recent["command"], "recent");
    let (hub, subscribe) = command(hub, "the subscribe command");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["args"]["level"], "summary");
    let (_, close) = command(hub, "the close command");
    assert_eq!(close["command"], "close");
    assert_eq!(close["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(close["args"], serde_json::json!({"now": true}));
    assert_eq!(
        finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}")),
        0
    );
    read_until(&pair.main, RESTORE, "the restore bytes");
    // A closed session prints no resume line: the mark bounds the bytes
    // after the restore, and none names the session.
    (&pair.slave)
        .write_all(b"ENDMARK")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let tail = read_until(&pair.main, b"ENDMARK", "the mark");
    assert!(!tail.windows(12).any(|window| window == b"fiber resume"));
}

#[test]
fn run_close_all_with_a_lost_hub_still_prints_the_resume_line() {
    let (hub, held) = UnixStream::pair().unwrap_or_else(|err| panic!("pair: {err}"));
    let write = held.try_clone().unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut pair, finished) = spawn_run(hub);
    first_frame(&pair);
    crate::link::write_line(&write, &hub_status("s_aaaaaaaaaaaaaaaa", "streaming"))
        .unwrap_or_else(|err| panic!("write: {err}"));
    read_until(&pair.main, b"parser", "the session row");
    // The hub is lost fast: its far end drops after the status draws, and
    // the test waits for `lost` on screen.
    drop(write);
    drop(held);
    read_until(&pair.main, b"lost", "the lost notice");
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
    read_until(&pair.main, RESTORE, "the restore bytes");
    let line = b"s_aaaaaaaaaaaaaaaa  fiber resume s_aaaaaaaaaaaaaaaa";
    let tail = read_until(&pair.main, line, "the resume line");
    assert_eq!(
        tail.get(tail.len().saturating_sub(line.len())..),
        Some(line.as_slice())
    );
}

#[test]
fn run_writes_the_title_once_until_it_changes() {
    let pair = open();
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
    let written = read_until(&pair.main, b"ENDMARK", "the mark");
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
        title: crate::osc::Title::default(),
        save: None,
        shape: crate::osc::Shape::default(),
        retry: None,
    };
    // No hub: a frame fetches no history, so nothing arrives here.
    let (_hub, idle) = mpsc::channel();
    assert_eq!(lp.step(Input::Bytes(b"@".to_vec()), &idle), None);
    assert!(lp.search.is_some());
    let (generation, result) = next_result(&rx, "the listing's first search");
    assert_eq!(generation, lp.app.generation());
    assert_eq!(lp.step(Input::Files { generation, result }, &idle), None);
    let shown = lp.app.completions().map(|c| c.lines).unwrap_or_default();
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
            Input::Bytes(_)
            | Input::Hub(_)
            | Input::Connected(..)
            | Input::ConnectFailed(_)
            | Input::Disconnected
            | Input::Resize
            | Input::Files { .. }
            | Input::Models(_)
            | Input::Image { .. } => panic!("a pause sent something else"),
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
    const START: &[u8] =
        b"\x1b[?1049h\x1b[22;2t\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b]11;?\x1b\\\x1b[?2031h\x1b[?996n\x1b[?u\x1b[c";
    let frames = watch(
        &pair.main,
        vec![START, b"shortcuts", b"the lists", b"are in"],
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
            }],
            notices: vec!["the lists are in".to_owned()],
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
    let mut output = watched_with_timeout(&frames, "terminal start", DEADLINE);
    assert_eq!(output, START);
    output = watched_with_timeout(&frames, "the first frame", DEADLINE);
    assert!(
        output
            .windows(b"shortcuts".len())
            .any(|window| window == b"shortcuts")
    );
    // The cached read cannot answer until the first frame is seen. Its
    // notice may already be in the cumulative output when this wait starts.
    release
        .send(())
        .unwrap_or_else(|err| panic!("release the cached read: {err}"));
    // The two text runs are separated by cursor and style controls in the
    // terminal stream, so both are checked in the cumulative output.
    output = watched_with_timeout(&frames, "the cached read's notice text", DEADLINE);
    assert!(
        output
            .windows(b"the lists".len())
            .any(|window| window == b"the lists")
    );
    output = watched_with_timeout(&frames, "the cached read's notice ending", DEADLINE);
    assert!(
        output
            .windows(b"are in".len())
            .any(|window| window == b"are in")
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
