//! The one pseudo-terminal driver for the binary-level screen tests
//! (`docs/tui.md`, "Look"; `docs/testing.md`, "Screens"): the real
//! binary on a sized pty, a reader draining the master from the first
//! frame to end of file, one-deadline waits over a channel, and a screen
//! model rebuilding the `vt100` grid.
//!
//! `terminal.rs`, `look.rs`, `quit_after_completed.rs` and
//! `model_picker_keys.rs` all drive the binary through this module.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsStr;
use std::fs;
use std::io::{self, ErrorKind};
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::Watchdog;
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::pty;

use super::{Deadline, Setup, expired, kill_group_detached, kill_pid};

/// The driver's default terminal size: a test may pick the size its
/// layout needs (the look tests use 160 by 48).
pub(crate) const COLS: u16 = 120;
pub(crate) const ROWS: u16 = 32;

/// Kitty's key-flags push: the loop writes it once its key parser is in
/// kitty mode (`crates/tui/src/event_loop.rs`), so input after it is
/// never encoded before the parser can take it.
pub(crate) const KITTY_PUSH: &[u8] = b"\x1b[>1u";
/// Mouse-motion enable: the terminal writes it with the other
/// mode-enable sequences, which never reach the cells.
pub(crate) const MOTION: &[u8] = b"\x1b[?1003h";
/// The start of an OSC 2 window-title write: the first frame's proof.
pub(crate) const TITLE: &[u8] = b"\x1b]2;";
/// The home title, written when no session is open.
pub(crate) const HOME_TITLE: &[u8] = b"\x1b]2;fiber\x07";
/// The start of the attention waiting title, `! fiber · <kind>`: it
/// appears only once the hub's `attention` line arrives and the session's
/// waiting row is listed (`crates/tui/src/app/attention.rs`).
pub(crate) const WAITING_TITLE: &[u8] = "\x1b]2;! fiber \u{b7} ".as_bytes();
/// The title the terminal shows once a turn finishes
/// (`crates/tui/src/app/attention.rs`, `crates/tui/src/osc.rs`):
/// session-side state reaching the terminal outside the cells.
pub(crate) const FINISHED_TITLE: &[u8] = "\x1b]2;\u{2713} fiber \u{b7} finished\x07".as_bytes();

/// The reader draining a pty master: for each chunk it answers the
/// binary's capability queries, publishes the chunk to the shared state,
/// and wakes the test once, from the spawn until it is stopped or the
/// master ends, so the terminal's output queue never fills while the
/// test runs (`docs/testing.md`, "Screens").
pub(crate) struct Reader {
    stop: std::io::PipeWriter,
    done: mpsc::Receiver<()>,
    deadline: Deadline,
}

impl Reader {
    /// Drains `master` on a thread, sharing `shared` with the test and
    /// answering queries through `writer`, and returns the reader with
    /// one wake per chunk appended. The thread ends on end of file, a
    /// read error, a failed reply write, the stop pipe, or the poll
    /// timing out at the deadline: it never ticks, and every thread this
    /// harness spawns ends by the test's deadline.
    pub(crate) fn start(
        master: fs::File,
        shared: Arc<Mutex<Shared>>,
        writer: Writer,
        deadline: Deadline,
    ) -> (Self, mpsc::Receiver<()>) {
        let (stop_read, stop_write) = std::io::pipe().unwrap();
        let (wake, wakes) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let master = OwnedFd::from(master);
        thread::Builder::new()
            .spawn(move || Self::drain(master, stop_read, shared, writer, wake, done, deadline))
            .unwrap();
        (
            Self {
                stop: stop_write,
                done: finished,
                deadline,
            },
            wakes,
        )
    }

    /// Drains `master` on a thread as in [`Reader::start`], sharing the
    /// run's state instead of a new one and dropping its wakes: nothing
    /// reads them after `wait` restores the drain.
    pub(crate) fn start_on(
        master: fs::File,
        shared: Arc<Mutex<Shared>>,
        writer: Writer,
        deadline: Deadline,
    ) -> Self {
        let (reader, wakes) = Self::start(master, shared, writer, deadline);
        drop(wakes);
        reader
    }

    /// Polls the master and the stop pipe with the deadline's remaining
    /// time as the timeout. For each chunk the thread takes the `Shared`
    /// lock and, in order: computes the chunk's query replies, writes
    /// them through the shared writer, and only then publishes the
    /// chunk. A waiter therefore never sees a query whose reply was not
    /// written: either the reply is on the master and the chunk is
    /// published, or the chunk is never published and every wait fails
    /// on the terminal ending. Lock order is always `Shared` then
    /// `Writer`; `Run::write` takes only `Writer`, so neither path
    /// deadlocks. Nothing in this critical section panics: an expired
    /// reply write is an error that ends the reader, so `Shared` is
    /// never poisoned and waiters are always woken.
    fn drain(
        master: OwnedFd,
        stop: std::io::PipeReader,
        shared: Arc<Mutex<Shared>>,
        writer: Writer,
        wake: mpsc::Sender<()>,
        done: mpsc::Sender<()>,
        deadline: Deadline,
    ) {
        let stop_fd = stop.as_fd();
        let mut fds = [
            PollFd::new(&master, PollFlags::IN),
            PollFd::new(&stop_fd, PollFlags::IN),
        ];
        loop {
            let left = deadline.left();
            let timeout = Timespec {
                tv_sec: i64::try_from(left.as_secs()).unwrap_or(i64::MAX),
                tv_nsec: left.subsec_nanos().into(),
            };
            match rustix::event::poll(&mut fds, Some(&timeout)) {
                Ok(0) => break,
                Ok(_) => {
                    if !fds[1].revents().is_empty() {
                        break;
                    }
                    if !fds[0].revents().is_empty() {
                        let mut chunk = [0u8; 4096];
                        match rustix::io::read(&master, &mut chunk) {
                            Ok(0) => {
                                end(&shared);
                                wake.send(()).unwrap_or(());
                                break;
                            }
                            Ok(n) => {
                                let published = {
                                    let mut guard = shared.lock().unwrap();
                                    let replies = guard.replies(&chunk[..n]);
                                    // A chunk is published only after its
                                    // replies are written: no replies, or
                                    // the write went through.
                                    let written = replies.is_empty()
                                        || write_within(&writer, &replies, deadline).is_ok();
                                    if written {
                                        guard.publish(&chunk[..n]);
                                    } else {
                                        guard.ended = true;
                                    }
                                    written
                                };
                                wake.send(()).unwrap_or(());
                                if !published {
                                    break;
                                }
                            }
                            Err(err) if err == rustix::io::Errno::INTR => {}
                            Err(_) => {
                                end(&shared);
                                wake.send(()).unwrap_or(());
                                break;
                            }
                        }
                    }
                }
                Err(rustix::io::Errno::INTR) => {}
                Err(_) => break,
            }
            if left.is_zero() {
                break;
            }
        }
        // One message for `ended` and one for `stop`: a test proves
        // the thread ended on its own before stopping it.
        done.send(()).unwrap_or(());
        done.send(()).unwrap_or(());
    }

    /// Waits up to `within` for the reader thread to end on its own,
    /// without sending any stop signal: true when it already finished.
    /// A prompt thread ends in milliseconds; the bound is wall-clock so
    /// a broken deadline or end-of-file path fails loudly instead of
    /// hanging the suite.
    pub(crate) fn ended(&self, within: Duration) -> bool {
        self.done.recv_timeout(within).is_ok()
    }

    /// Wakes the reader and waits for its thread, within
    /// `deadline.cleanup()`: true when the thread finished. It never
    /// blocks past the deadline, so a panic mid-test stops the reader as
    /// well as killing the group.
    pub(crate) fn stop(mut self) -> bool {
        use std::io::Write;
        self.stop.write_all(b"x").unwrap_or(());
        self.done.recv_timeout(self.deadline.cleanup()).is_ok()
    }
}

/// Marks the shared state ended and leaves its content as it was: end of
/// file or a read error ends the drain without publishing anything.
fn end(shared: &Mutex<Shared>) {
    shared.lock().unwrap().ended = true;
}

/// A pseudo-terminal: the main side the reader drains, and the terminal
/// side the child is born on, sized before the spawn.
pub(crate) struct Terminal {
    pub(crate) main: OwnedFd,
    pub(crate) terminal: fs::File,
}

/// Opens a `cols` by `rows` pty. The main side stays open while the run
/// uses the terminal side, and is never inherited: a hub `fiber` starts
/// would hold the master open.
pub(crate) fn open(cols: u16, rows: u16) -> Terminal {
    let main = pty::openpt(pty::OpenptFlags::RDWR | pty::OpenptFlags::NOCTTY).unwrap();
    rustix::io::fcntl_setfd(&main, rustix::io::FdFlags::CLOEXEC).unwrap();
    pty::grantpt(&main).unwrap();
    pty::unlockpt(&main).unwrap();
    let name = pty::ptsname(&main, Vec::new()).unwrap();
    let path = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
    let terminal = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    rustix::termios::tcsetwinsize(
        &terminal,
        rustix::termios::Winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    Terminal { main, terminal }
}

impl Terminal {
    /// The terminal side as standard IO.
    pub(crate) fn stdio(&self) -> Stdio {
        Stdio::from(self.terminal.try_clone().unwrap())
    }
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group. Dropping the watchdog kills the group; standing it down exits
/// quietly once the child is reaped and the group is empty.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop, through the guarded helper, which
/// refuses a group id of 1 or less (`docs/testing.md`, "Waits and
/// timeouts").
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        kill_group_detached(self.0, "KILL");
    }
}

/// A colour a grid cell holds: the terminal's default, a 256-palette
/// entry, or a truecolour triple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Colour {
    /// Unpainted: SGR 39 or 49, how a terminal shows its default.
    Default,
    /// SGR `38;5;n` or `48;5;n`.
    Indexed(u8),
    /// SGR `38;2;r;g;b` or `48;2;r;g;b`.
    Rgb(u8, u8, u8),
}

impl From<vt100::Color> for Colour {
    fn from(color: vt100::Color) -> Self {
        match color {
            vt100::Color::Default => Self::Default,
            vt100::Color::Idx(entry) => Self::Indexed(entry),
            vt100::Color::Rgb(red, green, blue) => Self::Rgb(red, green, blue),
        }
    }
}

/// One grid cell: its symbol with the pen that wrote it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cell {
    pub(crate) symbol: String,
    pub(crate) fg: Colour,
    pub(crate) bg: Colour,
    pub(crate) dim: bool,
    pub(crate) bold: bool,
}

impl Cell {
    /// The `vt100` cell as a [`Cell`]: a cell with no content reads as a
    /// blank cell, which is how a terminal shows unwritten cells.
    fn of(cell: &vt100::Cell) -> Self {
        let contents = cell.contents();
        Self {
            symbol: if contents.is_empty() {
                " ".to_owned()
            } else {
                contents.to_owned()
            },
            fg: cell.fgcolor().into(),
            bg: cell.bgcolor().into(),
            dim: cell.dim(),
            bold: cell.bold(),
        }
    }
}

/// The screen rebuilt from the output so far: its text and rows, each
/// cell's colours and attributes, the cursor position, and the
/// alternate-screen and hidden-cursor flags.
#[derive(Clone, Debug)]
pub(crate) struct Grid {
    pub(crate) contents: String,
    pub(crate) rows: Vec<String>,
    /// The cursor as (row, column).
    pub(crate) cursor: (u16, u16),
    pub(crate) alternate_screen: bool,
    pub(crate) hide_cursor: bool,
    screen: vt100::Screen,
}

impl Grid {
    /// One snapshot of the parser's screen.
    fn of(parser: &vt100::Parser) -> Self {
        let screen = parser.screen();
        let (_, cols) = screen.size();
        Self {
            contents: screen.contents(),
            rows: screen.rows(0, cols).collect(),
            cursor: screen.cursor_position(),
            alternate_screen: screen.alternate_screen(),
            hide_cursor: screen.hide_cursor(),
            screen: screen.clone(),
        }
    }

    /// The cell at column `x`, row `y`. Panics past the grid's edges.
    pub(crate) fn cell(&self, x: u16, y: u16) -> Cell {
        match self.screen.cell(y, x) {
            Some(found) => Cell::of(found),
            None => panic!("cell ({x}, {y}) is past the grid's edges"),
        }
    }
}

/// What the reader shares with the test: every byte read from the master
/// since spawn, the parser and grid rebuilt from it, the tail of a query
/// still arriving, a resize the next chunk has not applied yet, and
/// whether the reader reached the end.
pub(crate) struct Shared {
    output: Vec<u8>,
    parser: vt100::Parser,
    grid: Grid,
    queries: Vec<u8>,
    pending_size: Option<(u16, u16)>,
    ended: bool,
}

impl Shared {
    /// A blank `cols` by `rows` screen with no output.
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        let parser = vt100::Parser::new(rows, cols, 0);
        let grid = Grid::of(&parser);
        Self {
            output: Vec::new(),
            parser,
            grid,
            queries: Vec::new(),
            pending_size: None,
            ended: false,
        }
    }

    /// The chunk's query replies, in stream order. Only the query tail
    /// is updated: the chunk itself is published after its replies are
    /// written.
    pub(crate) fn replies(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.queries.extend_from_slice(bytes);
        query_replies(&mut self.queries)
    }

    /// Publishes the chunk whose replies are already written: a pending
    /// resize lands before the chunk's bytes parse, so the first frame
    /// after a resize draws at the new size; then the chunk is appended
    /// and the grid is snapshotted.
    pub(crate) fn publish(&mut self, bytes: &[u8]) {
        if let Some((cols, rows)) = self.pending_size.take() {
            self.parser.screen_mut().set_size(rows, cols);
        }
        self.parser.process(bytes);
        self.output.extend_from_slice(bytes);
        self.grid = Grid::of(&self.parser);
    }

    /// Every byte published so far.
    pub(crate) fn output(&self) -> Vec<u8> {
        self.output.clone()
    }
}

/// The one master writer: the reader's reply writes and `Run::write`
/// both write through it, so a reply and typed input never interleave.
pub(crate) type Writer = Arc<Mutex<fs::File>>;

/// Writes all of `bytes` through `writer` and flushes, on a thread taking
/// what remains of `deadline.left()`: expiry is a `TimedOut` error, and
/// the thread is never joined. It never panics, so the reader can hold
/// the `Shared` lock across it.
pub(crate) fn write_within(writer: &Writer, bytes: &[u8], deadline: Deadline) -> io::Result<()> {
    let writer = Arc::clone(writer);
    let bytes = bytes.to_vec();
    let (done, finished) = mpsc::channel();
    thread::Builder::new()
        .spawn(move || {
            use std::io::Write;
            let result = (|| {
                let mut guard = writer.lock().unwrap();
                guard.write_all(&bytes)?;
                guard.flush()
            })();
            done.send(result).unwrap_or(());
        })
        .map_err(io::Error::other)?;
    match finished.recv_timeout(deadline.left()) {
        Ok(result) => result,
        Err(_) => Err(io::Error::new(
            ErrorKind::TimedOut,
            "waited until the deadline for the terminal write",
        )),
    }
}

/// A capability query Fiber emits (`crates/tui/src/term.rs`,
/// `crates/tui/src/appearance.rs`) and the harness's reply: kitty's
/// disambiguate flags, a bare device-attributes answer, and a black
/// background with a dark theme report, so the grid is a fixed dark
/// xterm whose Esc key arrives as `CSI 27 u`. Before any report the
/// appearance is dark (`crates/tui/src/appearance.rs`).
const CAPABILITIES: [(&[u8], &[u8]); 4] = [
    (b"\x1b[?u", b"\x1b[?1u"),
    (b"\x1b[c", b"\x1b[?0c"),
    (b"\x1b]11;?\x1b\\", b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
    (b"\x1b[?996n", b"\x1b[?997;1n"),
];

/// How many trailing bytes the query tail keeps for a query split across
/// two reads: longer than the longest query above.
const PENDING_KEEP: usize = 16;

/// The replies for every whole query in `pending`, in stream order,
/// dropping the bytes through each answered query and keeping the tail
/// for a query still arriving.
pub(crate) fn query_replies(pending: &mut Vec<u8>) -> Vec<u8> {
    let mut replies = Vec::new();
    loop {
        let mut first: Option<(usize, usize)> = None;
        for (at, (query, _)) in CAPABILITIES.iter().enumerate() {
            if let Some(pos) = pending.windows(query.len()).position(|w| w == *query)
                && first.is_none_or(|(best, _)| pos < best)
            {
                first = Some((pos, at));
            }
        }
        let Some((pos, at)) = first else { break };
        replies.extend_from_slice(CAPABILITIES[at].1);
        pending.drain(..pos + CAPABILITIES[at].0.len());
    }
    let drop = pending.len().saturating_sub(PENDING_KEEP);
    pending.drain(..drop);
    replies
}

/// The terminal under test: its child, the pty master with its reader
/// draining it from the first frame to end of file, and one wake per
/// chunk appended. A reader drains the master except between `stall`
/// and `wait`. Sessions the hub starts run in their own process groups,
/// guarded by a matching watchdog on the workspace; the hub idles out
/// on its own.
pub(crate) struct Run {
    child: Option<Child>,
    /// The hub's socket, removed when the hub exits.
    hub_socket: PathBuf,
    watchdog: Option<Watchdog>,
    sessions: Option<Watchdog>,
    shared: Arc<Mutex<Shared>>,
    writer: Writer,
    reader: Option<Reader>,
    /// One wake per chunk appended.
    wakes: mpsc::Receiver<()>,
    cols: u16,
    rows: u16,
    deadline: Deadline,
}

/// What a run drained: the child's exit and every byte read from the
/// master since spawn, in order, through end of file.
pub(crate) struct Exited {
    pub(crate) status: ExitStatus,
    pub(crate) terminal: Vec<u8>,
}

impl Run {
    /// Spawns `fiber` with `args` on a `cols` by `rows` pty: standard
    /// input, output and error all on the terminal side, as on a real
    /// terminal. Each run starts from a cleared environment with only
    /// `PATH`, `HOME`, `FIBER_HOME` and `TERM=xterm-256color`, then
    /// applies `env` in order, so the grid is a fixed dark xterm: the
    /// binary never reads the ambient terminal's kind or theme. The
    /// slave is dropped after the spawn, and the master is CLOEXEC, so
    /// no hub `fiber` starts holds it open.
    pub(crate) fn spawn(
        setup: &Setup,
        cols: u16,
        rows: u16,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        let terminal = open(cols, rows);
        let sessions = Watchdog::matching(setup.workspace().to_str().unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .current_dir(setup.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", setup.root.path())
            .env("FIBER_HOME", setup.home())
            .env("TERM", "xterm-256color");
        for (key, value) in env {
            command.env(key, value);
        }
        command.args(args);
        command
            .stdin(terminal.stdio())
            .stdout(terminal.stdio())
            .stderr(terminal.stdio());
        let (child, watchdog) = spawn_watched(&mut command);
        drop(terminal.terminal);
        let deadline = setup.deadline;
        let main = fs::File::from(terminal.main);
        let shared = Arc::new(Mutex::new(Shared::new(cols, rows)));
        let writer: Writer = Arc::new(Mutex::new(main.try_clone().unwrap()));
        let (reader, wakes) =
            Reader::start(main, Arc::clone(&shared), Arc::clone(&writer), deadline);
        Self {
            child: Some(child),
            hub_socket: setup.home().join("run").join("hub"),
            watchdog: Some(watchdog),
            sessions: Some(sessions),
            shared,
            writer,
            reader: Some(reader),
            wakes,
            cols,
            rows,
            deadline,
        }
    }

    /// A run with no child over an already-open `master`, for harness
    /// tests feeding output by hand: the reader drains from the first
    /// chunk to end of file, as in [`Run::spawn`].
    pub(crate) fn attach(master: fs::File, cols: u16, rows: u16, deadline: Deadline) -> Self {
        let shared = Arc::new(Mutex::new(Shared::new(cols, rows)));
        let writer: Writer = Arc::new(Mutex::new(master.try_clone().unwrap()));
        let (reader, wakes) =
            Reader::start(master, Arc::clone(&shared), Arc::clone(&writer), deadline);
        Self {
            child: None,
            hub_socket: PathBuf::new(),
            watchdog: None,
            sessions: None,
            shared,
            writer,
            reader: Some(reader),
            wakes,
            cols,
            rows,
            deadline,
        }
    }

    /// Types bytes into the terminal through the one shared writer, so a
    /// reply and typed input never interleave. It panics naming the
    /// typing on failure, holding no lock.
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        write_within(&self.writer, bytes, self.deadline).expect("typing on the terminal");
    }

    /// The shared writer: harness tests own its lock as the pause point
    /// proving a chunk is published only after its replies are written
    /// (`docs/testing.md`, "Waits and timeouts").
    pub(crate) fn writer(&self) -> Writer {
        Arc::clone(&self.writer)
    }

    /// The output so far.
    pub(crate) fn output(&self) -> Vec<u8> {
        self.shared.lock().unwrap().output()
    }

    /// The grid rebuilt from the output so far.
    pub(crate) fn screen(&self) -> Grid {
        self.shared.lock().unwrap().grid.clone()
    }

    /// Waits under one named deadline for the whole wait until the grid
    /// matches, however many frames arrive, and returns the matching
    /// grid. When the terminal ends first the panic names the wait and
    /// shows the last grid, so a stall says how far the journey got.
    pub(crate) fn wait_screen(&mut self, what: &str, mut done: impl FnMut(&Grid) -> bool) -> Grid {
        loop {
            let (grid, ended) = {
                let shared = self.shared.lock().unwrap();
                (shared.grid.clone(), shared.ended)
            };
            if done(&grid) {
                return grid;
            }
            if ended {
                panic!(
                    "the terminal ended while waiting for {what}; screen:\n{}",
                    grid.contents
                );
            }
            if self.wakes.recv_timeout(self.deadline.left()).is_err() {
                panic!(
                    "waited until the deadline for {what}; screen:\n{}",
                    self.screen().contents
                );
            }
        }
    }

    /// Waits under the run's one deadline until the output at or after
    /// `from` holds `needle` as exact bytes, and returns where the match
    /// ends. The search starts at the caller's offset, never at an
    /// earlier match, so two markers arriving in either order both
    /// finish. Bytes that never reach the cells, such as the window
    /// title, the OSC 9 desktop notification, a bell and the
    /// mode-enable sequences, are waited for here instead of on the
    /// grid.
    pub(crate) fn wait_bytes(&mut self, from: usize, needle: &[u8], what: &str) -> usize {
        assert!(!needle.is_empty(), "a byte wait names its bytes");
        loop {
            let (output, ended) = {
                let shared = self.shared.lock().unwrap();
                (shared.output.clone(), shared.ended)
            };
            if let Some(end) = exact_end(&output, from, needle) {
                return end;
            }
            if ended {
                panic!("the terminal ended while waiting for {what}");
            }
            if self.wakes.recv_timeout(self.deadline.left()).is_err() {
                panic!(
                    "waited until the deadline for {what}; output: {:?}",
                    String::from_utf8_lossy(&self.output())
                );
            }
        }
    }

    /// The end of the first frame: the first OSC 2 title.
    /// `event_loop::run` draws the first frame, writes the title, then
    /// starts the input reader and the resize thread
    /// (`crates/tui/src/event_loop.rs`), so input after it is never too
    /// early for its receiver.
    pub(crate) fn ready(&mut self) -> usize {
        self.wait_bytes(0, TITLE, "the end of the first frame")
    }

    /// The turn that started after offset `from` finished, whichever
    /// order its two markers arrive in: the `completed` paint on the
    /// grid, and the finished title on the raw output at or after
    /// `from`. Each wait searches from its own start, never from the
    /// other's match, so the pair is order-free (see #1775).
    pub(crate) fn turn_finished(&mut self, from: usize) {
        self.wait_screen("the completed turn", |grid| {
            grid.contents.contains("completed")
        });
        self.wait_bytes(from, FINISHED_TITLE, "the finished title");
    }

    /// Resizes the terminal to `cols` by `rows`: the size is recorded
    /// before the signal, so the next chunk the reader takes parses at
    /// the new size. A run with no child signals nothing.
    pub(crate) fn resize(&mut self, cols: u16, rows: u16) {
        self.shared.lock().unwrap().pending_size = Some((cols, rows));
        (self.cols, self.rows) = (cols, rows);
        rustix::termios::tcsetwinsize(
            &*self.writer.lock().unwrap(),
            rustix::termios::Winsize {
                ws_col: cols,
                ws_row: rows,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        if let Some(child) = self.child.as_ref() {
            kill_pid(self.deadline, child.id(), "WINCH").unwrap();
        }
    }

    /// Stops the reader so `fiber`'s later output stays queued in the
    /// terminal until `wait`: every byte `fiber` writes from then on is
    /// undrained. A reader still running after the stop would prove
    /// nothing, so the stop is an assert naming the reader. A second
    /// call finds no reader and does nothing. After it, `wait_screen`
    /// and `wait_bytes` are not called: with no reader draining, their
    /// wakes never arrive and they wait until the deadline.
    pub(crate) fn stall(&mut self) {
        if let Some(reader) = self.reader.take() {
            assert!(reader.stop(), "expected the reader to stop");
        }
    }

    /// Restores the drain when stalled, keeping the shared state (the
    /// parser, the query tail) and the writer. Then waits for the child
    /// to exit and reaps it, checks its group empties, waits for the
    /// reader to end at end of file so every queued byte is read, stands
    /// the watchdog down and waits for the hub to idle out and remove
    /// its socket. Returns the exit and every byte read from the master
    /// since spawn.
    pub(crate) fn wait(mut self) -> Exited {
        if self.reader.is_none() {
            let main = self.writer.lock().unwrap().try_clone().unwrap();
            self.reader = Some(Reader::start_on(
                main,
                Arc::clone(&self.shared),
                Arc::clone(&self.writer),
                self.deadline,
            ));
        }
        let child = self.child.take().unwrap();
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => expired(self.deadline, group, &finished, "`fiber` to exit"),
        };
        assert!(
            fakes::group_empties(group, self.deadline.left()),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        let reader = self.reader.take().unwrap();
        assert!(
            reader.ended(self.deadline.left()),
            "waited until the deadline for the terminal's end of file"
        );
        assert!(
            !self.deadline.left().is_zero(),
            "waited until the deadline for the terminal's end of file"
        );
        reader.stop();
        self.watchdog
            .take()
            .unwrap()
            .stand_down(self.deadline.cleanup());
        until_gone(self.deadline, &self.hub_socket, "the hub to idle out");
        Exited {
            status: output.status,
            terminal: self.shared.lock().unwrap().output(),
        }
    }
}

impl Drop for Run {
    /// Stops the reader without waiting past the deadline, then kills the
    /// child's group and reaps the child on a detached thread, so a
    /// panic mid-test leaves nothing behind. A run with no child reaps
    /// nothing; the watchdogs kill the hub and its sessions as ever.
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            reader.stop();
        }
        if let Some(mut child) = self.child.take() {
            kill_group_detached(child.id(), "KILL");
            let reaped = thread::Builder::new().spawn(move || match child.wait() {
                Ok(_) | Err(_) => {}
            });
            match reaped {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

/// Waits under `deadline` for `socket` to go, naming `what` on expiry. The
/// spin stops when the deadline's remainder is zero.
fn until_gone(deadline: Deadline, socket: &Path, what: &str) {
    while socket.exists() {
        if deadline.left().is_zero() {
            panic!("waited until the deadline for {what}");
        }
        thread::yield_now();
    }
}

/// Whether `haystack` holds `needle` as bytes.
pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

/// Where `needle` ends in `haystack` at or after `from`, as exact bytes.
pub(crate) fn exact_end(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(haystack.len()));
    }
    if from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at + needle.len())
}

/// Every SGR parameter list in `bytes`, in order: `\x1b[m` is `[0]`.
/// Anything but a colour sequence is skipped.
pub(crate) fn sgr_params(bytes: &[u8]) -> Vec<Vec<u16>> {
    let mut lists = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while bytes
                .get(j)
                .is_some_and(|byte| (0x20..=0x3F).contains(byte))
            {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'm' {
                lists.push(if j == i + 2 {
                    vec![0]
                } else {
                    bytes[i + 2..j]
                        .split(|byte| *byte == b';')
                        .map(|digits| {
                            std::str::from_utf8(digits)
                                .unwrap_or("")
                                .parse()
                                .unwrap_or(0)
                        })
                        .collect()
                });
                i = j + 1;
            } else if j < bytes.len() {
                i = j + 1;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }
    lists
}
