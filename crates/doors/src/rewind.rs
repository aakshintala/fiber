//! The `rewind` driver command (`docs/invocation.md`, "Driver commands"):
//! delivers it to the loop's inbox, which answers whether a new session
//! continues this one from the point it names.
use contract::CommandId;
use contract::commands::RewindArgs;
use contract::inbox::Delivery;

use crate::client::{Conn, inbox_ack};

/// Runs `rewind`: delivers it with the inbox acknowledgement.
pub(crate) fn run(conn: &mut Conn, id: CommandId, args: RewindArgs) {
    let ack = inbox_ack(conn, id);
    conn.gate.deliver(Delivery::Rewind(args, ack));
}
