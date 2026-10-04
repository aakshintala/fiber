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

use std::io::Write;

/// What running a search decided.
pub(crate) enum Outcome {
    /// Finished. The value is the exit code: 0, 1 or 2.
    Done(i32),
    /// The call needs a flag or argument the built-in does not handle. The
    /// caller replaces itself with the system tool, before the first byte
    /// is read from standard input or written to standard output.
    Fallback,
}

/// Standard output behind one locked buffered writer: a write that fails,
/// such as a pipe closing early, stops the search quietly with the exit it
/// had so far.
pub(crate) struct Out<'a> {
    writer: &'a mut dyn Write,
    broken: bool,
}

impl<'a> Out<'a> {
    /// Writes through `writer`, stopping quietly at the first failure.
    pub(crate) fn new(writer: &'a mut dyn Write) -> Self {
        Self {
            writer,
            broken: false,
        }
    }

    /// Writes `bytes`, or nothing when an earlier write failed.
    pub(crate) fn emit(&mut self, bytes: &[u8]) {
        if self.broken {
            return;
        }
        if self.writer.write_all(bytes).is_err() {
            self.broken = true;
        }
    }

    /// Flushes what is buffered. A failed flush reads as a failed pipe.
    pub(crate) fn flush(&mut self) {
        if self.broken {
            return;
        }
        if self.writer.flush().is_err() {
            self.broken = true;
        }
    }

    /// Whether a write failed: the rest of the output is dropped quietly.
    pub(crate) fn broken(&self) -> bool {
        self.broken
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
