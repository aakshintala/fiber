//! What a signal does to a `fiber ask` session process (`docs/invocation.md`,
//! "Shutdown"): the callbacks the signals thread runs, wired to the turn's
//! cancel, the session's jobs, the door side, every command's process group
//! and every MCP server.

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
            mcp::kill_every_server();
        }),
    );
}

/// Starts the session just before its first line: the code of a signal
/// that came while armed, or `None` with the shutdown wired. A shutdown
/// cancels the turn for good, wakes the loop, and stops every job; a second
/// SIGTERM or SIGINT kills every command group at once.
pub(crate) fn start(
    signals: &Signals,
    cancel: &Arc<TurnCancel>,
    session: &Session,
    jobs: Arc<dyn Jobs>,
) -> Option<i32> {
    let turn = Arc::clone(cancel);
    let stopper = session.stopper();
    signals.start(
        Box::new(move |code| {
            // The code first: the loop reads it once the stopper wakes it.
            turn.shutdown(code);
            stopper();
            for id in jobs.running() {
                jobs.stop(&id);
            }
        }),
        Box::new(tools::kill_every_group),
    )
}
