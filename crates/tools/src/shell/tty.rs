//! The pseudo-terminal a `tty` command runs in (`docs/tools.md`, "Terminal
//! (`tty`)"). Fiber keeps the primary side: its reads are the command's
//! output, its writes are `jobs write`. The command gets the secondary side
//! as stdin, stdout and stderr, and as its controlling terminal.

use std::fs::File;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use contract::clock::Clock;
use contract::jobs::Input;
use contract::tool::Cancel;
use rustix::fs::{Mode, OFlags};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

use super::command::park;
use super::output::{Shared, lock};

/// A `tty` call's receipt carries the output that arrives in this long
/// (`docs/tools.md`, "Terminal (`tty`)").
const FIRST_OUTPUT: Duration = Duration::from_millis(250);

/// The receipt carries at most this much of the output: 16 KiB, the tool's
/// default cut (`docs/tools.md`, "Result and output").
const RECEIPT_CUT: u64 = 16 * 1024;

/// One terminal, before the command starts.
pub(super) struct Terminal {
    /// The primary's read side: what the command prints.
    pub reader: File,
    /// The secondary, for the command's three standard streams.
    pub secondary: OwnedFd,
    /// Types into the primary.
    pub input: Input,
}

/// Opens a terminal. The primary is close-on-exec, so no command inherits
/// it, and not a controlling terminal of this process.
pub(super) fn open() -> std::io::Result<Terminal> {
    let primary = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    // `openpt` has no close-on-exec flag on macOS, so it is set after, on
    // every platform.
    fcntl_setfd(&primary, FdFlags::CLOEXEC)?;
    grantpt(&primary)?;
    unlockpt(&primary)?;
    let name = ptsname(&primary, Vec::new())?;
    let secondary = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let writer = Mutex::new(File::from(primary.try_clone()?));
    // debt: a write larger than the terminal's input queue blocks until the program reads, until `jobs write` can be cancelled mid-write
    let input = Input(Box::new(move |bytes| {
        writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .write_all(bytes)
    }));
    Ok(Terminal {
        reader: File::from(primary),
        secondary,
        input,
    })
}

/// Waits until the output reached end of file or [`FIRST_OUTPUT`] passes on
/// `clock`, whichever is first. A cancel does not cut it short: the wait is
/// bounded.
pub(super) fn wait_first_output(shared: &Shared, clock: &dyn Clock, cancel: &dyn Cancel) {
    let until = clock.now().checked_add(FIRST_OUTPUT);
    loop {
        let (eof, seen) = {
            let inner = lock(&shared.inner);
            (inner.eof, inner.seq)
        };
        if eof || until.is_none_or(|until| clock.now() >= until) {
            return;
        }
        park(clock, shared, cancel, until, false, false, seen);
    }
}

/// The output file's last [`RECEIPT_CUT`] bytes, decoded lossily; empty when
/// the file cannot be read.
pub(super) fn output_so_far(path: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let len = file.metadata().map_or(0, |meta| meta.len());
    let start = len.saturating_sub(RECEIPT_CUT);
    let mut bytes = Vec::new();
    if file.seek(SeekFrom::Start(start)).is_ok() {
        let _read = file.take(len.saturating_sub(start)).read_to_end(&mut bytes);
    }
    // A cut can land inside a character.
    let begin = if start == 0 {
        0
    } else {
        bytes
            .iter()
            .position(|byte| byte & 0b1100_0000 != 0b1000_0000)
            .unwrap_or(bytes.len())
    };
    String::from_utf8_lossy(bytes.get(begin..).unwrap_or_default()).into_owned()
}

#[cfg(test)]
#[path = "tty_tests.rs"]
mod tests;
