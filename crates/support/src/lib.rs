//! Small shared mechanisms with no domain knowledge: the clock traits, the
//! process clock and the clock-parked wait, the cancel signal, the process-group
//! guard, the poison-ignoring `lock`, the admission lock, the stoppable read, the line
//! framing, the counted body reader, lowercase hex and SHA-256, the `mint` id,
//! the atomic file write, and the scrubbed `git`
//! command builder (`docs/architecture.md`, "The modules").

use std::sync::{Mutex, MutexGuard};

/// The admission lock: a door that admits until it is closed, once and for good.
pub mod admission;

/// An atomic whole-file write: bytes synced to a temporary beside the
/// destination, then renamed over it.
pub mod atomic;

/// The cancel signal a call watches, and the `Cancel` trait behind it
/// (`docs/tools.md`, "Cancellation").
pub mod cancel;

/// The clock seam, the process clock and the clock-parked wait
/// (`docs/testing.md`, "Values that change every run").
pub mod clock;

/// One deadline for a test's waits (`docs/testing.md`, "Waits and timeouts").
pub mod deadline;

/// The process-group guard, the process-wide list and the signals
/// (`docs/testing.md`, "Running tests").
pub mod group;

/// Kernel process identity for socket attribution: the peer process id
/// and the ancestor walk (`docs/permissions.md`, "Sessions on the sockets").
pub mod peer;

/// A `git` command with the scrubbed environment: `core.fsmonitor` off,
/// the lock and terminal-prompt guards pinned, the redirecting variables
/// removed, and hooks off unless the caller keeps them.
pub mod git;

/// A new id from random bytes, such as a session's or a command's.
pub mod id;

/// The stoppable read: a socket reader that a `Stop` ends on demand.
pub mod stoppable;

/// One wire line: bytes to the next line feed.
pub mod framing;

/// Counts bytes read through a body reader, including a partial read before an error.
pub mod counted {
    use std::io::{self, Read};

    /// Counts the bytes a caller reads from `inner`, even when a later read fails.
    pub struct Counted<R> {
        inner: R,
        bytes: u64,
    }

    impl<R> Counted<R> {
        /// Wraps `inner` with a zero byte count.
        pub fn new(inner: R) -> Self {
            Self { inner, bytes: 0 }
        }

        /// The number of bytes successfully read through this wrapper.
        pub fn consumed(&self) -> u64 {
            self.bytes
        }
    }

    impl<R: Read> Read for Counted<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let read = self.inner.read(buf)?;
            self.bytes = self
                .bytes
                .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
            Ok(read)
        }
    }

    #[cfg(test)]
    mod tests {
        use std::io::{self, Cursor, Read};

        use super::Counted;

        #[test]
        fn counts_successful_reads() {
            let mut counted = Counted::new(Cursor::new(b"abc"));
            let mut bytes = [0; 2];
            assert_eq!(counted.read(&mut bytes).unwrap(), 2);
            assert_eq!(counted.consumed(), 2);
            assert_eq!(counted.read(&mut bytes).unwrap(), 1);
            assert_eq!(counted.consumed(), 3);
            assert_eq!(counted.read(&mut bytes).unwrap(), 0);
            assert_eq!(counted.consumed(), 3);
        }

        struct ThenFail(bool);

        impl Read for ThenFail {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("read failed"));
                }
                self.0 = true;
                bytes[..2].copy_from_slice(b"ab");
                Ok(2)
            }
        }

        #[test]
        fn keeps_bytes_read_before_an_error() {
            let mut counted = Counted::new(ThenFail(false));
            let mut bytes = [0; 4];
            assert_eq!(counted.read(&mut bytes).unwrap(), 2);
            assert_eq!(counted.consumed(), 2);
            assert!(counted.read(&mut bytes).is_err());
            assert_eq!(counted.consumed(), 2);
        }
    }
}

/// Lowercase hex and SHA-256 as lowercase hex, each built into one
/// `String`; and percent-decoding.
mod hex;

/// Text on one line: control characters as spaces.
pub mod text;

pub use atomic::write_atomic;

pub use hex::{hex, percent_decode, sha256_hex};

pub use id::mint;

/// Locks `mutex`, returning the guard even when another thread panicked
/// while holding it: a poisoned lock still guards live state.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
