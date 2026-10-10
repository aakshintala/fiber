//! The shared pseudo-terminal harness for the binary-level look tests
//! (`docs/tui.md`, "Look"; `docs/testing.md`, "Screens"): the real
//! binary on a sized pty, a reader draining the master from the first
//! frame, one-deadline waits over a channel, and a screen model reading
//! the SGR stream.
//!
//! debt: a single pty driver; move terminal.rs onto this module once see
//! #1608, see #1557 and see #1537 merge.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::ffi::OsStr;
use std::fs;
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

use super::{Deadline, Setup, bounded, expired, kill_group_detached};

/// The reader draining a pty master: it appends every chunk to the shared
/// output and wakes the test once per chunk, from the spawn until it is
/// stopped or the master ends, so the terminal's output queue never fills
/// while the test runs (`docs/testing.md`, "Screens").
pub(crate) struct Reader {
    stop: std::io::PipeWriter,
    done: mpsc::Receiver<()>,
    deadline: Deadline,
}

impl Reader {
    /// Drains `master` on a thread, appending every chunk and waking the
    /// test once per chunk. The thread ends on end of file or a read
    /// error, on the stop pipe becoming readable, or when the poll times
    /// out at the deadline: it never ticks, and every thread this harness
    /// spawns ends by the test's deadline.
    pub(crate) fn start(
        master: fs::File,
        deadline: Deadline,
    ) -> (Self, Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>) {
        let (stop_read, stop_write) = std::io::pipe().unwrap();
        let (wake, wakes) = mpsc::channel();
        let (done, finished) = mpsc::channel();
        let output = Arc::new(Mutex::new(Vec::new()));
        let appended = Arc::clone(&output);
        let master = OwnedFd::from(master);
        thread::Builder::new()
            .spawn(move || Self::drain(master, stop_read, appended, wake, done, deadline))
            .unwrap();
        (
            Self {
                stop: stop_write,
                done: finished,
                deadline,
            },
            output,
            wakes,
        )
    }

    /// Drains `master` on a thread, appending to the shared `output`
    /// instead of a new buffer, and dropping its wakes: nothing reads
    /// them after `wait` restores the drain. The thread ends as in
    /// [`Reader::start`].
    pub(crate) fn start_on(
        master: fs::File,
        output: Arc<Mutex<Vec<u8>>>,
        deadline: Deadline,
    ) -> Self {
        let (stop_read, stop_write) = std::io::pipe().unwrap();
        let (wake, wakes) = mpsc::channel();
        drop(wakes);
        let (done, finished) = mpsc::channel();
        let appended = Arc::clone(&output);
        let master = OwnedFd::from(master);
        thread::Builder::new()
            .spawn(move || Self::drain(master, stop_read, appended, wake, done, deadline))
            .unwrap();
        Self {
            stop: stop_write,
            done: finished,
            deadline,
        }
    }

    /// Polls the master and the stop pipe with the deadline's remaining
    /// time as the timeout, appending master chunks, until end of file, a
    /// read error, the stop pipe, or the deadline.
    fn drain(
        master: OwnedFd,
        stop: std::io::PipeReader,
        output: Arc<Mutex<Vec<u8>>>,
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
                            Ok(0) => break,
                            Ok(n) => {
                                output.lock().unwrap().extend_from_slice(&chunk[..n]);
                                wake.send(()).unwrap_or(());
                            }
                            Err(err) if err == rustix::io::Errno::INTR => {}
                            Err(_) => break,
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
    fn stdio(&self) -> Stdio {
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
    sessions: Watchdog,
    main: fs::File,
    reader: Option<Reader>,
    /// One wake per chunk appended.
    wakes: mpsc::Receiver<()>,
    output: Arc<Mutex<Vec<u8>>>,
    /// Where the last `read_until` match ended.
    seen: usize,
    deadline: Deadline,
}

/// What a run drained: the child's exit and every byte read from the
/// master since spawn, in order, through end of file.
pub(crate) struct Exited {
    pub(crate) status: ExitStatus,
    pub(crate) terminal: Vec<u8>,
}

impl Run {
    /// Spawns `fiber` with no arguments on a `cols` by `rows` pty:
    /// standard input, output and error all on the terminal side, as on a
    /// real terminal. Each run starts from a cleared environment with only
    /// `PATH`, `HOME`, `FIBER_HOME` and its own variables.
    pub(crate) fn spawn(setup: &Setup, cols: u16, rows: u16, env: &[(&str, &str)]) -> Self {
        let terminal = open(cols, rows);
        let sessions = Watchdog::matching(setup.workspace().to_str().unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .current_dir(setup.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", setup.root.path())
            .env("FIBER_HOME", setup.home());
        for (key, value) in env {
            command.env(key, value);
        }
        command
            .stdin(terminal.stdio())
            .stdout(terminal.stdio())
            .stderr(terminal.stdio());
        let (child, watchdog) = spawn_watched(&mut command);
        let deadline = setup.deadline;
        let main = fs::File::from(terminal.main);
        let (reader, output, wakes) = Reader::start(main.try_clone().unwrap(), deadline);
        Self {
            child: Some(child),
            hub_socket: setup.home().join("run").join("hub"),
            watchdog: Some(watchdog),
            sessions,
            main,
            reader: Some(reader),
            wakes,
            output,
            seen: 0,
            deadline,
        }
    }

    /// Types bytes into the terminal, on a thread bounded by the test's
    /// [`Deadline`].
    pub(crate) fn write(&mut self, bytes: &[u8]) {
        let mut main = self.main.try_clone().unwrap();
        let bytes = bytes.to_vec();
        bounded(self.deadline, "typing on the terminal", move || {
            use std::io::Write;
            main.write_all(&bytes)?;
            main.flush()
        })
        .unwrap();
    }

    /// The output so far.
    pub(crate) fn output(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    /// Reads until the output after the last match holds `needle`, under
    /// the test's one deadline for the whole wait, however much other
    /// output arrives. On expiry the panic names what it waited for and
    /// shows the output, so a stall says how far the journey got. A needle
    /// with a space matches its words in order with only a terminal
    /// frame's gap between them: spaces, cursor moves (`\x1b[<r>;<c>H`)
    /// and SGR (`\x1b[...m`); unchanged cells are never rewritten, so a
    /// space can arrive as a cursor move rather than a byte, and a style
    /// change can split words with SGR. The wait runs on the test thread:
    /// each wake takes only what remains of the deadline.
    pub(crate) fn read_until(&mut self, needle: &str) {
        loop {
            if let Some(end) = phrase_end(&self.output(), self.seen, needle) {
                self.seen = end;
                return;
            }
            let left = self.deadline.left();
            if left.is_zero() || self.wakes.recv_timeout(left).is_err() {
                panic!(
                    "waited until the deadline for {needle:?}; output: {:?}",
                    String::from_utf8_lossy(&self.output())
                );
            }
        }
    }

    /// Feeds the whole output so far into a fresh `Screen` on each wake
    /// until `done` holds, under the run's one deadline, panicking with
    /// `what` and the output on expiry. On success sets `seen` to the
    /// output's length, so a later `read_until` matches only newer output.
    pub(crate) fn screen_until(
        &mut self,
        cols: u16,
        rows: u16,
        what: &str,
        done: impl Fn(&Screen) -> bool,
    ) -> Screen {
        loop {
            let output = self.output();
            let mut screen = Screen::new(cols, rows);
            screen.feed(&output);
            if done(&screen) {
                self.seen = output.len();
                return screen;
            }
            let left = self.deadline.left();
            if left.is_zero() || self.wakes.recv_timeout(left).is_err() {
                panic!(
                    "waited until the deadline for {what}; output: {:?}",
                    String::from_utf8_lossy(&output)
                );
            }
        }
    }

    /// Stops the reader so `fiber`'s later output stays queued in the
    /// terminal until `wait`: every byte `fiber` writes from then on is
    /// undrained. A reader still running after the stop would prove
    /// nothing, so the stop is an assert naming the reader. A second
    /// call finds no reader and does nothing. After it, `read_until`
    /// and `screen_until` are not called: with no reader draining, their
    /// wakes never arrive and they wait until the deadline.
    pub(crate) fn stall(&mut self) {
        if let Some(reader) = self.reader.take() {
            assert!(reader.stop(), "expected the reader to stop");
        }
    }

    /// Restores the drain when stalled, waits for the child to exit
    /// and reaps it, then waits for the reader to end at end of file
    /// before stopping it, so every queued byte is read: stopping
    /// through the stop pipe while the master may still hold bytes
    /// drops them. Then stands the watchdog down and waits for the hub
    /// to idle out and remove its socket. Returns the exit and every
    /// byte read from the master since spawn.
    pub(crate) fn wait(mut self) -> Exited {
        if self.reader.is_none() {
            let main = self.main.try_clone().unwrap();
            let output = Arc::clone(&self.output);
            self.reader = Some(Reader::start_on(main, output, self.deadline));
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
            terminal: self.output.lock().unwrap().clone(),
        }
    }
}

impl Drop for Run {
    /// Stops the reader without waiting past the deadline: a panic
    /// mid-test stops the reader as well as killing the group through the
    /// watchdogs.
    fn drop(&mut self) {
        if let Some(reader) = self.reader.take() {
            reader.stop();
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

/// Where `needle` ends in `haystack` at or after `from`: an exact byte
/// match, unless `needle` holds a space and starts with a non-escape
/// byte, when each single space may instead be spaces, cursor moves and
/// SGR in any mix, and nothing else.
pub(crate) fn phrase_end(haystack: &[u8], from: usize, needle: &str) -> Option<usize> {
    let wanted = needle.as_bytes();
    if wanted.is_empty() {
        return Some(from.min(haystack.len()));
    }
    if !needle.contains(' ') || wanted[0] == 0x1b {
        return exact_end(haystack, from, wanted);
    }
    let words: Vec<&[u8]> = needle.split_whitespace().map(str::as_bytes).collect();
    if words.len() < 2 {
        return exact_end(haystack, from, wanted);
    }
    let mut cursor = from;
    while cursor + words[0].len() <= haystack.len() {
        let at = haystack[cursor..]
            .windows(words[0].len())
            .position(|window| window == words[0])?;
        let mut pos = cursor + at + words[0].len();
        let mut matched = true;
        for word in &words[1..] {
            let Some(gap) = frame_gap_end(haystack, pos) else {
                matched = false;
                break;
            };
            pos = gap;
            if haystack[pos..].starts_with(word) {
                pos += word.len();
            } else {
                matched = false;
                break;
            }
        }
        if matched {
            return Some(pos);
        }
        cursor += at + 1;
    }
    None
}

/// Where `needle` ends in `haystack` at or after `from`, as exact bytes.
fn exact_end(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from > haystack.len() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| from + at + needle.len())
}

/// Where the terminal frame's gap starting at `pos` ends: one or more
/// spaces, cursor moves and SGR sequences, and nothing else.
fn frame_gap_end(haystack: &[u8], mut pos: usize) -> Option<usize> {
    let start = pos;
    loop {
        if haystack.get(pos) == Some(&b' ') {
            pos += 1;
        } else if let Some(end) = cursor_move_end(haystack, pos).or_else(|| sgr_end(haystack, pos))
        {
            pos = end;
        } else {
            break;
        }
    }
    (pos > start).then_some(pos)
}

/// Where the cursor move at `pos` ends: `\x1b[<row>;<col>H`.
fn cursor_move_end(haystack: &[u8], pos: usize) -> Option<usize> {
    if haystack.get(pos) != Some(&0x1b) || haystack.get(pos + 1) != Some(&b'[') {
        return None;
    }
    let mut end = pos + 2;
    let row = end;
    while haystack.get(end).is_some_and(|byte| byte.is_ascii_digit()) {
        end += 1;
    }
    if end == row || haystack.get(end) != Some(&b';') {
        return None;
    }
    end += 1;
    let col = end;
    while haystack.get(end).is_some_and(|byte| byte.is_ascii_digit()) {
        end += 1;
    }
    if end == col || haystack.get(end) != Some(&b'H') {
        return None;
    }
    Some(end + 1)
}

/// Where the SGR sequence at `pos` ends: `\x1b[...m`.
fn sgr_end(haystack: &[u8], pos: usize) -> Option<usize> {
    if haystack.get(pos) != Some(&0x1b) || haystack.get(pos + 1) != Some(&b'[') {
        return None;
    }
    let mut end = pos + 2;
    while haystack
        .get(end)
        .is_some_and(|byte| byte.is_ascii_digit() || *byte == b';')
    {
        end += 1;
    }
    (haystack.get(end) == Some(&b'm')).then_some(end + 1)
}

/// A colour a screen cell holds: the terminal's default, a 256-palette
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

/// One screen cell: its symbol with the pen that wrote it. Unwritten
/// cells are a space on [`Colour::Default`], which is how a terminal
/// shows SGR 49.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cell {
    pub(crate) symbol: String,
    pub(crate) fg: Colour,
    pub(crate) bg: Colour,
    pub(crate) dim: bool,
    pub(crate) bold: bool,
}

/// The pen writing cells: the SGR state.
#[derive(Debug, Clone, Copy)]
struct Pen {
    fg: Colour,
    bg: Colour,
    bold: bool,
    dim: bool,
}

impl Default for Pen {
    /// SGR 0: every role the terminal's default colour.
    fn default() -> Self {
        Self {
            fg: Colour::Default,
            bg: Colour::Default,
            bold: false,
            dim: false,
        }
    }
}

/// A test-model screen reading the SGR stream: cursor addressing, the
/// pen, and erase display. It writes every UTF-8 char into one cell and
/// drops a char past the last column or row, with no autowrap: a
/// test-model limit, not a terminal. It holds for the cells the runs
/// assert (ASCII, `▌ ▐ ▄ ▀`, spaces), and ratatui's crossterm backend
/// re-addresses the cursor before any cell that does not follow the last
/// one it wrote, so a wide char misplaces only its own second cell, never
/// a later one.
pub(crate) struct Screen {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    cursor: (u16, u16),
    pen: Pen,
    /// An unfinished escape sequence or UTF-8 char at the last feed's
    /// end, completed by the next feed.
    pending: Vec<u8>,
}

impl Screen {
    /// A blank `cols` by `rows` screen.
    pub(crate) fn new(cols: u16, rows: u16) -> Self {
        let blank = Cell {
            symbol: " ".to_owned(),
            fg: Colour::Default,
            bg: Colour::Default,
            dim: false,
            bold: false,
        };
        Self {
            cols,
            rows,
            cells: vec![blank; usize::from(cols).saturating_mul(usize::from(rows))],
            cursor: (0, 0),
            pen: Pen::default(),
            pending: Vec::new(),
        }
    }

    /// The cell at `x`, `y`.
    pub(crate) fn cell(&self, x: u16, y: u16) -> &Cell {
        &self.cells[usize::from(y)
            .saturating_mul(usize::from(self.cols))
            .saturating_add(usize::from(x))]
    }

    /// Reads `bytes` onto the screen.
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
        let mut buf = std::mem::take(&mut self.pending);
        let mut i = 0;
        while i < buf.len() {
            let next = match buf[i] {
                0x1b => self.escape(&buf, i),
                b'\r' => {
                    self.cursor.0 = 0;
                    Some(i + 1)
                }
                b'\n' => {
                    self.cursor.1 = self.cursor.1.saturating_add(1);
                    Some(i + 1)
                }
                // Control bytes are never content: the attention bell
                // rings one (`docs/tui.md`, "Getting the person's
                // attention"), and no cell holds it.
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f => Some(i + 1),
                _ => self.utf8(&buf, i),
            };
            match next {
                Some(at) => i = at,
                None => break,
            }
        }
        self.pending = buf.split_off(i);
    }

    /// Writes `ch` at the cursor with the pen and moves one column.
    fn put(&mut self, ch: char) {
        let (cols, rows) = (usize::from(self.cols), usize::from(self.rows));
        if usize::from(self.cursor.0) < cols && usize::from(self.cursor.1) < rows {
            self.cells[usize::from(self.cursor.1)
                .saturating_mul(cols)
                .saturating_add(usize::from(self.cursor.0))] = Cell {
                symbol: ch.to_string(),
                fg: self.pen.fg,
                bg: self.pen.bg,
                dim: self.pen.dim,
                bold: self.pen.bold,
            };
        }
        self.cursor.0 = self.cursor.0.saturating_add(1);
    }

    /// Consumes the escape sequence at `i`: `Some` past it, or `None`
    /// when it is unfinished and a later feed completes it.
    fn escape(&mut self, buf: &[u8], i: usize) -> Option<usize> {
        match buf.get(i + 1)? {
            b'[' => self.csi(buf, i),
            // OSC to BEL or ESC backslash.
            b']' => {
                let mut j = i + 2;
                loop {
                    match buf.get(j) {
                        None => return None,
                        Some(0x07) => return Some(j + 1),
                        Some(0x1b) if buf.get(j + 1) == Some(&b'\\') => {
                            return Some(j + 2);
                        }
                        Some(_) => j += 1,
                    }
                }
            }
            // DCS to ESC backslash.
            b'P' => {
                let mut j = i + 2;
                loop {
                    match buf.get(j) {
                        None => return None,
                        Some(0x1b) if buf.get(j + 1) == Some(&b'\\') => {
                            return Some(j + 2);
                        }
                        Some(_) => j += 1,
                    }
                }
            }
            // Any other ESC x is both bytes.
            _ => Some(i + 2),
        }
    }

    /// Consumes the CSI sequence at `i`: cursor addressing, the pen, and
    /// erase display update the model, and every other sequence is
    /// skipped. `Some` past it, or `None` when it is unfinished.
    fn csi(&mut self, buf: &[u8], i: usize) -> Option<usize> {
        let mut j = i + 2;
        while buf.get(j).is_some_and(|byte| (0x20..=0x3F).contains(byte)) {
            j += 1;
        }
        let final_byte = *buf.get(j)?;
        if !(0x40..=0x7E).contains(&final_byte) {
            return Some(j + 1);
        }
        match final_byte {
            b'm' => self.sgr(&buf[i + 2..j]),
            b'H' => {
                let moves: Vec<&[u8]> = buf[i + 2..j].split(|byte| *byte == b';').collect();
                let row = moves.first().map_or(1, |digits| param(digits));
                let col = moves.get(1).map_or(1, |digits| param(digits));
                self.cursor = (col.saturating_sub(1), row.saturating_sub(1));
            }
            b'J' if buf[i + 2..j] == *b"2" => {
                let blank = Cell {
                    symbol: " ".to_owned(),
                    fg: self.pen.fg,
                    bg: self.pen.bg,
                    dim: false,
                    bold: false,
                };
                for cell in &mut self.cells {
                    *cell = blank.clone();
                }
            }
            _ => {}
        }
        Some(j + 1)
    }

    /// Updates the pen from one SGR parameter list: empty or 0 resets; 1
    /// and 2 set bold and dim; 22 clears both; 39 and 49 take the default;
    /// 38 and 48 take an indexed or RGB colour; 58 with its
    /// sub-parameters and 59 are consumed and ignored; anything else is
    /// ignored.
    fn sgr(&mut self, params: &[u8]) {
        let nums: Vec<u16> = if params.is_empty() {
            vec![0]
        } else {
            params
                .split(|byte| *byte == b';')
                .map(|digits| {
                    std::str::from_utf8(digits)
                        .unwrap_or("")
                        .parse()
                        .unwrap_or(0)
                })
                .collect()
        };
        let mut i = 0;
        while i < nums.len() {
            let step = match nums[i] {
                0 => {
                    self.pen = Pen::default();
                    1
                }
                1 => {
                    self.pen.bold = true;
                    1
                }
                2 => {
                    self.pen.dim = true;
                    1
                }
                22 => {
                    self.pen.bold = false;
                    self.pen.dim = false;
                    1
                }
                39 => {
                    self.pen.fg = Colour::Default;
                    1
                }
                49 => {
                    self.pen.bg = Colour::Default;
                    1
                }
                59 => 1,
                38 | 48 | 58 => {
                    let extended = nums[i];
                    let (colour, len) = match nums.get(i + 1) {
                        Some(5) => (nums.get(i + 2).map(|n| Colour::Indexed(sat(*n))), 3),
                        Some(2) => (
                            match (nums.get(i + 2), nums.get(i + 3), nums.get(i + 4)) {
                                (Some(r), Some(g), Some(b)) => {
                                    Some(Colour::Rgb(sat(*r), sat(*g), sat(*b)))
                                }
                                _ => None,
                            },
                            5,
                        ),
                        _ => (None, 1),
                    };
                    match (extended, colour) {
                        (38, Some(colour)) => self.pen.fg = colour,
                        (48, Some(colour)) => self.pen.bg = colour,
                        _ => {}
                    }
                    len
                }
                _ => 1,
            };
            i += step;
        }
    }

    /// Writes the UTF-8 char at `i` with the pen: `Some` past it, or
    /// `None` when its bytes are split across feeds. One char takes one
    /// cell, however wide the terminal draws it.
    fn utf8(&mut self, buf: &[u8], i: usize) -> Option<usize> {
        let end = (i + 4).min(buf.len());
        match std::str::from_utf8(&buf[i..end]) {
            Ok(text) => {
                let ch = text.chars().next().unwrap();
                self.put(ch);
                Some(i + ch.len_utf8())
            }
            Err(err) if err.valid_up_to() > 0 => {
                let text = std::str::from_utf8(&buf[i..i + err.valid_up_to()]).unwrap();
                let ch = text.chars().next().unwrap();
                self.put(ch);
                Some(i + ch.len_utf8())
            }
            Err(err) if err.error_len().is_some() => Some(i + 1),
            Err(_) => None,
        }
    }
}

/// One SGR parameter: digits, else the default 1.
fn param(digits: &[u8]) -> u16 {
    std::str::from_utf8(digits)
        .unwrap_or("")
        .parse()
        .unwrap_or(1)
}

/// A palette entry: SGR sends bytes.
fn sat(entry: u16) -> u8 {
    u8::try_from(entry).unwrap_or(u8::MAX)
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
