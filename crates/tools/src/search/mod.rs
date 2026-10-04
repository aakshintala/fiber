//! The search behind the shell's `grep` and `find` (`docs/tools.md`, "Search").
//!
//! The shell tool defines two shell functions that run the Fiber binary's
//! hidden `grep` and `find` subcommands; those land here. A call the
//! built-in does not handle replaces the process with the system tool
//! before anything is read or written.

mod bre;
mod fallback;
mod find;
mod grep;
mod grep_args;
mod notice;
mod walk;

pub use find::find_main;
pub use grep::grep_main;

use std::io::{self, Write};

/// What running a search decided.
pub(crate) enum Outcome {
    /// Finished. The value is the exit code: 0, 1 or 2.
    Done(i32),
    /// The call needs a flag or argument the built-in does not handle. The
    /// caller replaces itself with the system tool, before the first byte
    /// is read from standard input or written to standard output.
    Fallback,
}

/// Standard output behind one locked buffered writer: a closed pipe stops
/// the search quietly with the exit it had so far, while any other write
/// failure is kept for the caller to report as an error.
pub(crate) struct Out<'a> {
    writer: &'a mut dyn Write,
    broken: bool,
    error: Option<String>,
}

impl<'a> Out<'a> {
    /// Writes through `writer`, stopping at the first failure.
    pub(crate) fn new(writer: &'a mut dyn Write) -> Self {
        Self {
            writer,
            broken: false,
            error: None,
        }
    }

    /// Writes `bytes`, or nothing when an earlier write failed.
    pub(crate) fn emit(&mut self, bytes: &[u8]) {
        if self.broken {
            return;
        }
        if let Err(error) = self.writer.write_all(bytes) {
            self.fail(error);
        }
    }

    /// Flushes what is buffered. A failed flush reads as a failed write.
    pub(crate) fn flush(&mut self) {
        if self.broken {
            return;
        }
        if let Err(error) = self.writer.flush() {
            self.fail(error);
        }
    }

    /// Whether a write failed: the rest of the output is dropped.
    pub(crate) fn broken(&self) -> bool {
        self.broken
    }

    /// The first write failure that was not a closed pipe, if any: the
    /// caller reports it and exits 2.
    pub(crate) fn take_error(&mut self) -> Option<String> {
        self.error.take()
    }

    /// Stops the search: a closed pipe stays quiet, any other failure is
    /// kept for [`Out::take_error`].
    fn fail(&mut self, error: io::Error) {
        self.broken = true;
        if error.kind() != io::ErrorKind::BrokenPipe {
            self.error = Some(error.to_string());
        }
    }
}

/// The exit after the final flush of the buffered writer: a closed pipe
/// keeps the exit the search had so far, any other failure prints
/// `<program>: writing output: <reason>` and exits 2.
pub(crate) fn flush_exit(
    program: &str,
    code: i32,
    flushed: io::Result<()>,
    stderr: &mut dyn Write,
) -> i32 {
    match flushed {
        Ok(()) => code,
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => code,
        // A run that already failed printed its own line.
        Err(_) if code == 2 => code,
        Err(error) => {
            writeln!(stderr, "{program}: writing output: {error}").unwrap_or(());
            2
        }
    }
}

/// How long one system tool a test starts may take (`docs/testing.md`,
/// "Waits and timeouts").
#[cfg(test)]
const TOOL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// Waits for a spawned system tool under [`TOOL_DEADLINE`]: on expiry the
/// child's process group is killed and the test fails naming `what` it
/// waited for. The child belongs in its own process group, as the binary
/// tests spawn theirs.
#[cfg(test)]
pub(crate) fn wait_output(child: std::process::Child, what: &str) -> std::process::Output {
    let group = child.id();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(child.wait_with_output()).unwrap_or(());
    });
    match finished.recv_timeout(TOOL_DEADLINE) {
        Ok(Ok(output)) => output,
        Ok(Err(_)) => panic!("the waiter for {what} died"),
        Err(_) => {
            fakes::kill_group(group, "KILL").unwrap_or(false);
            let reaped = finished.recv_timeout(TOOL_DEADLINE).is_ok();
            panic!("waited {TOOL_DEADLINE:?} for {what} (reaped after the kill: {reaped})");
        }
    }
}

/// Parses ASCII digits into a number: nothing for an empty value,
/// anything but digits, or an overflow.
pub(crate) fn decimal(value: &[u8]) -> Option<usize> {
    if value.is_empty() || !value.iter().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut number = 0usize;
    for byte in value {
        number = number
            .checked_mul(10)
            .and_then(|shifted| shifted.checked_add(usize::from(*byte - b'0')))?;
    }
    Some(number)
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
