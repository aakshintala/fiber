//! The terminal in a pseudo-terminal, as `crates/main/tests/terminal.rs`
//! runs it: bare `fiber` with standard input, output and error on the
//! terminal side at 60x12, `TERM=xterm-256color`, and one reader thread
//! feeding what it draws to the screen the waits read.

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;

use contract::clock::Clock;

use crate::home::Home;
use crate::run::{Proc, left};
use crate::screen::Screen;

/// The terminal under test and what it has drawn.
pub(crate) struct Terminal {
    pub(crate) proc: Proc,
    main: fs::File,
    output: Arc<Mutex<Output>>,
    /// One wake per chunk appended.
    wakes: mpsc::Receiver<()>,
}

/// What the terminal has drawn: the raw bytes and the screen they leave.
struct Output {
    raw: Vec<u8>,
    screen: Screen,
}

fn err(what: &str) -> impl Fn(rustix::io::Errno) -> String + '_ {
    move |errno| format!("{what}: {errno}")
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
    /// Starts `fiber` with `args` in `home`'s workspace on a new pty.
    pub(crate) fn spawn(
        home: &Home,
        args: &[&str],
        path: Option<&OsStr>,
        clock: &dyn Clock,
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
                ws_col: 60,
                ws_row: 12,
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
        let reader = main
            .try_clone()
            .map_err(|e| format!("cloning the pty master: {e}"))?;
        let output = Arc::new(Mutex::new(Output {
            raw: Vec::new(),
            screen: Screen::new(60, 12),
        }));
        let appended = Arc::clone(&output);
        let (tx, wakes) = mpsc::channel();
        fakes::pty::read_to_eof(reader, move |chunk| {
            if let Ok(mut out) = appended.lock() {
                out.raw.extend_from_slice(chunk);
                out.screen.feed(chunk);
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

    fn holds(&self, needle: &str) -> bool {
        self.output
            .lock()
            .map(|output| output.screen.holds(needle))
            .unwrap_or(false)
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
            match self.wakes.recv_timeout(wait) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("the terminal closed before {what}"));
                }
            }
        }
        Ok(())
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
