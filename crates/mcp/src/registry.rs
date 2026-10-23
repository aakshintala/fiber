//! Stopping every MCP server still starting (`docs/mcp.md`, "Starting
//! servers"). Live servers are listed in `support::group`'s process-wide
//! list instead, signalled as groups;
//! [`stop_every_start`] is what stays here.

use std::sync::LazyLock;

use support::cancel::Token;

/// Stops every server still starting, and every start after it: the flag is
/// sticky for the life of the process. It sets the flag and wakes the parked
/// handshakes; the stop and reap run on each start's own thread. Idempotent.
pub fn stop_every_start() {
    STOPPING.cancel();
}

/// The handshake's cancel, fired by [`stop_every_start`]: sticky for the
/// life of the process, so a start after the stop sees it cancelled.
pub(crate) static STOPPING: LazyLock<Token> = LazyLock::new(Token::new);

/// Runs while a signaller holds the child that keeps its group unreaped,
/// just before the SIGTERM: the test pauses there to force a reap against
/// it. No outside seam can pause a signaller holding that child, so the
/// pause point stays as this race's only way to hold the SIGTERM back.
#[cfg(test)]
pub(crate) fn before_signal() {
    crate::server::tests::before_signal();
}

#[cfg(not(test))]
pub(crate) fn before_signal() {}

/// Runs as a reap is about to take `which` lock (`child` or `live`): the
/// test waits there to know the reap contends. A reap parked on either
/// lock is invisible from outside, so the pause point stays as this race's
/// only way to see the reap wait.
#[cfg(test)]
pub(crate) fn before_lock(which: &'static str) {
    crate::server::tests::before_lock(which);
}

#[cfg(not(test))]
pub(crate) fn before_lock(_which: &'static str) {}

pub(crate) use support::lock;
