//! The pseudo-terminal a `tty` command runs in (`docs/tools.md`, "Terminal
//! (`tty`)"). Fiber keeps the primary side: its reads are the command's
//! output, its writes are `jobs write`. The command gets the secondary side
//! as stdin, stdout and stderr, and as its controlling terminal.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};
use contract::jobs::Input;
use contract::tool::Cancel;
use rustix::event::{PollFd, PollFlags, poll};
use rustix::fs::{Mode, OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

use super::command::park;
use super::output::{Shared, lock};

/// A `tty` call's receipt carries the output that arrives in this long
/// (`docs/tools.md`, "Terminal (`tty`)").
const FIRST_OUTPUT: Duration = Duration::from_millis(250);

/// How long a write waits on the clock before it tries a full terminal
/// queue again. Picked, not measured.
const WRITE_RETRY: Duration = Duration::from_millis(10);

/// One terminal, before the command starts.
pub(super) struct Terminal {
    /// The primary's read side: what the command prints.
    pub reader: Primary,
    /// The secondary, for the command's three standard streams.
    pub secondary: OwnedFd,
    /// Types into the primary.
    pub input: Input,
}

/// The primary, read to the end of the command's output. It is nonblocking,
/// so a write to the same description can give up on a full queue; a read
/// that would block waits in `poll`.
pub(super) struct Primary(File);

impl Read for Primary {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match self.0.read(buf) {
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    let mut fds = [PollFd::new(&self.0, PollFlags::IN)];
                    poll(&mut fds, None)?;
                }
                other => return other,
            }
        }
    }
}

/// Opens a terminal. The primary is close-on-exec, so no command inherits
/// it, and not a controlling terminal of this process.
pub(super) fn open() -> std::io::Result<Terminal> {
    let primary = openpt(OpenptFlags::RDWR.union(OpenptFlags::NOCTTY))?;
    // `openpt` has no close-on-exec flag on macOS, so it is set after, on
    // every platform.
    fcntl_setfd(&primary, FdFlags::CLOEXEC)?;
    // The reader and the writer share the description, so both are
    // nonblocking.
    fcntl_setfl(&primary, fcntl_getfl(&primary)?.union(OFlags::NONBLOCK))?;
    grantpt(&primary)?;
    unlockpt(&primary)?;
    let name = ptsname(&primary, Vec::new())?;
    let secondary = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR.union(OFlags::NOCTTY).union(OFlags::CLOEXEC),
        Mode::empty(),
    )?;
    let writer = Mutex::new(File::from(primary.try_clone()?));
    let input = Input(Box::new(move |bytes, clock, cancel| {
        let mut file = writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        write_chunks(&mut *file, bytes, clock, cancel)
    }));
    Ok(Terminal {
        reader: Primary(File::from(primary)),
        secondary,
        input,
    })
}

/// Wakes a write that is waiting for room: the clock moved or the call was
/// cancelled.
#[derive(Default)]
struct Nudge {
    seq: Mutex<u64>,
    cv: Condvar,
}

impl Wake for Nudge {
    fn wake(&self) {
        let mut seq = self
            .seq
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *seq = seq.wrapping_add(1);
        self.cv.notify_all();
    }
}

impl Nudge {
    fn seen(&self) -> u64 {
        *self
            .seq
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Waits until `until` on `clock` or a wake after `seen`.
    fn park(&self, clock: &dyn Clock, until: Option<Instant>, seen: u64) {
        let mut slot = Some(
            self.seq
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        clock.wait_until(until, &mut |bound| {
            let Some(guard) = slot.take() else {
                return;
            };
            if *guard != seen {
                slot = Some(guard);
                return;
            }
            slot = Some(match bound {
                Some(bound) => {
                    self.cv
                        .wait_timeout(guard, bound)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0
                }
                None => self
                    .cv
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            });
        });
    }
}

/// Writes `bytes` to the nonblocking primary as the queue takes them. A
/// full queue parks on `clock` for [`WRITE_RETRY`], or until a wake; the
/// cancel is checked before every write. Returns how many bytes went in:
/// fewer than given means the cancel fired.
fn write_chunks(
    file: &mut impl Write,
    bytes: &[u8],
    clock: &dyn Clock,
    cancel: &dyn Cancel,
) -> std::io::Result<usize> {
    let nudge = Arc::new(Nudge::default());
    let wake: Arc<dyn Wake> = nudge.clone();
    cancel.subscribe(Arc::downgrade(&wake));
    clock.subscribe(Arc::downgrade(&wake));
    let mut done = 0;
    while done < bytes.len() {
        let seen = nudge.seen();
        if cancel.is_cancelled() {
            break;
        }
        match file.write(bytes.get(done..).unwrap_or_default()) {
            Ok(0) => return Err(ErrorKind::WriteZero.into()),
            Ok(n) => done += n,
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                nudge.park(clock, clock.now().checked_add(WRITE_RETRY), seen);
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(done)
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

/// The output file so far, decoded lossily; empty when it cannot be read.
/// The loop bounds the result (`docs/tools.md`, "Bounded results").
pub(super) fn output_so_far(path: &Path) -> String {
    std::fs::read(path).map_or_else(
        |_| String::new(),
        |bytes| String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[cfg(test)]
#[path = "tty_tests.rs"]
mod tests;
