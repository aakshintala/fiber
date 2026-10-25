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
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::Watchdog;
use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::pty;

use super::{Deadline, KillGroup, Setup, expired, spawn_watched};

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
    #[track_caller]
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
    #[track_caller]
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
    #[track_caller]
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
        Deadline::after(within).recv(&self.done).is_ok()
    }

    /// Wakes the reader and waits for its thread, within
    /// `deadline.cleanup()`: true when the thread finished. It never
    /// blocks past the deadline, so a panic mid-test stops the reader as
    /// well as killing the group.
    pub(crate) fn stop(mut self) -> bool {
        use std::io::Write;
        self.stop.write_all(b"x").unwrap_or(());
        self.deadline.cleanup_phase().recv(&self.done).is_ok()
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
#[track_caller]
pub(crate) fn open(cols: u16, rows: u16) -> Terminal {
    open_with_pixels(cols, rows, 0, 0)
}

/// Opens a `cols` by `rows` pty reporting `xpixel` by `ypixel` pixels:
/// the cell size the terminal under test divides its grid by
/// (`docs/tui.md`, "Images").
#[track_caller]
pub(crate) fn open_with_pixels(cols: u16, rows: u16, xpixel: u16, ypixel: u16) -> Terminal {
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
            ws_xpixel: xpixel,
            ws_ypixel: ypixel,
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
    /// Whether the cell draws reversed: the focus look.
    pub(crate) inverse: bool,
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
            inverse: cell.inverse(),
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

    /// The screen's size as (columns, rows): the bounds for scanning
    /// cells, since row text trims trailing blanks.
    pub(crate) fn size(&self) -> (u16, u16) {
        let (rows, cols) = self.screen.size();
        (cols, rows)
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
    graphics: bool,
    /// Whether a synchronized-output block is open: waits snapshot whole frames only.
    sync_open: bool,
    /// The last bytes seen, completing a marker split across two reads.
    sync_tail: Vec<u8>,
    pending_size: Option<(u16, u16)>,
    ended: bool,
    /// The output length at the last grid snapshot: the bytes a
    /// replay needs to reach the snapshotted grid.
    snapshot_len: usize,
    /// The output length at the last grid wait the test completed:
    /// the dump holds the output through it, never a snapshot taken
    /// after quit.
    dump_len: Option<usize>,
}

impl Shared {
    /// A blank `cols` by `rows` screen with no output.
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        Self::new_with_graphics(cols, rows, None)
    }

    fn new_with_graphics(cols: u16, rows: u16, pixel_size: Option<(u16, u16)>) -> Self {
        let graphics = pixel_size.is_some();
        let parser = vt100::Parser::new(rows, cols, 0);
        let grid = Grid::of(&parser);
        Self {
            output: Vec::new(),
            parser,
            grid,
            queries: Vec::new(),
            graphics,
            sync_open: false,
            sync_tail: Vec::new(),
            pending_size: None,
            ended: false,
            snapshot_len: 0,
            dump_len: None,
        }
    }

    /// The chunk's query replies, in stream order. Only the query tail
    /// is updated: the chunk itself is published after its replies are
    /// written.
    pub(crate) fn replies(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.queries.extend_from_slice(bytes);
        query_replies_with_graphics(&mut self.queries, self.graphics)
    }

    /// Publishes the chunk whose replies are already written: a pending
    /// resize lands before the chunk's bytes parse, so the first frame
    /// after a resize draws at the new size; then the chunk is appended,
    /// parsed, and snapshotted unless a synchronized-output block is open.
    /// When `LOOK_DIFF_GRID` names a path, each snapshot also overwrites
    /// that path with the output through the last grid wait the test
    /// completed (the whole output when no grid wait completed yet), so
    /// the dump holds the output through the last grid wait the test
    /// completes, never a snapshot taken after quit. Unset, no file is
    /// written. The write failure is ignored: a bad path surfaces as a
    /// missing dump where the dump is read, never as a look-test failure.
    pub(crate) fn publish(&mut self, bytes: &[u8]) {
        if let Some((cols, rows)) = self.pending_size.take() {
            self.parser.screen_mut().set_size(rows, cols);
        }
        self.parser.process(bytes);
        self.output.extend_from_slice(bytes);
        self.sync_through(bytes);
        if !self.sync_open {
            self.grid = Grid::of(&self.parser);
            self.snapshot_len = self.output.len();
            if let Some(path) = std::env::var_os("LOOK_DIFF_GRID") {
                let end = self
                    .dump_len
                    .unwrap_or(self.snapshot_len)
                    .min(self.output.len());
                drop(std::fs::write(path, &self.output[..end]));
            }
        }
    }

    /// Folds `chunk`'s markers into the block state in stream order, then
    /// keeps the tail for a marker split across two reads.
    fn sync_through(&mut self, chunk: &[u8]) {
        let mut both = std::mem::take(&mut self.sync_tail);
        let split = both.len();
        both.extend_from_slice(chunk);
        let mut at = 0;
        while at + SYNC_BEGIN.len() <= both.len() {
            if both[at..].starts_with(SYNC_BEGIN) && at + SYNC_BEGIN.len() > split {
                self.sync_open = true;
                at += SYNC_BEGIN.len();
            } else if both[at..].starts_with(SYNC_END) && at + SYNC_END.len() > split {
                self.sync_open = false;
                at += SYNC_END.len();
            } else {
                at += 1;
            }
        }
        self.sync_tail = both[both.len().saturating_sub(SYNC_KEEP)..].to_vec();
    }

    /// The grid rebuilt from the output so far: whole frames only.
    pub(crate) fn grid(&self) -> Grid {
        self.grid.clone()
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
#[track_caller]
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
    match deadline.recv(&finished) {
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

/// Kitty's graphics probe and an OK reply for pixel-reporting terminals.
const GRAPHICS_CAPABILITY: (&[u8], &[u8]) = (
    b"\x1b_Gi=31,a=q,s=1,v=1,f=24,m=0;AAAA\x1b\\",
    b"\x1b_Gi=31;OK\x1b\\",
);

/// How many trailing bytes the query tail keeps for a query split across
/// two reads: enough for the longest query, including the optional graphics probe.
const PENDING_KEEP: usize = 64;

/// Synchronized output's markers (DEC mode 2026): the terminal brackets
/// every frame in them (`docs/tui.md`, "Performance").
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";
const SYNC_END: &[u8] = b"\x1b[?2026l";
/// The sync tail's kept bytes: one fewer than the markers' length, so a
/// marker split across two reads completes.
const SYNC_KEEP: usize = 7;

/// The replies for every whole query in `pending`, in stream order,
/// dropping the bytes through each answered query and keeping the tail
/// for a query still arriving.
pub(crate) fn query_replies(pending: &mut Vec<u8>) -> Vec<u8> {
    query_replies_with_graphics(pending, false)
}

/// Answers the ordinary capability queries and, when `graphics` is true,
/// the kitty graphics probe used by a pixel-reporting pty.
pub(crate) fn query_replies_with_graphics(pending: &mut Vec<u8>, graphics: bool) -> Vec<u8> {
    let mut replies = Vec::new();
    loop {
        let mut first: Option<(usize, usize, &[u8])> = None;
        for (query, reply) in CAPABILITIES
            .iter()
            .chain(graphics.then_some(&GRAPHICS_CAPABILITY))
        {
            if let Some(pos) = pending.windows(query.len()).position(|w| w == *query)
                && first.is_none_or(|(best, _, _)| pos < best)
            {
                first = Some((pos, query.len(), reply));
            }
        }
        let Some((pos, query_len, reply)) = first else {
            break;
        };
        replies.extend_from_slice(reply);
        pending.drain(..pos + query_len);
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
    /// The hub's home: its `hub serve` carries `FIBER_HOME=<home>` in
    /// its environment, which the exit wait lists for. Empty for an
    /// attached run, which waits on no hub.
    hub_home: PathBuf,
    watchdog: Option<Watchdog>,
    /// The sessions watchdog and the workspace path it matches.
    sessions: Option<(Watchdog, String)>,
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
    /// Spawns without seeding import receipts: the first-run offer
    /// test's fresh home.
    #[track_caller]
    pub(crate) fn spawn_unseeded(
        setup: &Setup,
        cols: u16,
        rows: u16,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        Self::spawn_inner(setup, cols, rows, None, args, env, false)
    }

    /// Spawns with seeding import receipts: every other test's
    /// home, where the offer stays down.
    #[track_caller]
    pub(crate) fn spawn(
        setup: &Setup,
        cols: u16,
        rows: u16,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        Self::spawn_inner(setup, cols, rows, None, args, env, true)
    }

    /// Spawns with seeding import receipts on a terminal reporting
    /// `xpixel` by `ypixel` pixels, for cell-size math under test
    /// (`docs/tui.md`, "Images"). It also answers the kitty graphics
    /// probe.
    #[track_caller]
    pub(crate) fn spawn_pixels(
        setup: &Setup,
        cols: u16,
        rows: u16,
        xpixel: u16,
        ypixel: u16,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Self {
        Self::spawn_inner(setup, cols, rows, Some((xpixel, ypixel)), args, env, true)
    }

    /// Spawns `fiber` with `args` on a `cols` by `rows` pty: standard
    /// input, output and error all on the terminal side, as on a real
    /// terminal. Each run starts from a cleared environment with only
    /// `PATH`, `HOME`, `FIBER_HOME` and `TERM=xterm-256color`, then
    /// applies `env` in order, so the grid is a fixed dark xterm: the
    /// binary never reads the ambient terminal's kind or theme. The
    /// slave is dropped after the spawn, and the master is CLOEXEC, so
    /// no hub `fiber` starts holds it open. A pixel size enables the
    /// graphics probe; `None` keeps the ordinary pty capability set.
    #[track_caller]
    fn spawn_inner(
        setup: &Setup,
        cols: u16,
        rows: u16,
        pixel_size: Option<(u16, u16)>,
        args: &[&str],
        env: &[(&str, &str)],
        seed_receipts: bool,
    ) -> Self {
        // A seeded pty run starts with the import already done: the
        // first-run offer would cover the pinned frames these tests
        // assert. The files are empty, so a run that really imports
        // still reads its sources: only a parsed receipt short-
        // circuits. The offer itself has its own binary test.
        if seed_receipts {
            for name in ["import-claude-code.json", "import-codex.json"] {
                let receipt = setup.home().join(name);
                if !receipt.is_file() {
                    fs::write(&receipt, "{}\n").unwrap();
                }
            }
        }
        let pixels = pixel_size.unwrap_or((0, 0));
        let terminal = open_with_pixels(cols, rows, pixels.0, pixels.1);
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
        let shared = Arc::new(Mutex::new(Shared::new_with_graphics(
            cols, rows, pixel_size,
        )));
        let writer: Writer = Arc::new(Mutex::new(main.try_clone().unwrap()));
        let (reader, wakes) =
            Reader::start(main, Arc::clone(&shared), Arc::clone(&writer), deadline);
        Self {
            child: Some(child),
            hub_socket: setup.home().join("run").join("hub"),
            hub_home: setup.home().to_path_buf(),
            watchdog: Some(watchdog),
            sessions: Some((sessions, setup.workspace().to_string_lossy().into_owned())),
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
            hub_home: PathBuf::new(),
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
    #[track_caller]
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        write_within(&self.writer, bytes, self.deadline).expect("typing on the terminal");
    }

    /// The shared writer: harness tests own its lock as the pause point
    /// proving a chunk is published only after its replies are written
    /// (`docs/testing.md`, "Waits and timeouts").
    pub(crate) fn writer(&self) -> Writer {
        Arc::clone(&self.writer)
    }

    /// The terminal child's pid: the tests signal the terminal through
    /// it. A run with no child has none.
    pub(crate) fn id(&self) -> u32 {
        self.child.as_ref().expect("the run has a child").id()
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
    /// grid. A completed wait records the output through its grid, and
    /// when `LOOK_DIFF_GRID` names a path rewrites the dump with the
    /// output through the last grid wait the test completed, so the dump
    /// holds the output through the last grid wait the test completes,
    /// never a snapshot taken after quit. When the terminal ends first
    /// the panic names the wait and shows the last grid, so a stall says
    /// how far the journey got. A deadline expiry panics with the grid
    /// the last check saw, never a fresh read, and names how much of the
    /// deadline remained when the wait began.
    #[track_caller]
    pub(crate) fn wait_screen(&mut self, what: &str, done: impl FnMut(&Grid) -> bool) -> Grid {
        self.wait_screen_leading(what, done, String::new)
    }

    /// [`Run::wait_screen`] whose failure panic starts with `lead()`, so a
    /// caller's report comes before the wait's name and the screen dump
    /// and survives a length limit on the captured output. It is the one
    /// panic, so no earlier panic hook output can push the report out.
    /// `lead` runs only on failure and supplies its own separator; the
    /// predicate, the deadline and the fast-fail path are `wait_screen`'s.
    #[track_caller]
    pub(crate) fn wait_screen_leading(
        &mut self,
        what: &str,
        mut done: impl FnMut(&Grid) -> bool,
        lead: impl FnOnce() -> String,
    ) -> Grid {
        let start_left = self.deadline.left();
        loop {
            let (grid, ended, snapshot_len) = {
                let shared = self.shared.lock().unwrap();
                (shared.grid.clone(), shared.ended, shared.snapshot_len)
            };
            if done(&grid) {
                let dump = {
                    let mut shared = self.shared.lock().unwrap();
                    shared.dump_len = Some(snapshot_len.min(shared.output.len()));
                    let end = shared.dump_len.unwrap_or(0).min(shared.output.len());
                    std::env::var_os("LOOK_DIFF_GRID")
                        .map(|path| (path, shared.output[..end].to_vec()))
                };
                if let Some((path, bytes)) = dump {
                    drop(std::fs::write(path, &bytes));
                }
                return grid;
            }
            if ended {
                panic!(
                    "{}the terminal ended while waiting for {what}; screen:\n{}",
                    lead(),
                    grid.contents
                );
            }
            if self.deadline.recv(&self.wakes).is_err() {
                panic!(
                    "{}waited until the deadline for {what} (deadline had {start_left:?} left when the wait began); screen:\n{}",
                    lead(),
                    grid.contents
                );
            }
        }
    }

    /// Waits under one named deadline like [`Run::wait_screen`], and on
    /// expiry re-panics with the stage name plus the current grid and
    /// the caller's extra context: the caught panic already names the
    /// wait and shows the grid its last check saw, so the report rides
    /// on the timeout panic instead of replacing it. `extra` runs only
    /// on failure, and takes no `&mut Run`, so the caller passes reads
    /// that borrow nothing the wait holds (a session-log excerpt over
    /// `Setup`, never the run's own grid). It never loosens the wait:
    /// the predicate, the deadline and the fast-fail path are
    /// `wait_screen`'s own.
    #[track_caller]
    pub(crate) fn wait_screen_reporting(
        &mut self,
        what: &str,
        mut done: impl FnMut(&Grid) -> bool,
        extra: impl FnOnce() -> String,
    ) -> Grid {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.wait_screen(what, &mut done)
        }));
        match result {
            Ok(grid) => grid,
            Err(payload) => {
                let base = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| {
                        payload
                            .downcast_ref::<&str>()
                            .map(|held| (*held).to_owned())
                    })
                    .unwrap_or_else(|| what.to_owned());
                let grid = self.screen().rows.join("\n");
                panic!("{base}; grid:\n{grid}; {}", extra());
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
    #[track_caller]
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
            if self.deadline.recv(&self.wakes).is_err() {
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
    #[track_caller]
    pub(crate) fn ready(&mut self) -> usize {
        self.wait_bytes(0, TITLE, "the end of the first frame")
    }

    /// The turn that started after offset `from` finished: the finished
    /// title on the raw output at or after `from` first, then the
    /// `completed` paint on the grid. Either marker may arrive first:
    /// the grid and the output both accumulate, so the pair stays
    /// order-free (see #1775). The title wait is the per-turn
    /// gate: it searches from `from`, so `from` must be taken after the
    /// previous turn's title reached this terminal, or an earlier
    /// turn's title matches first (see #2211). The paint wait only
    /// confirms the close drew; the grid keeps an earlier turn's
    /// paint, so it never proves which turn finished. A caller
    /// proving one turn's durable events reads the session log under
    /// its deadline after this returns.
    #[track_caller]
    pub(crate) fn turn_finished(&mut self, from: usize) {
        self.wait_bytes(from, FINISHED_TITLE, "the finished title");
        self.wait_screen("the completed turn", |grid| {
            grid.contents.contains("completed")
        });
    }

    /// Resizes the terminal to `cols` by `rows`: the size is recorded
    /// before the signal, so the next chunk the reader takes parses at
    /// the new size. A run with no child signals nothing.
    #[track_caller]
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
            fakes::kill_pid(child.id(), "WINCH").unwrap();
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
    /// the watchdog down and waits for the hub process to exit. Returns
    /// the exit and every byte read from the master since spawn.
    #[track_caller]
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
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => expired(self.deadline, group, &finished, "`fiber` to exit"),
        };
        assert!(
            fakes::group_empties(group, self.deadline),
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
            .stand_down(self.deadline.cleanup_phase());
        until_hub_exits(
            self.deadline,
            &self.hub_socket,
            &self.hub_home,
            "the hub process to exit",
        );
        // The hub exiting leaves its sessions running. Kill them, as the
        // sessions watchdog would, and stand it down only once no process
        // holds the workspace path. On a miss it stays armed.
        if let Some((sessions, workspace)) = self.sessions.take() {
            if let Err(err) = fakes::kill_matching(&workspace) {
                panic!("killing the sessions in the workspace: {err}");
            }
            if let Err(err) = fakes::try_matching_exits(&workspace, self.deadline) {
                panic!(
                    "waited until the deadline for every process holding the workspace path to exit: {err}"
                );
            }
            sessions.stand_down(self.deadline.cleanup_phase());
        }
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
            drop(fakes::kill_group(child.id(), "KILL"));
            let reaped = thread::Builder::new().spawn(move || match child.wait() {
                Ok(_) | Err(_) => {}
            });
            match reaped {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

/// Waits under `deadline` for the hub's socket to go and then for its
/// `hub serve` process to exit, naming `what` on expiry: the socket's
/// absence alone cannot prove the hub is gone, since the hub removes the
/// socket before it drops its hub and returns. An empty `hub_home` is an
/// attached run, which waits on no hub. The socket poll, the single
/// listing and the pid wait share the one deadline, so the wait ends at
/// the deadline however each phase behaves. The wait lives in
/// [`until_hub_exits_with`], so tests pin it with a fake lister.
#[track_caller]
pub(crate) fn until_hub_exits(deadline: Deadline, socket: &Path, hub_home: &Path, what: &str) {
    let home = hub_home.to_string_lossy();
    until_hub_exits_with(deadline, socket, &home, what, &mut || {
        list_processes(deadline.left())
    });
}

/// [`until_hub_exits`] with the process listing injected: waits for the
/// socket to go with the shared paced wait, then lists once to find this
/// run's hub and waits on its pid under the same deadline, panicking
/// naming `what` on expiry. An empty `home` is an attached run, which
/// lists nothing and waits on the socket alone. A socket-only wait would
/// return while a listed hub lives; the one-listing test in `look.rs`
/// pins that the lister runs once, after the socket is gone.
pub(crate) fn until_hub_exits_with(
    deadline: Deadline,
    socket: &Path,
    home: &str,
    what: &str,
    list: &mut dyn FnMut() -> String,
) {
    let socket = socket.to_path_buf();
    fakes::wait_until(what, deadline, move || !socket.exists());
    if home.is_empty() {
        return;
    }
    let pids = hub_pids(&list(), home);
    if pids.is_empty() {
        return;
    }
    assert!(
        fakes::pids_exit(&pids, deadline),
        "waited until the deadline for {what}"
    );
}

/// Whether the hub is gone: its socket is absent and no listed process
/// is this run's hub. An empty `home` is an attached run, which lists
/// nothing and waits on the socket alone. Pure apart from the socket
/// probe, so the pause-point repro in `look.rs` pins it without threads:
/// the socket gone while the hub is alive is not gone.
pub(crate) fn hub_gone(socket: &Path, home: &str, list: &mut dyn FnMut() -> String) -> bool {
    if socket.exists() {
        return false;
    }
    if home.is_empty() {
        return true;
    }
    hub_pids(&list(), home).is_empty()
}

/// The pids in the `ps` table `table` that are this run's hub: every row
/// whose command holds `hub serve` and whose whitespace-split words hold
/// the exact token `FIBER_HOME=<home>`. Pure, so the boundary table in
/// `look.rs` pins it: a longer value is a different home, and a row
/// without the token, such as the lister's own, never matches.
pub(crate) fn hub_pids(table: &str, home: &str) -> Vec<u32> {
    let token = format!("FIBER_HOME={home}");
    table
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let pid: u32 = words.next()?.parse().ok()?;
            if !line.contains("hub serve") {
                return None;
            }
            if !words.any(|word| word == token.as_str()) {
                return None;
            }
            Some(pid)
        })
        .collect()
}

/// Every process's pid with its command line and environment appended:
/// the table `hub_pids` parses. `ps` appends the environment with `-E`
/// on macOS and `e` on Linux. It runs with a cleared environment holding
/// only `PATH`, never the check-run nonce and never `FIBER_HOME`, so its
/// own row holds no token and it can never match itself.
#[track_caller]
pub(crate) fn list_processes(within: Duration) -> String {
    let user = std::env::var("USER").expect("USER names the test's user");
    let mut command = Command::new("ps");
    if cfg!(target_os = "macos") {
        command.args(["-E", "-ww", "-o", "pid=,command=", "-U", &user]);
    } else {
        command.args(["e", "-ww", "-o", "pid=,command=", "-U", &user]);
    }
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let output = fakes::within(
        "ps to list every process",
        Deadline::after(within),
        move || command.output(),
    )
    .expect("ps to list every process");
    assert!(
        output.status.success(),
        "ps to list every process failed: {}",
        output.status
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// `wait_screen` panics with the grid its last check saw, never a
    /// fresh read, and names how much of the deadline remained when the
    /// wait began. The marker reaches the run's own terminal but its grid
    /// stays stale behind a synchronized-output block, so the checked
    /// grid holds no marker while the output does; the predicate then
    /// publishes the held bytes to the grid synchronously after its check,
    /// with no wake, so the timeout panic must show the checked grid while
    /// a fresh read sees the marker. Replacing `grid.contents` with
    /// `self.screen().contents` in the panic makes the shown grid contain
    /// the marker and fails the equality below. The test uses no sleep
    /// (`docs/testing.md`, "Waits and timeouts").
    #[test]
    fn wait_screen_panics_with_the_checked_grid_and_names_the_deadline_left() {
        let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
        let deadline = Deadline::on(&**clock);
        let terminal = open(80, 24);
        let main = fs::File::from(terminal.main);
        let mut terminal_side = terminal.terminal;
        let mut run = Run::attach(main, 80, 24, deadline);
        // The marker goes through the run's own terminal: the reader
        // publishes it to the output but holds the grid behind the sync
        // block, so the grid stays stale while the output advances.
        let from = run.output().len();
        terminal_side.write_all(SYNC_BEGIN).unwrap();
        terminal_side.write_all(b"marker").unwrap();
        run.wait_bytes(from, b"marker", "the fed marker");
        assert!(
            !run.screen().contents.contains("marker"),
            "the grid stays stale behind the sync block"
        );
        // Stop and join the reader before draining wakes: the publish
        // releases the shared lock before its wake is sent, so a
        // nonblocking drain can run before the wake lands and the expired
        // wait below would consume it and check twice (`docs/testing.md`,
        // "Races are forced, not waited for"). After the join no more
        // wakes arrive, so the drain leaves none pending and the wait
        // times out on its first receive.
        let reader = run.reader.take().expect("the run has a reader");
        assert!(reader.stop(), "expected the reader to stop");
        while run.wakes.try_recv().is_ok() {}
        // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
        clock.advance(fakes::deadline::WAITS);
        assert!(
            deadline.left().is_zero(),
            "the wait starts with no time left"
        );
        let before = run.screen().contents.clone();
        assert!(
            !before.contains("marker"),
            "the checked grid holds no marker"
        );
        let shared = std::sync::Arc::clone(&run.shared);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run.wait_screen("the never-arriving needle", |grid| {
                assert!(
                    !grid.contents.contains("marker"),
                    "the checked grid is still stale"
                );
                // Publish the held marker to the grid after the check,
                // synchronously and with no wake: the panic below must
                // still show the checked grid.
                {
                    let mut guard = shared.lock().unwrap();
                    guard.sync_open = false;
                    guard.publish(b"");
                    assert!(
                        guard.grid.contents.contains("marker"),
                        "the fresh grid sees the marker after the check"
                    );
                }
                grid.contents.contains("needle-never-arrives")
            })
        }));
        let message = result
            .unwrap_err()
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        let shown = message.split("screen:\n").nth(1).unwrap_or("");
        assert_eq!(
            shown, before,
            "the panic shows the checked grid: {message:?}"
        );
        assert!(
            !shown.contains("marker"),
            "the held output stays out of the panic: {message:?}"
        );
        assert!(
            message.contains("left when the wait began"),
            "the panic names the remaining deadline: {message:?}"
        );
        assert!(
            message.contains("the never-arriving needle"),
            "the panic names the wait: {message:?}"
        );
        let fresh = run.screen().contents.clone();
        assert!(
            fresh.contains("marker"),
            "a fresh read after the check sees the marker"
        );
    }

    /// A led wait puts the caller's lead first and leaves the wait's name,
    /// the remaining deadline and the screen dump after it; the same wait
    /// with no lead still opens with the wait's own text. Both waits
    /// start on a fake clock advanced past the bound, so they expire at
    /// once (`docs/testing.md`, "Waits and timeouts").
    #[test]
    fn wait_screen_leading_puts_the_lead_before_the_wait_and_the_screen() {
        let clock = Box::leak(Box::new(fakes::clock::FakeClock::new()));
        let deadline = Deadline::on(&**clock);
        let terminal = open(80, 24);
        let main = fs::File::from(terminal.main);
        let mut run = Run::attach(main, 80, 24, deadline);
        // No code under test waits on the clock here; the clock only ages the Deadline (ruling 6179406086).
        clock.advance(fakes::deadline::WAITS);
        let panic_text = |result: std::thread::Result<Grid>| {
            result
                .unwrap_err()
                .downcast_ref::<String>()
                .cloned()
                .unwrap_or_default()
        };
        let led = panic_text(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || run.wait_screen_leading("the needle", |_| false, || "the lead; ".to_owned()),
        )));
        assert!(
            led.starts_with("the lead; waited until the deadline for the needle (deadline had "),
            "the lead comes first: {led:?}"
        );
        assert!(
            led.contains("left when the wait began); screen:\n"),
            "the deadline and the screen dump follow: {led:?}"
        );
        let plain = panic_text(std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || run.wait_screen("the needle", |_| false),
        )));
        assert!(
            plain.starts_with("waited until the deadline for the needle (deadline had "),
            "a wait with no lead opens with its own text: {plain:?}"
        );
    }

    /// `shows_row` matches a whole trimmed row only: padding around the
    /// reply text still matches, while a row with any additional text,
    /// such as the working status line, does not. A substring check
    /// would match the bare stem and the status line alike, so each
    /// negative case below fails it.
    #[test]
    fn shows_row_matches_a_whole_trimmed_row_only() {
        use super::super::journey::shows_row;
        let mut parser = vt100::Parser::new(24, 80, 0);
        parser.process("   Working   \r\nWorking 3s · Esc to interrupt\r\n".as_bytes());
        let grid = Grid::of(&parser);
        assert!(shows_row(&grid, "Working"), "the padded reply row matches");
        assert!(
            !shows_row(&grid, "Work"),
            "a stem of the reply row does not match"
        );
        assert!(
            !shows_row(&grid, "Working!"),
            "a longer stem of the reply row does not match"
        );
        assert!(
            !shows_row(&grid, "Working 3s"),
            "a stem of the status line does not match"
        );
        let mut bare = vt100::Parser::new(24, 80, 0);
        bare.process("Working 3s · Esc to interrupt\r\n".as_bytes());
        let bare = Grid::of(&bare);
        assert!(
            !shows_row(&bare, "Working"),
            "the working status line alone does not satisfy the reply wait"
        );
        assert!(!shows_row(&grid, "idle"), "absent text does not match");
    }
}
