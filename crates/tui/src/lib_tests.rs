//! Tests for the loop, and for `run` through a pseudo-terminal.

use super::{Input, Loop, Screen};
use crate::app::App;
use crate::link::Line;
use ratatui::backend::{Backend, CrosstermBackend, TestBackend};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

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
struct Sink(Arc<Mutex<Vec<u8>>>);

impl Sink {
    fn len(&self) -> usize {
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
fn new_loop<B: Backend>(backend: B, tty: Option<File>) -> (Loop<B>, Arc<Mutex<Vec<String>>>) {
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
    };
    (lp, attached)
}

/// Runs `lp` over `inputs`, then with every sender gone.
fn feed<B: Backend>(lp: &mut Loop<B>, inputs: Vec<Input>) -> i32 {
    let (tx, rx) = mpsc::channel();
    for input in inputs {
        tx.send(input).unwrap_or_else(|err| panic!("send: {err}"));
    }
    drop(tx);
    lp.run(&rx)
}

fn hello() -> contract::HubLine {
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
fn command(
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
        .draw(&lp.app)
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
    crate::term::setup(&pair.slave).unwrap_or_else(|err| panic!("setup: {err}"));
    let start = "\x1b[?1049h\x1b[?u\x1b[c";
    assert_eq!(
        read_exact(&pair.main, start.len(), "the start bytes"),
        start.as_bytes()
    );
    super::restore();
    let end = "\x1b[?1049l\x1b[?25h";
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
                PathBuf::from("/w"),
                Box::new(move || Ok((hub, hello))),
                Box::new(|_| {}),
                clock,
            );
            match done.send(code) {
                Ok(()) | Err(_) => {}
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    // The terminal bytes at start: alternate screen, the two queries, then
    // the first frame before anything is written to the master.
    let start = read_exact(
        &pair.main,
        "\x1b[?1049h".len() + "\x1b[?u\x1b[c".len(),
        "the start bytes",
    );
    assert_eq!(start, b"\x1b[?1049h\x1b[?u\x1b[c");
    let frame = read_exact(&pair.main, 10, "the first frame");
    assert!(frame.contains(&b'>'));
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
    let marker = b"\x1b[?1049l\x1b[?25h";
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
                PathBuf::from("/w"),
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
    read_until(&pair.main, b"\x1b[?1049l\x1b[?25h", "the restore bytes");
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
                PathBuf::from("/w"),
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
    // The input line moves to the new last row.
    read_until(&pair.main, b"\x1b[10;1H>", "the input line on row 10");
    pair.main
        .write_all(&[0x03, 0x03])
        .unwrap_or_else(|err| panic!("write: {err}"));
    let code = finished
        .recv_timeout(DEADLINE)
        .unwrap_or_else(|err| panic!("waited {DEADLINE:?} for run to return: {err}"));
    assert_eq!(code, 0);
}
