//! Owns the registry of running driver shells and their shutdown cancellation.

use std::sync::{Arc, PoisonError};

use super::{Gate, lock};

/// Driver shells running now, and whether shutdown has begun, which cancels a
/// new one as it registers. Both sit under this lock, so a shell that registers
/// after `close` cannot miss the snapshot.
pub(super) struct RunningShells {
    pub(super) stopped: bool,
    pub(super) running: Vec<Arc<crate::shell::ShellCancel>>,
}

fn cancel_each(shells: &[Arc<crate::shell::ShellCancel>]) {
    for shell in shells {
        shell.cancel();
    }
}

/// What [`Gate::stop_running`] found running.
pub(crate) struct Stopped {
    pub(crate) turn: bool,
    pub(crate) shell: bool,
}

impl Gate {
    /// Waits until no driver shell is registered: each has answered.
    pub(super) fn wait_shells(&self) {
        let mut shells = lock(&self.shells);
        while !shells.running.is_empty() {
            #[cfg(test)]
            self.note(super::tests::Probe::ShellsWaiting);
            shells = self
                .shell_ended
                .wait(shells)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Stops a running turn and every driver shell. The shells are
    /// cancelled after the registry lock is released, so a shell's return
    /// can remove its entry without waiting on this call.
    pub(crate) fn stop_running(&self) -> super::Stopped {
        let turn = lock(&self.cancel).as_ref().is_some_and(|cancel| cancel());
        let shells = lock(&self.shells).running.clone();
        cancel_each(&shells);
        Stopped {
            turn,
            shell: !shells.is_empty(),
        }
    }

    /// Marks the gate so a shell that registers later is cancelled at once,
    /// and cancels the shells already running. Closing the socket does not
    /// stop a tool blocked in `run`.
    pub(super) fn cancel_shells(&self) {
        let running = {
            let mut shells = lock(&self.shells);
            shells.stopped = true;
            shells.running.clone()
        };
        cancel_each(&running);
    }

    /// Registers a driver shell's cancel. After `close` or the stopper it
    /// is registered too, so they wait for its thread, and cancelled at once.
    pub(crate) fn track_shell(&self, cancel: Arc<crate::shell::ShellCancel>) {
        let stopped = {
            let mut shells = lock(&self.shells);
            shells.running.push(Arc::clone(&cancel));
            shells.stopped
        };
        // After the lock: `cancel` wakes the tool, which must not need this lock.
        if stopped {
            cancel.cancel();
        }
    }

    pub(crate) fn untrack_shell(&self, cancel: &Arc<crate::shell::ShellCancel>) {
        lock(&self.shells)
            .running
            .retain(|tracked| !Arc::ptr_eq(tracked, cancel));
        self.shell_ended.notify_all();
    }
}
