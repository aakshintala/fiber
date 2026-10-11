//! Owns the registry of running driver shells and their shutdown cancellation.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// Driver shells running now, and whether shutdown has begun, which cancels a
/// new one as it registers.
pub(crate) struct Shells {
    running: Mutex<RunningShells>,
    ended: Condvar,
}

/// Driver shells running now, and whether shutdown has begun, which cancels a
/// new one as it registers. Both sit under this lock, so a shell that registers
/// after `close` cannot miss the snapshot.
struct RunningShells {
    stopped: bool,
    running: Vec<Arc<crate::shell::ShellCancel>>,
}

fn cancel_each(shells: &[Arc<crate::shell::ShellCancel>]) {
    for shell in shells {
        shell.cancel();
    }
}

/// What [`crate::session::Gate::stop_running`] found running.
pub(crate) struct Stopped {
    pub(crate) turn: bool,
    pub(crate) shell: bool,
}

impl Shells {
    pub(crate) fn new() -> Self {
        Self {
            running: Mutex::new(RunningShells {
                stopped: false,
                running: Vec::new(),
            }),
            ended: Condvar::new(),
        }
    }

    /// Registers a driver shell's cancel. After `close` or the stopper it
    /// is registered too, so they wait for its thread, and cancelled at once.
    pub(crate) fn track(&self, cancel: Arc<crate::shell::ShellCancel>) {
        let stopped = {
            let mut running = lock(&self.running);
            running.running.push(Arc::clone(&cancel));
            running.stopped
        };
        // After the lock: `cancel` wakes the tool, which must not need this lock.
        if stopped {
            cancel.cancel();
        }
    }

    /// Unregisters a driver shell's cancel and wakes whoever waits for none
    /// to be registered.
    pub(crate) fn untrack(&self, cancel: &Arc<crate::shell::ShellCancel>) {
        lock(&self.running)
            .running
            .retain(|tracked| !Arc::ptr_eq(tracked, cancel));
        self.ended.notify_all();
    }

    /// Cancels every driver shell and says whether any ran. The shells are
    /// cancelled after the registry lock is released, so a shell's return
    /// can remove its entry without waiting on this call.
    pub(super) fn stop_all(&self) -> bool {
        let running = lock(&self.running).running.clone();
        cancel_each(&running);
        !running.is_empty()
    }

    /// Marks the registry so a shell that registers later is cancelled at once,
    /// and cancels the shells already running.
    pub(super) fn seal_and_cancel(&self) {
        let running = {
            let mut running = lock(&self.running);
            running.stopped = true;
            running.running.clone()
        };
        cancel_each(&running);
    }

    /// Waits until no driver shell is registered: each has answered. Calls
    /// `on_wait` before each park.
    pub(super) fn wait(&self, on_wait: &dyn Fn()) {
        let mut running = lock(&self.running);
        while !running.running.is_empty() {
            on_wait();
            running = self
                .ended
                .wait(running)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    pub(crate) fn sealed(&self) -> bool {
        lock(&self.running).stopped
    }

    #[cfg(test)]
    pub(crate) fn running_len(&self) -> usize {
        lock(&self.running).running.len()
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
