//! One parked holder on a FIFO, reaped on every path by the drop below
//! (`docs/testing/running.md`, "Running tests").
//!
//! Every free-FIFO site builds this helper from the shell command string
//! the test already embeds; the helper derives the watchdog pattern from
//! that same string inside, so the caller never passes a pattern.

use std::fs;
use std::path::{Path, PathBuf};

/// A parked holder: the FIFO it blocks on, the read+write release handle
/// keeping the FIFO alive, the watchdog guard that kills the holder when
/// the test dies, and the watchdog pattern the drop reaps on every path.
///
/// Field order is load-bearing: Rust drops struct fields in declaration
/// order, so the FIFO path drops first, then the release handle closes
/// (giving a blocked holder EOF), and only then the guard drops and its
/// kill reaps the holder. Keep this order: dropping the guard before the
/// handle closes reaps a process never asked to leave. The pattern drops
/// last; it is a `String` and drops no resource.
///
/// The handle and the guard sit behind `Option` because the helper
/// implements `Drop`, and a `Drop` type cannot move its fields out:
/// `stand_down` takes them, and the drop skips a stood-down helper.
pub struct Hold {
    path: PathBuf,
    release: Option<fs::File>,
    guard: Option<crate::Watchdog>,
    pattern: String,
}

impl Hold {
    /// Parks a holder for `command` on `path`: makes the FIFO, derives the
    /// pattern from `command`, then opens the hold. Each test owns a fresh
    /// `TempDir`, so the FIFO path never preexists (`fifo` panics on an
    /// existing path).
    ///
    /// # Panics
    ///
    /// When the FIFO cannot be made or opened, or when `command` embeds
    /// neither the hold path nor a pid-scoped `hold #` fragment to derive
    /// a pattern from.
    #[allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test helper; a failure is the test's"
    )]
    #[track_caller]
    pub fn park(path: &Path, command: &str) -> Self {
        crate::fifo(path);
        let pattern = derive_pattern(command, path);
        Self::on(path, &pattern)
    }

    /// Opens the hold on the FIFO at `path`, which already exists. Arms
    /// the watchdog on `pattern`, then opens the release end read+write,
    /// so the open never blocks and a holder's `read` parks. The release
    /// end stays open until the hold is dropped or stood down: a release
    /// written before the reader opens is buffered in the FIFO and read
    /// when the reader opens it (`docs/testing.md:297-299`).
    ///
    /// # Panics
    ///
    /// When `pattern` is empty, or when the release end cannot be opened.
    #[allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test helper; a failure is the test's"
    )]
    #[track_caller]
    pub fn on(path: &Path, pattern: &str) -> Self {
        let guard = crate::Watchdog::matching(pattern);
        let release = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap();
        Self {
            path: path.to_path_buf(),
            release: Some(release),
            guard: Some(guard),
            pattern: pattern.to_owned(),
        }
    }

    /// The FIFO's path: the socket site writes its rendezvous line against
    /// it, and the job site names its row by it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The release handle: explicit test writes (the item finish path)
    /// go through it. Dropping the helper closes it.
    #[allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test helper; a failure is the test's"
    )]
    pub fn release(&mut self) -> &mut fs::File {
        self.release.as_mut().unwrap()
    }

    /// The holder already exited on the success path: take the handle and
    /// disarm the watchdog (it exits without signalling) instead of
    /// dropping into its kill. Takes the cleanup-phase deadline, like
    /// every teardown wait.
    #[allow(
        clippy::panic,
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test helper; a failure is the test's"
    )]
    pub fn stand_down(mut self, deadline: crate::deadline::Deadline) {
        drop(self.release.take());
        self.guard.take().unwrap().stand_down(deadline);
    }
}

impl Drop for Hold {
    /// Reaps the parked holder on every path, not only when the test dies
    /// (`docs/testing/running.md`, "Running tests"): a holder whose FIFO
    /// was never written would otherwise outlive the test, and the leak
    /// scan fails the run on macOS where the orphan survives.
    ///
    /// Unlink first so a holder that has not opened the FIFO yet fails its
    /// open with ENOENT and exits, closing the delayed-start window the
    /// one-shot kill below can miss. A stood-down helper took its handle
    /// and disarmed its guard, so there is nothing to reap. Best-effort only:
    /// a drop that panics while unwinding would abort the test process,
    /// so a straggler stays the temp-dir detector's to report, as before.
    fn drop(&mut self) {
        if self.release.is_none() {
            return;
        }
        drop(fs::remove_file(&self.path));
        reap(self.pattern.as_str());
    }
}

/// Kills the parked holder, and re-kills once when the pattern is still
/// listed after the first kill's wait.
// debt: no test separates this kill from the watchdog's async kill of the
// same pattern on drop, so the empty-body mutant survives; pin the sync
// kill alone once a test reaches `reap` without the watchdog armed.
fn reap(pattern: &str) {
    // An error means no holder matched: nothing to reap.
    if crate::kill_matching(pattern).is_err() {
        return;
    }
    if !crate::matching_exits(
        pattern,
        crate::Deadline::after(std::time::Duration::from_millis(100)),
    ) {
        drop(crate::kill_matching(pattern));
    }
}

/// The watchdog pattern for `command`: the hold path itself when the
/// command embeds it (the absolute-path delegate, search, slash and socket
/// shapes), else the pid-scoped `hold #` fragment (the relative job
/// command). Either way the pattern is a substring of the spawned holder's
/// own command line, and unique to this test.
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test helper; a failure is the test's"
)]
fn derive_pattern(command: &str, hold: &Path) -> String {
    let hold = hold
        .to_str()
        .unwrap_or_else(|| panic!("the hold path is UTF-8: {}", hold.display()));
    if command.contains(hold) {
        return hold.to_owned();
    }
    if let Some(start) = command.find("hold #") {
        return command[start..].trim_end().to_owned();
    }
    panic!(
        "the hold helper derives its watchdog pattern from the command; it holds no pattern for {command:?}"
    );
}

#[cfg(test)]
#[path = "hold_tests.rs"]
mod tests;
