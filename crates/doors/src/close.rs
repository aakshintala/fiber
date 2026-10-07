//! The `close` driver command (`docs/invocation.md`, "Driver commands"): an
//! ordinary close finishes what is running, while one with `now` starts the
//! shutdown instead (`docs/invocation.md`, "Shutdown").

use contract::CommandId;
use contract::inbox::Delivery;

use crate::client::{Conn, accept, inbox_ack};

/// Runs `close`: with `now` and a wired shutdown, the answer is queued
/// before the shutdown starts, so no shutdown, however fast, can drop it,
/// and nothing reaches the inbox, which keeps a pending approval pending
/// and starts no ending-notice turn. Without either, an ordinary close.
pub(crate) fn run(conn: &mut Conn, id: CommandId, now: bool) {
    if now {
        // Cloned out of the gate's lock, and called with no gate lock held.
        if let Some(start) = conn.gate.close_now() {
            accept(conn, id, None);
            start();
            return;
        }
    }
    let ack = inbox_ack(conn, id);
    conn.gate.deliver(Delivery::Close(ack));
}
