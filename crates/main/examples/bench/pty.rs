//! The terminal in a pseudo-terminal, as `crates/main/tests/terminal.rs`
//! runs it: bare `fiber` with standard input, output and error on the
//! terminal side at 60x12, `TERM=xterm-256color`, and one reader thread
//! appending what it draws.

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Instant;

use contract::clock::Clock;
use rustix::pty;

use crate::home::Home;
use crate::run::{Proc, left};

/// The terminal under test and what it has drawn.
pub(crate) struct Terminal {
    pub(crate) proc: Proc,
    main: fs::File,
    output: Arc<Mutex<Vec<u8>>>,
    /// One wake per chunk appended.
    wakes: mpsc::Receiver<()>,
}

fn err(what: &str) -> impl Fn(rustix::io::Errno) -> String + '_ {
    move |errno| format!("{what}: {errno}")
}

/// The length of the cursor-position sequence `ESC [ <row> ; <col> H` at
/// the start of `bytes`, if one is there.
fn cursor_position(bytes: &[u8]) -> Option<usize> {
    let rest = bytes.strip_prefix(b"\x1b[")?;
    let digits = |from: &[u8]| from.iter().take_while(|b| b.is_ascii_digit()).count();
    let row = digits(rest);
    let rest = rest.get(row..)?.strip_prefix(b";").filter(|_| row > 0)?;
    let col = digits(rest);
    rest.get(col..)?.strip_prefix(b"H").filter(|_| col > 0)?;
    Some(2 + row + 1 + col + 1)
}

/// Whether `needle` matches `output` from its start. Each space in
/// `needle` matches a space or one cursor-position sequence: a frame
/// skips the cells it leaves unchanged, a blank cell between two words
/// included, and moves the cursor past them.
fn matches_at(mut output: &[u8], needle: &[u8]) -> bool {
    for &byte in needle {
        let skip = if byte == b' ' && output.first() != Some(&b' ') {
            cursor_position(output)
        } else {
            (output.first() == Some(&byte)).then_some(1)
        };
        match skip.and_then(|skip| output.get(skip..)) {
            Some(rest) => output = rest,
            None => return false,
        }
    }
    true
}

/// Whether `output` holds `needle` anywhere ([`matches_at`]). An empty
/// needle matches nothing.
fn holds(output: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && (0..output.len()).any(|start| {
            output
                .get(start..)
                .is_some_and(|rest| matches_at(rest, needle))
        })
}

impl Terminal {
    /// Starts `fiber` with `args` in `home`'s workspace on a new pty.
    pub(crate) fn spawn(
        home: &Home,
        args: &[&str],
        path: Option<&OsStr>,
        clock: &dyn Clock,
    ) -> Result<Self, String> {
        let main = pty::openpt(pty::OpenptFlags::RDWR | pty::OpenptFlags::NOCTTY)
            .map_err(err("opening a pty"))?;
        // Not inherited: a hub the terminal starts would hold the master open.
        rustix::io::fcntl_setfd(&main, rustix::io::FdFlags::CLOEXEC)
            .map_err(err("marking the pty close-on-exec"))?;
        pty::grantpt(&main).map_err(err("granting the pty"))?;
        pty::unlockpt(&main).map_err(err("unlocking the pty"))?;
        let name = pty::ptsname(&main, Vec::new()).map_err(err("naming the pty"))?;
        let name = PathBuf::from(OsStr::from_bytes(name.as_bytes()));
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
        let main = fs::File::from(main);
        let mut reader = main
            .try_clone()
            .map_err(|e| format!("cloning the pty master: {e}"))?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let appended = Arc::clone(&output);
        let (tx, wakes) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                let Some(chunk) = buf.get(..n) else { break };
                match appended.lock() {
                    Ok(mut output) => output.extend_from_slice(chunk),
                    Err(_) => break,
                }
                if tx.send(()).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            proc,
            main,
            output,
            wakes,
        })
    }

    fn holds(&self, needle: &[u8]) -> bool {
        self.output
            .lock()
            .map(|output| holds(&output, needle))
            .unwrap_or(false)
    }

    /// Waits until `until` for the output to hold `needle`.
    pub(crate) fn wait_for(
        &self,
        clock: &dyn Clock,
        until: Instant,
        needle: &str,
    ) -> Result<(), String> {
        let what = format!("{needle:?} on the terminal");
        while !self.holds(needle.as_bytes()) {
            match self.wakes.recv_timeout(left(clock, until, &what)?) {
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
