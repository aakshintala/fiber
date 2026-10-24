//! Small shared mechanisms with no domain knowledge: the clock traits, the
//! process clock and the clock-parked wait, the cancel signal, the process-group
//! guard, the poison-ignoring `lock`, the admission lock, the stoppable read, the line
//! framing, lowercase hex and SHA-256, the `mint` id, the atomic file write,
//! and the scrubbed `git`
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

/// The process-group guard, the process-wide list and the signals
/// (`docs/testing.md`, "Running tests").
pub mod group;

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

/// Lowercase hex and SHA-256 as lowercase hex, each built into one `String`.
mod hex;

/// Text on one line: control characters as spaces.
pub mod text;

pub use atomic::write_atomic;

pub use hex::{hex, sha256_hex};

pub use id::mint;

/// Locks `mutex`, returning the guard even when another thread panicked
/// while holding it: a poisoned lock still guards live state.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
