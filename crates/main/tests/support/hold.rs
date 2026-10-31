//! One parked holder on the test workspace's `hold` FIFO, with the
//! watchdog that reaps it when the test dies (`docs/testing.md`,
//! "Running tests").
//!
//! Every free-FIFO site builds this helper from the shell command string
//! the test already embeds; the helper derives the watchdog pattern from
//! that same string inside, so the caller never passes a pattern.

use std::fs;
use std::path::{Path, PathBuf};

use super::{Deadline, Setup};

/// A parked holder: the FIFO it blocks on, the read+write release handle
/// keeping the FIFO alive, and the watchdog guard that kills the holder
/// when the test dies.
///
/// Field order is load-bearing: Rust drops struct fields in declaration
/// order, so the FIFO path drops first, then the release handle closes
/// (giving a blocked holder EOF), and only then the guard drops and its
/// kill reaps the holder. Keep this order: dropping the guard before the
/// handle closes reaps a process never asked to leave.
pub(crate) struct Hold {
    path: PathBuf,
    release: fs::File,
    guard: fakes::Watchdog,
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
        let guard = fakes::Watchdog::matching(&derive_pattern(command, &path));
        let release = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        Self {
            path,
            release,
            guard,
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
        &mut self.release
    }

    /// The holder already exited on the success path: disarm the watchdog
    /// (it exits without signalling) instead of dropping into its kill.
    /// Takes the cleanup-phase deadline, like every teardown wait.
    pub(crate) fn stand_down(self, deadline: Deadline) {
        let Self {
            path: _,
            release,
            guard,
        } = self;
        drop(release);
        guard.stand_down(deadline);
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
