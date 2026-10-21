//! The terminal in a pseudo-terminal, as `crates/main/tests/terminal.rs`
//! runs it: bare `fiber` with standard input, output and error on the
//! terminal side, `TERM=xterm-256color`, and one reader thread feeding
//! what it draws to the screen the waits read. While recording, the
//! reader also cuts each synchronized-output frame out of the byte
//! stream.

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use contract::clock::Clock;

use crate::home::Home;
use crate::run::{self, Proc, left};
use crate::screen::Screen;

/// The terminal under test and what it has drawn.
pub(crate) struct Terminal {
    pub(crate) proc: Proc,
    main: fs::File,
    output: Arc<Mutex<Output>>,
    /// One wake per chunk appended.
    wakes: mpsc::Receiver<()>,
}

/// What the terminal has drawn: the raw bytes, the screen they leave,
/// and the recorded frames.
struct Output {
    raw: Vec<u8>,
    screen: Screen,
    recording: bool,
    recorder: Recorder,
}

fn err(what: &str) -> impl Fn(rustix::io::Errno) -> String + '_ {
    move |errno| format!("{what}: {errno}")
}

/// One synchronized-output frame (mode 2026) drawn while recording:
/// when the chunk holding its end marker was read, how many bytes the
/// block held from its begin marker through its end marker, and the
/// whole screen right after the end marker. The caller cuts the
/// conversation column out of `text`: the grid keeps every column so one
/// record serves every width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Frame {
    pub(crate) at: Instant,
    pub(crate) bytes: usize,
    pub(crate) text: String,
}

/// The mode-2026 blocks seen while recording: whether one is open, how
/// many bytes it holds so far, the unsettled tail (at most one frame's
/// worth, since every marker the scan needs arrives whole), and the
/// frames completed. A marker split across two reads joins in `pending`:
/// only whole markers move the state, so a prefix at the end of a chunk
/// waits for the next one.
struct Recorder {
    cols: usize,
    open: bool,
    block_len: usize,
    pending: Vec<u8>,
    frames: Vec<Frame>,
}

/// One synchronized-output marker's length: begin and end are both 8
/// bytes.
const MARKER: usize = 8;

/// The block's begin marker.
const SYNC_BEGIN: &[u8] = b"\x1b[?2026h";

/// The block's end marker.
const SYNC_END: &[u8] = b"\x1b[?2026l";

impl Recorder {
    /// Ready to record on a `cols`-wide screen.
    fn new(cols: usize) -> Self {
        Self {
            cols,
            open: false,
            block_len: 0,
            pending: Vec::new(),
            frames: Vec::new(),
        }
    }

    /// Folds `chunk` into the blocks seen so far, feeding `screen` exactly
    /// the settled bytes: up to and through each whole marker, so the
    /// snapshot for a frame sees the screen right after its end marker
    /// and before the next byte. Bytes outside any block feed the screen
    /// but record nothing.
    fn push(&mut self, screen: &mut Screen, chunk: &[u8], now: Instant) {
        self.pending.extend_from_slice(chunk);
        let mut cursor = 0usize;
        let mut at = 0usize;
        while at.saturating_add(MARKER) <= self.pending.len() {
            let begin = self
                .pending
                .get(at..)
                .is_some_and(|rest| rest.starts_with(SYNC_BEGIN));
            let end = !begin
                && self
                    .pending
                    .get(at..)
                    .is_some_and(|rest| rest.starts_with(SYNC_END));
            if !begin && !end {
                at = at.saturating_add(1);
                continue;
            }
            let through = at.saturating_add(MARKER);
            if begin {
                self.open = true;
                self.block_len = MARKER;
                if let Some(span) = self.pending.get(cursor..through) {
                    screen.feed(span);
                }
            } else if self.open {
                self.block_len = self
                    .block_len
                    .saturating_add(through.saturating_sub(cursor));
                if let Some(span) = self.pending.get(cursor..through) {
                    screen.feed(span);
                }
                self.frames.push(Frame {
                    at: now,
                    bytes: self.block_len,
                    text: screen.columns(0, self.cols),
                });
                self.open = false;
                self.block_len = 0;
            } else if let Some(span) = self.pending.get(cursor..through) {
                screen.feed(span);
            }
            cursor = through;
            at = through;
        }
        self.pending.drain(..cursor);
    }
}

/// The first-run import offer's title: fresh bench homes hold no
/// receipts, so a bare home terminal always opens it over the prompt.
pub(crate) const OFFER_NEEDLE: &str = "Import settings";
/// The home prompt marker, drawn once the offer is gone.
const PROMPT_NEEDLE: &str = "\u{203a}";

/// What a bare home terminal shows while its first-run offer may be up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Offer {
    /// The offer is up: Esc dismisses it for the run.
    Up,
    /// The prompt is already there: no offer to dismiss.
    Gone,
    /// Neither marker yet: keep waiting.
    Waiting,
}

/// Which of the three the screen shows: the offer wins, since it covers
/// the prompt it opens over.
pub(crate) fn offer_state(screen: &Screen) -> Offer {
    if screen.holds(OFFER_NEEDLE) {
        Offer::Up
    } else if screen.holds(PROMPT_NEEDLE) {
        Offer::Gone
    } else {
        Offer::Waiting
    }
}

/// The timeout error with what the terminal had drawn: its byte count, the
/// last 3000 bytes as lossy text, and the screen's rows as text, so a
/// failed wait shows where the replay stood.
pub(crate) fn timeout_note(expired: String, bytes: &[u8], screen: &Screen) -> String {
    const TAIL: usize = 3000;
    let tail = bytes.get(bytes.len().saturating_sub(TAIL)..).unwrap_or(&[]);
    format!(
        "{expired}; drew {} bytes, ending {:?}, screen:\n{}",
        bytes.len(),
        String::from_utf8_lossy(tail),
        screen.text()
    )
}

impl Terminal {
    /// Starts `fiber` with `args` in `home`'s workspace on a new pty at
    /// 60x12.
    pub(crate) fn spawn(
        home: &Home,
        args: &[&str],
        path: Option<&OsStr>,
        clock: &dyn Clock,
    ) -> Result<Self, String> {
        Self::spawn_sized(home, args, path, clock, 60, 12)
    }

    /// Starts `fiber` with `args` in `home`'s workspace on a new pty at
    /// `cols` by `rows`.
    pub(crate) fn spawn_sized(
        home: &Home,
        args: &[&str],
        path: Option<&OsStr>,
        clock: &dyn Clock,
        cols: u16,
        rows: u16,
    ) -> Result<Self, String> {
        let (main, name) = fakes::pty::open().map_err(|err| format!("opening a pty: {err}"))?;
        let terminal = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .map_err(|e| format!("opening {}: {e}", name.display()))?;
        rustix::termios::tcsetwinsize(
            &terminal,
            rustix::termios::Winsize {
                ws_col: cols,
                ws_row: rows,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .map_err(err("sizing the pty"))?;
        let side = || {
            terminal
                .try_clone()
                .map(Stdio::from)
                .map_err(|e| format!("cloning the pty: {e}"))
        };
        let mut command = crate::run::command(home.fiber(), home.root(), &home.home(), path);
        command
            .current_dir(home.workspace())
            .args(args)
            .env("TERM", "xterm-256color")
            .stdin(side()?)
            .stdout(side()?)
            .stderr(side()?);
        let proc = Proc::spawn(&mut command, clock)?;
        drop(command);
        drop(terminal);
        Self::watch(proc, main, cols, rows)
    }

    /// Watches `proc`'s pty `main` with the reader thread feeding the
    /// screen: what `spawn` hands a started child, and what the bench
    /// tests hand a fake one.
    pub(crate) fn watch(proc: Proc, main: fs::File, cols: u16, rows: u16) -> Result<Self, String> {
        let reader = main
            .try_clone()
            .map_err(|e| format!("cloning the pty master: {e}"))?;
        let output = Arc::new(Mutex::new(Output {
            raw: Vec::new(),
            screen: Screen::new(cols.into(), rows.into()),
            recording: false,
            recorder: Recorder::new(cols.into()),
        }));
        let appended = Arc::clone(&output);
        let (tx, wakes) = mpsc::channel();
        fakes::pty::read_to_eof(reader, move |chunk| {
            if let Ok(mut out) = appended.lock() {
                let Output {
                    raw,
                    screen,
                    recording,
                    recorder,
                } = &mut *out;
                raw.extend_from_slice(chunk);
                if *recording {
                    recorder.push(screen, chunk, run::System.now());
                } else {
                    screen.feed(chunk);
                }
            }
            tx.send(()).unwrap_or(());
        });
        Ok(Self {
            proc,
            main,
            output,
            wakes,
        })
    }

    /// Dismisses the first-run import offer the way a person does: waits
    /// until the offer draws and sends Esc, or returns at once when the
    /// prompt is already there. Fresh bench homes hold no receipts, so
    /// the offer always opens; Esc on a plain home is a no-op in the
    /// harness, so the dismissal never disturbs a terminal with nothing
    /// to dismiss.
    pub(crate) fn dismiss_first_run_offer(
        &mut self,
        clock: &dyn Clock,
        until: Instant,
    ) -> Result<(), String> {
        let what = format!("{OFFER_NEEDLE:?} or {PROMPT_NEEDLE:?} on the terminal");
        loop {
            if let Some(done) = self.offer() {
                return done;
            }
            let wait = match left(clock, until, &what) {
                Ok(wait) => wait,
                Err(expired) => {
                    return Err(match self.output.lock() {
                        Ok(output) => timeout_note(expired, &output.raw, &output.screen),
                        Err(_) => expired,
                    });
                }
            };
            #[allow(
                clippy::disallowed_methods,
                reason = "the bench's wait, computed from its injected clock"
            )]
            let woke = self.wakes.recv_timeout(wait);
            match woke {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("the terminal closed before {what}"));
                }
            }
        }
    }

    /// The offer's state now: Esc on `Up`, done on `Gone`, and `None`
    /// while neither marker shows.
    fn offer(&mut self) -> Option<Result<(), String>> {
        match self
            .output
            .lock()
            .ok()
            .map(|output| offer_state(&output.screen))?
        {
            Offer::Up => Some(self.write(b"\x1b")),
            Offer::Gone => Some(Ok(())),
            Offer::Waiting => None,
        }
    }

    fn holds(&self, needle: &str) -> bool {
        self.output
            .lock()
            .map(|output| output.screen.holds(needle))
            .unwrap_or(false)
    }

    /// The first row and column holding `needle` on the live screen, for
    /// clicks on rows the waits locate: the screen's own position query.
    pub(crate) fn find(&self, needle: &str) -> Option<(usize, usize)> {
        self.output
            .lock()
            .ok()
            .and_then(|output| output.screen.find(needle))
    }

    /// Waits until `until` for the screen to hold `needle`.
    pub(crate) fn wait_for(
        &self,
        clock: &dyn Clock,
        until: Instant,
        needle: &str,
    ) -> Result<(), String> {
        let what = format!("{needle:?} on the terminal");
        while !self.holds(needle) {
            let wait = match left(clock, until, &what) {
                Ok(wait) => wait,
                Err(expired) => {
                    return Err(match self.output.lock() {
                        Ok(output) => timeout_note(expired, &output.raw, &output.screen),
                        Err(_) => expired,
                    });
                }
            };
            #[allow(
                clippy::disallowed_methods,
                reason = "the bench's wait, computed from its injected clock"
            )]
            let woke = self.wakes.recv_timeout(wait);
            match woke {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("the terminal closed before {what}"));
                }
            }
        }
        Ok(())
    }

    /// Records each complete mode-2026 frame while `on`: the frames stay
    /// available through [`Terminal::frames`]. Off by default.
    pub(crate) fn record(&self, on: bool) {
        if let Ok(mut out) = self.output.lock() {
            out.recording = on;
        }
    }

    /// The frames recorded so far, oldest first.
    pub(crate) fn frames(&self) -> Vec<Frame> {
        self.output
            .lock()
            .map(|out| out.recorder.frames.clone())
            .unwrap_or_default()
    }

    /// Types `bytes` into the terminal.
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.main
            .write_all(bytes)
            .map_err(|e| format!("typing into the terminal: {e}"))
    }
}

#[cfg(test)]
#[path = "pty_tests.rs"]
mod tests;
