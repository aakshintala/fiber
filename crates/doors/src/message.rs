//! The `message` driver command (`docs/tools.md`, "Messaging other
//! sessions"): a session message from another session, answered once it is
//! in the target's inbox.

use contract::commands::Message as Args;
use contract::inbox::SessionMessage;
use contract::{CommandId, ErrorCode};

use crate::client::{Conn, accept, reject};
use crate::session::admit::Shut;

/// The session is closing and takes no session message.
pub(crate) const CLOSED: &str = "The session is closing and takes no session message.";

/// Runs `message`: reads the sender's latest `session_named` name on the
/// reader thread, admits the message under the admission lock, and answers
/// at once, without waiting for the loop.
pub(crate) fn run(conn: &mut Conn, id: CommandId, args: Args) {
    let name = log::session_name(&conn.gate.home, &args.from_session_id);
    let message = SessionMessage {
        from_session_id: args.from_session_id,
        command_id: id.clone(),
        text: args.text,
        name,
    };
    match conn.gate.deliver_message(message) {
        Ok(()) => accept(conn, id, None),
        Err(Shut::Closed) => reject(conn, Some(id), ErrorCode::Closing, CLOSED),
        Err(Shut::Ended) => reject(conn, Some(id), ErrorCode::Closing, crate::client::ENDED),
    }
}
