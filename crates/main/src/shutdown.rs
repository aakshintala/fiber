//! What a signal does to a `fiber ask` session process (`docs/invocation.md`,
//! "Shutdown"): the callbacks the signals thread runs, wired to the turn's
//! cancel, the session's jobs, the door side, a switch's credential read,
//! every command's process group and every MCP server. A `close` with `now`
//! takes the same path with exit code 0, through the hook wired here.

use std::sync::Arc;

use contract::jobs::Jobs;
use doors::{Session, Signals};
use r#loop::TurnCancel;

/// Arms the signals just before the first child process starts: from now a
/// signal is recorded, every server still starting is told to stop, and at
/// the bound every command group and MCP server still alive is killed.
pub(crate) fn arm(signals: &Signals) {
    signals.arm(
        Box::new(mcp::stop_every_start),
        Box::new(|| {
            tools::kill_every_group();
            extensions::kill_every_group();
            mcp::kill_every_server();
        }),
    );
}

/// Arms the signals just before the session's worktree is created: from
/// now a signal is recorded, every server still starting is told to stop,
/// and the worktree's own reads are cancelled, killing its `git` and the
/// hook together. At the bound the reads go first, then every command
/// group and MCP server still alive is killed.
pub(crate) fn arm_isolating(signals: &Signals, reads: &Arc<crate::switch::Reads>) {
    let record_reads = Arc::clone(reads);
    let bound_reads = Arc::clone(reads);
    signals.arm(
        Box::new(move || {
            mcp::stop_every_start();
            record_reads.cancel();
        }),
        Box::new(move || {
            bound_reads.cancel();
            tools::kill_every_group();
            extensions::kill_every_group();
            mcp::kill_every_server();
        }),
    );
}
/// Starts the session just before its first line: the code of a signal
/// that came while armed, or `None` with the shutdown wired. A shutdown
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
    let turn = Arc::clone(cancel);
    let stopper = session.stopper();
    let started = signals.start(
        Box::new(move |code| {
            // The code first: the loop reads it once the stopper wakes it.
            turn.shutdown(code);
            reads.cancel();
            stopper();
            for id in jobs.running() {
                jobs.stop(&id);
            }
        }),
        Box::new(|| {
            tools::kill_every_group();
            extensions::kill_every_group();
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
