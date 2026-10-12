//! The cancel guard for the entry points that run a `command` credential
//! source outside a session (`docs/configuration.md`, "Secrets"): `fiber
//! models`, `fiber refresh-model-lists` and the terminal's model picker
//! each cancel what their credential commands left, on a normal return and
//! on a signal. A taken signal cancels, settles in-flight spawns and exits
//! with 128 plus its number, never with the default action.

use std::ffi::c_int;
use std::io;
use std::sync::Arc;
use std::thread;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::shapes::Failure;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};

use crate::switch::Reads;

/// The signals `fiber models` and `fiber refresh-model-lists` cancel on:
/// the foreground SIGINT with the two every Fiber process exits on. Never
/// SIGQUIT: it keeps its default of a core dump.
pub(crate) const FOREGROUND: [c_int; 3] = [SIGINT, SIGTERM, SIGHUP];

/// The signals the terminal cancels on: SIGTERM and SIGHUP. Its SIGINT and
/// SIGQUIT belong to the TUI's `catch_interrupts`.
pub(crate) const TERMINAL: [c_int; 2] = [SIGTERM, SIGHUP];

/// Cancels the entry point's [`Reads`] on a registered signal, settles an
/// in-flight spawn, then exits with 128 plus the signal's number. `Drop`
/// closes the registration and joins the watcher, so a taken signal always
/// exits before the entry point returns. Sends no signal itself.
pub(crate) struct CancelOnSignal {
    thread: Option<thread::JoinHandle<()>>,
    handle: signal_hook::iterator::Handle,
}

impl CancelOnSignal {
    /// Watches `signals` for `reads`: on the first, cancel, settle, then
    /// exit with 128 plus its number.
    pub(crate) fn new(
        reads: Arc<Reads>,
        clock: Arc<dyn Clock>,
        signals: &[c_int],
    ) -> io::Result<Self> {
        Self::arm(
            reads,
            clock,
            signals,
            Box::new(|signal| std::process::exit(doors::signal_code(signal))),
        )
    }

    /// Watches `signals` for `reads`: on each, cancel, settle, then
    /// `finish`. Only the final action is injected: the cancel and the
    /// settle are production code on every tested path.
    fn arm(
        reads: Arc<Reads>,
        clock: Arc<dyn Clock>,
        signals: &[c_int],
        finish: Box<dyn Fn(c_int) + Send>,
    ) -> io::Result<Self> {
        let mut registered = signal_hook::iterator::Signals::new(signals)?;
        let handle = registered.handle();
        let watcher = thread::Builder::new()
            .name("fiber-cancel-on-signal".to_owned())
            // Chosen small; the watcher calls only cancel, settle and exit,
            // so it does no work that needs the default stack.
            .stack_size(65_536)
            .spawn(move || {
                for signal in registered.forever() {
                    // The cancel wakes a read in flight on the main thread;
                    // the settle waits out a spawn that raced it; only then
                    // does the process end.
                    reads.cancel();
                    reads.settle(&*clock, doors::SHUTDOWN_BOUND);
                    finish(signal);
                }
            });
        match watcher {
            Ok(thread) => Ok(Self {
                thread: Some(thread),
                handle,
            }),
            Err(error) => {
                // A thread that never started watches nothing: close the
                // registration and fail before any command starts.
                handle.close();
                Err(error)
            }
        }
    }
}

impl Drop for CancelOnSignal {
    /// Closes the registration, which ends the watcher's wait, then joins
    /// it: a taken signal's exit lands before the entry point returns.
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _joined = thread.join();
        }
    }
}

/// Cancels the entry point's [`Reads`] and settles in-flight spawns when
/// the entry point returns, whatever the result: a normal return kills
/// what the process's credential commands left in their groups, the same
/// as a signal does. Cancelling twice is harmless.
pub(crate) struct CancelAndSettle(pub(crate) Arc<Reads>, pub(crate) Arc<dyn Clock>);

impl Drop for CancelAndSettle {
    fn drop(&mut self) {
        self.0.cancel();
        self.0.settle(&*self.1, doors::SHUTDOWN_BOUND);
    }
}

/// The signal registration an entry point runs: builds its
/// [`CancelOnSignal`]. Takes owned [`Arc`]s so the entry points pass
/// [`CancelOnSignal::new`] directly; tests pass a refusal. A function
/// pointer so `guarded` stays injectable.
pub(crate) type Register = fn(Arc<Reads>, Arc<dyn Clock>, &[c_int]) -> io::Result<CancelOnSignal>;

/// Builds an entry point's guards in drop order: the plain cancel, then
/// the signal watcher, so a taken signal's watcher finishes before the
/// plain cancel runs. A refused registration fails closed before any
/// runner exists: the entry point returns before any credential command
/// starts. Builds the `signals:` failure here; callers add no text.
pub(crate) fn guarded(
    reads: &Arc<Reads>,
    clock: &Arc<dyn Clock>,
    signals: &[c_int],
    register: Register,
) -> Result<(CancelAndSettle, CancelOnSignal), Failure> {
    match register(Arc::clone(reads), Arc::clone(clock), signals) {
        Ok(watcher) => Ok((
            CancelAndSettle(Arc::clone(reads), Arc::clone(clock)),
            watcher,
        )),
        Err(error) => Err(doors::failure(
            ErrorCode::IoFailed,
            format!("signals: {error}"),
        )),
    }
}

#[cfg(test)]
#[path = "read_signals_tests.rs"]
mod tests;
