//! One parked holder on the test workspace's `hold` FIFO, reaped on
//! every path by the drop below (`docs/testing/running.md`, "Running
//! tests").
//!
//! Every free-FIFO site builds this helper from the shell command string
//! the test already embeds; the helper derives the watchdog pattern from
//! that same string inside, so the caller never passes a pattern.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Deadline, Setup};

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
pub(crate) struct Hold {
    path: PathBuf,
    release: Option<fs::File>,
    guard: Option<fakes::Watchdog>,
    pattern: String,
}

impl Hold {
    /// Parks a holder for `command` on `<workspace>/hold`: makes the FIFO,
    /// arms the watchdog on the pattern derived from `command`, then opens
    /// the release handle read+write so a `head` holder blocks in its read
    /// instead of its open. Each test owns a fresh `TempDir`, so the FIFO
    /// path never preexists (`fifo` panics on an existing path).
    ///
    /// # Panics
    ///
    /// When the FIFO cannot be made or opened, or when `command` embeds
    /// neither the hold path nor a pid-scoped `hold #` fragment to derive
    /// a pattern from.
    pub(crate) fn park(setup: &Setup, command: &str) -> Self {
        let path = setup.workspace().join("hold");
        fakes::fifo(&path);
        let pattern = derive_pattern(command, &path);
        let guard = fakes::Watchdog::matching(&pattern);
        let release = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        Self {
            path,
            release: Some(release),
            guard: Some(guard),
            pattern,
        }
    }

    /// The FIFO's path: the socket site writes its rendezvous line against
    /// it, and the job site names its row by it.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The release handle: explicit test writes (the item finish path)
    /// go through it. Dropping the helper closes it.
    pub(crate) fn release(&mut self) -> &mut fs::File {
        self.release.as_mut().unwrap()
    }

    /// The holder already exited on the success path: take the handle and
    /// disarm the watchdog (it exits without signalling) instead of
    /// dropping into its kill. Takes the cleanup-phase deadline, like
    /// every teardown wait.
    pub(crate) fn stand_down(mut self, deadline: Deadline) {
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
    /// one-shot kill below can miss; then kill the hold pattern so a
    /// parked `head` dies with its group, and re-kill once when the
    /// pattern is still listed. A stood-down helper took its handle and
    /// disarmed its guard, so there is nothing to reap. Best-effort only:
    /// a drop that panics while unwinding would abort the test process,
    /// so a straggler stays the temp-dir detector's to report, as before.
    fn drop(&mut self) {
        if self.release.is_none() {
            return;
        }
        drop(fs::remove_file(&self.path));
        // An error means no holder matched: nothing to reap.
        if fakes::kill_matching(self.pattern.as_str()).is_err() {
            return;
        }
        if !fakes::matching_exits(
            self.pattern.as_str(),
            fakes::Deadline::after(std::time::Duration::from_millis(100)),
        ) {
            drop(fakes::kill_matching(self.pattern.as_str()));
        }
    }
}

/// The watchdog pattern for `command`: the hold path itself when the
/// command embeds it (the absolute-path delegate, search, slash and socket
/// shapes), else the pid-scoped `hold #` fragment (the relative job
/// command). Either way the pattern is a substring of the spawned holder's
/// own command line, and unique to this test.
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
