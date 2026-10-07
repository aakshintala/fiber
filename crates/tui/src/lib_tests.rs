//! Tests for the loop, and for `run` through a pseudo-terminal.

use super::{Input, Loop, Screen};
use crate::app::App;
use crate::keys::{Event, Key};
use crate::link::Line;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

/// The launch description `run` tests start from: `/w`, outside git.
fn launch() -> super::Launch {
    super::Launch {
        workspace: PathBuf::from("/w"),
        project: "-w".to_owned(),
        git: false,
        hover: true,
        version: "0.0.1".to_owned(),
        model: None,
        thinking: None,
        logo_glyph: "⌇".to_owned(),
    }
}

/// One named wall-clock deadline for every blocking wait.
const DEADLINE: Duration = Duration::from_secs(10);

/// A pty pair: the main side and the slave as a file.
struct Pair {
    /// The main side.
    main: File,
    /// The slave side, the injected tty.
    slave: File,
}

/// Opens a pty pair with a 60x12 window.
fn open() -> Pair {
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
fn read_until(main: &File, marker: &[u8], what: &str) -> Vec<u8> {
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
    match finished.recv_timeout(DEADLINE) {
        Ok(buf) => buf,
        Err(_) => panic!("waited {DEADLINE:?} for {what}"),
    }
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
        pointer: crate::mouse::Pointer::default(),
        hover: true,
        var: Box::new(|_| None),
        copy_command: None,
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
    assert_eq!(lp.parser.feed(b"\x1b"), vec![Event::Key(Key::Esc)]);
}

#[test]
fn the_first_kitty_reply_pushes_the_flags_once() {
    let pair = open();
    let tty = pair
        .slave
        .try_clone()
        .unwrap_or_else(|err| panic!("dup: {err}"));
    let (mut lp, _) = new_loop(TestBackend::new(60, 12), Some(tty));
    // A second reply pushes nothing more: the next bytes on the tty are
    // the marker written after it.
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
        b"\x1b[>1uEND"
    );
    assert_eq!(crate::term::KITTY_PUSH, b"\x1b[>1u");
    // Once pushed, Esc is `CSI 27u`: a lone ESC ending a read is held.
    assert!(lp.parser.feed(b"\x1b").is_empty());
    assert_eq!(lp.parser.feed(b"[27u"), vec![Event::Key(Key::Esc)]);
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
    assert_eq!(start["args"]["content"][0]["text"], "hi");
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
    let (_, subscribe) = command(reader, "the subscribe command");
    assert_eq!(subscribe["command"], "subscribe");
    assert_eq!(subscribe["session_id"], "s_aaaaaaaaaaaaaaaa");
    assert_eq!(subscribe["args"]["level"], "full");
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
    let start = "\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[?u\x1b[c";
    assert_eq!(
        read_exact(&pair.main, start.len(), "the start bytes"),
        start.as_bytes()
    );
    super::restore();
    let end = "\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h";
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
    // The input line draws on the new last row; the test backend keeps its
    // 60x12 buffer, cleared by the resize.
    let shown = crate::view::text(lp.screen.terminal.backend().inner.buffer());
    let rows: Vec<&str> = shown.lines().collect();
    assert_eq!(rows.get(9).copied(), Some("> hi"));
    assert!(rows.iter().skip(10).all(|row| row.is_empty()));
}

#[test]
fn the_screen_reports_the_tty_size_not_the_backends() {
    // ratatui clears a fixed viewport at the size its backend reports;
    // the backend's own answer is never asked.
    let mut screen =
        Screen::new(TestBackend::new(60, 12), 40, 10).unwrap_or_else(|err| panic!("screen: {err}"));
    let size = |screen: &Screen<TestBackend>| {
        screen
            .terminal
            .size()
            .unwrap_or_else(|err| panic!("size: {err}"))
    };
    assert_eq!(size(&screen), ratatui::layout::Size::new(40, 10));
    screen
        .resize(30, 8)
        .unwrap_or_else(|err| panic!("resize: {err}"));
    assert_eq!(size(&screen), ratatui::layout::Size::new(30, 8));
    let window = ratatui::backend::Backend::window_size(screen.terminal.backend_mut())
        .unwrap_or_else(|err| panic!("window: {err}"));
    assert_eq!(window.columns_rows, ratatui::layout::Size::new(30, 8));
}

/// A backend that records each call it gets.
#[derive(Default)]
struct Calls(Vec<String>);

impl Backend for Calls {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.0.push(format!("draw {}", content.count()));
        Ok(())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.push("hide".to_owned());
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.push("show".to_owned());
        Ok(())
    }

    fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
        self.0.push("get".to_owned());
        Ok(ratatui::layout::Position::new(3, 4))
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> Result<(), Self::Error> {
        let position = position.into();
        self.0.push(format!("set {} {}", position.x, position.y));
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.push("clear".to_owned());
        Ok(())
    }

    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> Result<(), Self::Error> {
        self.0.push(format!("clear {clear_type}"));
        Ok(())
    }

    fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
        Ok(ratatui::layout::Size::new(1, 1))
    }

    fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
        Ok(ratatui::backend::WindowSize {
            columns_rows: ratatui::layout::Size::new(1, 1),
            pixels: ratatui::layout::Size::new(1, 1),
        })
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.push("flush".to_owned());
        Ok(())
    }
}

#[test]
fn the_tty_sized_backend_passes_every_call_but_the_size_through() {
    let mut backend = super::TtySized {
        inner: Calls::default(),
        size: ratatui::layout::Size::new(40, 10),
    };
    let cell = ratatui::buffer::Cell::default();
    let ok = |result: Result<(), std::convert::Infallible>| result.unwrap_or(());
    ok(backend.draw([(0, 0, &cell), (1, 0, &cell)].into_iter()));
    ok(backend.hide_cursor());
    ok(backend.show_cursor());
    let position = backend
        .get_cursor_position()
        .unwrap_or_else(|err| match err {});
    assert_eq!(position, ratatui::layout::Position::new(3, 4));
    ok(backend.set_cursor_position((5, 6)));
    ok(backend.clear());
    ok(backend.clear_region(ratatui::backend::ClearType::CurrentLine));
    ok(backend.flush());
    assert_eq!(
        backend.inner.0,
        [
            "draw 2",
            "hide",
            "show",
            "get",
            "set 5 6",
            "clear",
            "clear CurrentLine",
            "flush"
        ]
    );
    let size = backend.size().unwrap_or_else(|err| match err {});
    assert_eq!(size, ratatui::layout::Size::new(40, 10));
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
            let code = super::run(
                slave,
                launch(),
                Box::new(move || Ok((hub, hello))),
                Box::new(|_| {}),
                clock,
            );
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
        b"\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[?u\x1b[c";
    let start = read_exact(&pair.main, expected.len(), "the start bytes");
    assert_eq!(start, expected);
    // On home the input line is not on the last row: the first frame is
    // read through the placeholder, whose letters are written together.
    read_until(&pair.main, b"shortcuts", "the first frame");
    // The slave is in raw mode while running.
    let raw = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&before));
    assert!(!is_cooked(&raw));
    // Ctrl+C twice quits with 0 while the reader is still blocked: the
    // master stays open and nothing is closed to wake it.
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
    let after = rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"));
    assert!(is_cooked(&after));
    // After the last frame the output holds the restore bytes.
    let marker =
        b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h";
    let tail = read_until(&pair.main, marker, "the restore bytes");
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
        b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h",
        "the restore bytes",
    );
    assert!(is_cooked(
        &rustix::termios::tcgetattr(&pair.slave).unwrap_or_else(|err| panic!("attr: {err}"))
    ));
    // A second restore writes nothing: the next bytes are the test's own.
    super::restore();
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
    let (mut reader, asked) = command(reader, "the commands command");
    assert_eq!(asked["command"], "commands");
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
    let backend = lp.screen.terminal.backend_mut();
    assert!(backend.inner.cursor_visible());
    backend.inner.assert_cursor_position((3, 11));
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

/// A reader on a pipe standing in for the tty: the reader, the pipe's
/// write end and the loop's channel.
fn piped_reader() -> (super::Reader, io::PipeWriter, mpsc::Receiver<Input>) {
    let (read, write) = io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let tty = File::from(std::os::fd::OwnedFd::from(read));
    let (tx, rx) = mpsc::channel();
    let reader = super::Reader::spawn(&tty, tx).unwrap_or_else(|| panic!("the reader started"));
    (reader, write, rx)
}

/// The next bytes the reader sends, with one deadline.
fn next_bytes(rx: &mpsc::Receiver<Input>, what: &str) -> Vec<u8> {
    match rx.recv_timeout(DEADLINE) {
        Ok(Input::Bytes(bytes)) => bytes,
        Ok(_) => panic!("{what}: not bytes"),
        Err(err) => panic!("waited {DEADLINE:?} for {what}: {err}"),
    }
}

/// Pauses `reader` on a thread with one deadline, handing it back.
fn paused(reader: super::Reader) -> super::Reader {
    within("the pause to return", move || {
        let mut reader = reader;
        reader.pause();
        reader
    })
}

#[test]
fn a_paused_reader_holds_the_ttys_bytes_until_resumed() {
    let (reader, mut tty, rx) = piped_reader();
    tty.write_all(b"a")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert_eq!(next_bytes(&rx, "the first byte"), b"a");
    let reader = paused(reader);
    // Pause returned only once the reader parked.
    assert!(reader.gate.lock().parked);
    tty.write_all(b"b")
        .unwrap_or_else(|err| panic!("write: {err}"));
    // Parked on the condition variable, it reads nothing.
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    assert!(reader.gate.lock().parked);
    reader.resume();
    assert_eq!(next_bytes(&rx, "the byte after resume"), b"b");
    assert!(!reader.gate.lock().parked);
    // A second pause and resume works the same.
    let reader = paused(reader);
    tty.write_all(b"c")
        .unwrap_or_else(|err| panic!("write: {err}"));
    assert!(matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
    reader.resume();
    assert_eq!(next_bytes(&rx, "the byte after the second resume"), b"c");
}

#[test]
fn bytes_read_before_the_pause_are_sent_not_lost() {
    let (reader, mut tty, rx) = piped_reader();
    tty.write_all(b"xy")
        .unwrap_or_else(|err| panic!("write: {err}"));
    let reader = paused(reader);
    reader.resume();
    let mut got = Vec::new();
    while got.len() < 2 {
        got.extend(next_bytes(&rx, "the bytes written before the pause"));
    }
    assert_eq!(got, b"xy");
}

#[test]
fn pause_on_an_ended_reader_returns_at_once() {
    let (reader, tty, rx) = piped_reader();
    // The tty's end ends the reader: its sender drops.
    drop(tty);
    match rx.recv_timeout(DEADLINE) {
        Err(mpsc::RecvTimeoutError::Disconnected) => {}
        Ok(_) => panic!("bytes instead of the reader's end"),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("waited {DEADLINE:?} for the reader to end")
        }
    }
    let reader = paused(reader);
    assert!(reader.gate.lock().ended);
    assert!(!reader.gate.lock().parked);
}

#[test]
fn a_reader_whose_channel_closed_ends() {
    let (reader, mut tty, rx) = piped_reader();
    drop(rx);
    tty.write_all(b"a")
        .unwrap_or_else(|err| panic!("write: {err}"));
    // Its send fails, and it records its end.
    let state = reader.gate.lock();
    let (state, waited) = reader
        .gate
        .changed
        .wait_timeout_while(state, DEADLINE, |state| !state.ended)
        .unwrap_or_else(|err| panic!("lock: {err}"));
    assert!(
        !waited.timed_out(),
        "waited {DEADLINE:?} for the reader to end"
    );
    drop(state);
    // Pause on it returns at once.
    let reader = paused(reader);
    assert!(!reader.gate.lock().parked);
}

#[test]
fn hand_over_gives_the_terminal_and_its_input_to_the_program() {
    let mut pair = open();
    crate::term::setup(&pair.slave, true).unwrap_or_else(|err| panic!("setup: {err}"));
    let start = "\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[?u\x1b[c";
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
    let restore =
        "\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h";
    let echoed = read_until(&pair.main, restore.as_bytes(), "the restore bytes");
    assert!(echoed.ends_with(restore.as_bytes()));
    // In between, the cooked terminal echoed the program's line.
    let resumed =
        "typed\r\n\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[>1u";
    assert_eq!(
        read_until(&pair.main, resumed.as_bytes(), "the resume bytes"),
        resumed.as_bytes()
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

#[test]
fn pause_on_a_reader_already_parked_returns_at_once() {
    let (_woken, wake) = io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
    let gate = Arc::new(super::Gate::default());
    gate.lock().parked = true;
    let reader = paused(super::Reader { gate, wake });
    assert!(reader.gate.lock().paused);
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
    let shown = crate::view::text(lp.screen.terminal.backend().inner.buffer());
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
    let osc = b"\x1b]52;c;bGV0IGEgPSAxOw==\x07";
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
    let shown = crate::view::text(lp.screen.terminal.backend().inner.buffer());
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
    b"\x1b[<u\x1b[?2004l\x1b[?1003l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?1049l\x1b[?25h";

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
                Box::new(move || Ok((hub, hello))),
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
        b"\x1b[?1049h\x1b[?2004h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?1003h\x1b[?u\x1b[c";
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
