//! What a signal does to a `fiber ask` session process (`docs/invocation.md`,
//! "Shutdown"): the callbacks the signals thread runs, wired to the turn's
//! cancel, the session's jobs, the door side, a switch's credential read,
//! and every listed process group, MCP servers' included. A `close` with
//! `now` takes the same path with exit code 0, through the hook wired here.

use std::io;
use std::sync::Arc;

use contract::clock::Clock;
use contract::jobs::Jobs;
use doors::{Session, Signals};
use r#loop::TurnCancel;

/// Arms the signals just before the first child process starts: from now a
/// signal is recorded, every server still starting is told to stop, the
/// session's reads are cancelled, killing every credential command they
/// started, and at the bound every listed process group, MCP servers'
/// included, still alive is killed.
pub(crate) fn arm(signals: &Signals, reads: &Arc<crate::switch::Reads>) {
    let record_reads = Arc::clone(reads);
    let bound_reads = Arc::clone(reads);
    signals.arm(
        Box::new(move || {
            mcp::stop_every_start();
            record_reads.cancel();
        }),
        Box::new(move || {
            bound_reads.cancel();
            support::group::kill_every_group();
        }),
    );
}
/// Installs the signals with a writer for the pid-named diagnostic file:
/// `fiber ask`, `fiber session` and the extension-case process all route
/// their bound line through [`crate::startup_log::diag`]. A home that
/// cannot be resolved installs with no writer, so no bound line is
/// written, and shutdown still runs.
pub(crate) fn install(clock: Arc<dyn Clock>) -> io::Result<Arc<Signals>> {
    Signals::install(clock.clone(), crate::startup_log::diag(clock))
}

/// Starts the session just before its first line: the code of a signal
/// that came while armed, or `None` with the shutdown wired, still armed
/// until [`Signals::commit`] decides recorded versus started. A shutdown
/// cancels the turn for good, ends a switch's credential read and kills its
/// command, wakes the loop, and stops every job; a second SIGTERM or SIGINT
/// kills every command group at once. With `None`, a
/// `close` with `now` starts the same shutdown with exit code 0.
pub(crate) fn start(
    signals: &Arc<Signals>,
    cancel: &Arc<TurnCancel>,
    session: &Session,
    jobs: Arc<dyn Jobs>,
    reads: Arc<crate::switch::Reads>,
) -> Option<i32> {
    // The session exists now, so the bound line goes to its file: before
    // any `attach` it would land in the pid-named one.
    signals.attach(session.id().clone());
    let turn = Arc::clone(cancel);
    let stopper = session.stopper();
    let started = signals.start(
        Box::new(move |code| {
            // A shutdown stops every running extension callback and its
            // `host.exec` group as it stops a tool call's: SIGTERM now,
            // SIGKILL 800 ms later for a group still alive
            // (`docs/invocation.md`, "Shutdown").
            shutdown_steps(
                code,
                |code| turn.shutdown(code),
                || reads.cancel(),
                extensions::stop_every_callback,
                || {
                    stopper();
                    for id in jobs.running() {
                        jobs.stop(&id);
                    }
                },
            );
        }),
        Box::new(|| {
            support::group::kill_every_group();
        }),
    );
    if started.is_some() {
        return started;
    }
    // The `Weak` breaks the cycle the `on_signal` closure already holds
    // through the stopper: `main` keeps the `Arc` for the whole process.
    let weak = Arc::downgrade(signals);
    session.close_now(Arc::new(move || {
        if let Some(signals) = weak.upgrade() {
            signals.close_now();
        }
    }));
    None
}

fn shutdown_steps(
    code: i32,
    publish: impl FnOnce(i32),
    cancel_reads: impl FnOnce(),
    stop_callbacks: impl FnOnce(),
    rest: impl FnOnce(),
) {
    // The code first: the loop reads it once the stopper wakes it.
    publish(code);
    stop_callbacks();
    cancel_reads();
    rest();
}

#[cfg(test)]
#[path = "shutdown_tests.rs"]
mod tests;
