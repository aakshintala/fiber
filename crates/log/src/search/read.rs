//! The reader every log and artifact is searched through: it sees a
//! cancelled call between chunks (`docs/tools.md`, "Cancellation": a
//! built-in Rust tool "checks for cancellation between chunks of work").

use std::io::{self, Read};

use contract::tool::Cancel;

/// `inner`, failing every read once the call is cancelled, and noting when
/// it reached the end.
pub(super) struct Cancelling<'a, R> {
    /// The file.
    inner: R,
    /// The call's signal.
    cancel: &'a dyn Cancel,
    /// Whether a read returned the end of the file.
    eof: bool,
}

impl<'a, R: Read> Cancelling<'a, R> {
    /// `inner`, read until `cancel` fires.
    pub(super) fn new(inner: R, cancel: &'a dyn Cancel) -> Self {
        Self {
            inner,
            cancel,
            eof: false,
        }
    }

    /// Whether a read returned the end of the file: a search that stopped
    /// early never sets it.
    pub(super) fn eof(&self) -> bool {
        self.eof
    }
}

impl<R: Read> Read for Cancelling<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Not `Interrupted`, which a reader retries.
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("cancelled"));
        }
        let read = self.inner.read(buf)?;
        if read == 0 && !buf.is_empty() {
            self.eof = true;
        }
        Ok(read)
    }
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
