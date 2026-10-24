//! Handing a running command to a job and continuing its drive.

use std::fs::File;
use std::sync::{Arc, Weak};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::Event;
use contract::jobs::Input;
use contract::tool::Cancel;

use super::command::{Finished, MovePolicy, MoveReason};
use super::drive::{LoopEnd, Run, finish, pump, view};
use super::monitor::Feed;
use super::output::{JobStream, Shared, check_start, lock};
use super::spill::Config as SpillConfig;

/// The call's cancel reaches the command through this bridge. The cancel
/// holds it weakly and [`Moved`] holds the only strong reference, so
/// dropping it at a move stops the call's cancel reaching the job.
pub(super) struct CancelBridge(Weak<Shared>);

impl CancelBridge {
    pub(super) fn arm(shared: &Arc<Shared>) -> Arc<Self> {
        Arc::new(Self(Arc::downgrade(shared)))
    }
}

impl Wake for CancelBridge {
    fn wake(&self) {
        if let Some(shared) = self.0.upgrade() {
            shared.wake();
        }
    }
}

/// A command the drive loop has decided to move. The call thread opens the
/// job; the job thread continues from [`Moved::drive_job`].
pub(crate) struct Moved {
    pub reason: MoveReason,
    pub(super) bridge: Option<Arc<CancelBridge>>,
    pub(super) progress: Run,
    /// The job's output file stops at this many bytes. Tests set a small one.
    pub(super) cap: u64,
    /// Where a resumed command spills past 1 MiB, when the shell has one.
    pub(super) spill: Option<SpillConfig>,
}

impl Moved {
    // The drive saves member evidence on the run before the move, so only
    // a test's watchdog still reads the group id.
    #[cfg(test)]
    pub(crate) fn pgid(&self) -> u32 {
        self.progress.pgid
    }

    /// The command's terminal input, once; `None` for a command on pipes.
    pub(crate) fn take_input(&self) -> Option<Input> {
        lock(&self.progress.shared.inner).input.take()
    }

    /// The command's output state, which the call waits on after the move.
    pub(super) fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.progress.shared)
    }

    /// Copies bytes already read into `file`, then points the reader at it.
    /// Both happen under the reader lock, so a chunk is not kept twice or lost.
    /// A spill in progress goes to the job's output file at `path`; the
    /// string names any raw file left behind, for the receipt.
    pub(crate) fn attach_output(&self, file: File, path: &std::path::Path) -> Option<String> {
        lock(&self.progress.shared.inner).attach(file, path, self.cap)
    }

    /// Writes a monitor's held standard error to `file` and points its
    /// reader there; with no file, its standard error is dropped from now
    /// on. Nothing for any other command.
    pub(crate) fn attach_errors(&self, file: Option<File>) {
        if let Some(errors) = lock(&self.progress.shared.inner).errors.as_mut() {
            errors.attach(file, self.cap);
        }
    }

    /// Wakes this command when `cancel` fires. The job's stop uses it.
    pub(crate) fn arm(&self, cancel: &dyn Cancel) {
        let wake: Arc<dyn Wake> = self.progress.shared.clone();
        cancel.subscribe(Arc::downgrade(&wake));
    }

    /// The call's cancel no longer reaches this command.
    pub(crate) fn detach_call_cancel(&mut self) {
        self.bridge = None;
    }

    /// Runs the command to the end with no further move, on the job's
    /// cancel, which [`Moved::arm`] has subscribed.
    pub(crate) fn drive_job(
        mut self,
        clock: &dyn Clock,
        cancel: &dyn Cancel,
        stream: JobStream,
        feed: Option<Feed>,
    ) -> Finished {
        self.progress.job = Some(stream);
        self.progress.feed = feed.map(Box::new);
        self.run(MovePolicy::Stay, clock, cancel, &Silent)
    }

    /// Keeps running in the foreground. Used when the job could not be opened.
    /// Arms the spill, so a resumed command spills past 1 MiB as a
    /// foreground one does.
    pub(crate) fn resume(
        self,
        clock: &dyn Clock,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
    ) -> Finished {
        if self.spill.is_some() {
            let mut inner = lock(&self.progress.shared.inner);
            if inner.spill.is_none() && inner.file.is_none() {
                inner.spill_config = self.spill.clone();
                check_start(&mut inner, 0);
            }
        }
        self.run(MovePolicy::Stay, clock, cancel, emit)
    }

    fn run(
        self,
        policy: MovePolicy,
        clock: &dyn Clock,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
    ) -> Finished {
        let Moved {
            bridge,
            mut progress,
            ..
        } = self;
        let _bridge = bridge;
        match pump(&mut progress, policy, clock, cancel, emit) {
            LoopEnd::Finished(finished) => finished,
            // `Stay` does not move. Finishing here keeps a bug from spinning.
            LoopEnd::Move(_) => {
                let view = view(&progress.shared, cancel);
                // The group leaves the list only once this run saw it
                // empty; a group never seen empty stays until the shared
                // kill prunes it.
                if progress.seen_empty
                    && let Some(listing) = progress.listing.take()
                {
                    support::group::live().unlist(*listing);
                }
                finish(
                    &progress.shared,
                    progress.stop,
                    progress.sent_signal,
                    progress.seen_empty,
                    view.eof,
                    emit,
                    &mut progress.stream_carry,
                )
            }
        }
    }
}

struct Silent;

impl Emit for Silent {
    fn emit(&self, _event: &Event) {}
}
