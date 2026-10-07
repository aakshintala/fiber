//! A `reply` the session's extensions may answer (`docs/extensions.md`,
//! "Commands and screens"): the door takes one that answers a pending
//! `host.ask`; any other reply goes to the loop as today, so a stale id
//! still gets `stale_request` from the loop.

use contract::CommandId;
use contract::commands::Reply;
use contract::inbox::Delivery;

use crate::client::{Conn, inbox_ack};

/// Routes a `reply` with `id`: builds its ack, offers it to the door, and
/// delivers `Delivery::Reply` for a reply the door hands back, or when no
/// door is set.
pub(crate) fn route(conn: &mut Conn, id: CommandId, reply: Reply) {
    let ack = inbox_ack(conn, id);
    let Some(door) = conn.gate.door() else {
        conn.gate.deliver(Delivery::Reply(reply, ack));
        return;
    };
    // Taken: the holding extension answers the ack itself.
    if let Some((reply, ack)) = door.reply(reply, ack) {
        conn.gate.deliver(Delivery::Reply(reply, ack));
    }
}
