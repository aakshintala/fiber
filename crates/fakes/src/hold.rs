//! One parked holder on a FIFO, cleaned up on every path by the drop below
//! (`docs/testing/running.md`, "Running tests").
//!
//! Every free-FIFO site builds this helper from the shell command string
//! the test already embeds; the helper derives the watchdog pattern from
//! that same string inside, so the caller never passes a pattern.

use std::fs;
use std::path::{Path, PathBuf};

/// A parked holder: the FIFO it blocks on, the read+write release handle
/// keeping the FIFO alive, and the watchdog guard that kills the holder
/// when the test dies.
///
/// Field order is load-bearing: Rust drops struct fields in declaration
/// order, so the FIFO path drops first, then the release handle closes
/// (giving a blocked holder EOF), and only then the guard drops and its
/// kill reaps the holder. Keep this order: dropping the guard before the
/// handle closes reaps a process never asked to leave.
///
/// The handle and the guard sit behind `Option` because the helper
/// implements `Drop`, and a `Drop` type cannot move its fields out:
/// `stand_down` takes them, and the drop skips a stood-down helper.
pub struct Hold {
    path: PathBuf,
    release: Option<fs::File>,
    guard: Option<crate::Watchdog>,
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
    /// neither the hold path nor a per-process `hold #` fragment (one
    /// fragment per test process, shared by its holds) to derive a pattern
    /// from.
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
    /// Cleans up the parked holder on every path, not only when the test
    /// dies (`docs/testing/running.md`, "Running tests"): a holder whose
    /// FIFO was never written would otherwise outlive the test, and the
    /// leak scan fails the run on macOS where the orphan survives.
    ///
    /// Unlink first so a holder that has not opened the FIFO yet fails its
    /// open with ENOENT and exits. A stood-down helper took its handle
    /// and disarmed its guard, so there is nothing to clean up. The fields
    /// then drop in declaration order: the handle closes (giving a blocked
    /// holder EOF), and the guard's kill reaps the holder. Best-effort only:
    /// a drop that panics while unwinding would abort the test process,
    /// so a straggler stays the temp-dir detector's to report, as before.
    fn drop(&mut self) {
        if self.release.is_none() {
            return;
        }
        drop(fs::remove_file(&self.path));
    }
}

/// The watchdog pattern for `command`: the hold path itself when the
/// command embeds it (the absolute-path delegate, search, slash and socket
/// shapes), else the per-process `hold #` fragment (the relative job
/// command, one fragment per test process, so a sibling process can match
/// by pid prefix). The pattern is a substring of the command text, not
/// necessarily of the live process's command line: on Linux bash 5.3.9 execs
/// the last command and drops the trailing comment, so the fragment matches
/// nothing there; whether macOS bash 3.2 keeps it is open.
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
