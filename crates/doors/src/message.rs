//! The `message` driver command (`docs/tools.md`, "Messaging other
//! sessions"): a session message from another session, answered when the
//! loop takes it.

use contract::commands::Message as Args;
use contract::inbox::{Ack, Rejection, SessionMessage};
use contract::{CommandId, ErrorCode};

use crate::client::{Conn, inbox_ack};
use crate::session::admit::Shut;

/// The session is closing and takes no session message.
pub(crate) const CLOSED: &str = "The session is closing and takes no session message.";

/// Runs `message`: reads the sender's latest `session_named` name on the
/// reader thread, admits the message under the admission lock, and answers
/// nothing: the loop's take answers the sender's ack, so `delivered`
/// arrives only after the target's `before_message` hook ran. A closed or
/// ended admission rejects `closing` through the ack.
pub(crate) fn run(conn: &mut Conn, id: CommandId, args: Args) {
    let name = log::session_name(&conn.gate.home, &args.from_session_id);
    let message = SessionMessage {
        from_session_id: args.from_session_id,
        parent_session_id: args.parent_session_id,
        command_id: id.clone(),
        text: args.text,
        name,
    };
    let ack = inbox_ack(conn, id);
    let closing = |ack: Ack, text: &str| {
        ack.0(Err(Rejection {
            code: ErrorCode::Closing,
            message: text.to_owned(),
        }));
    };
    match conn.gate.deliver_message(message, ack) {
        Ok(()) => {}
        Err((Shut::Closed, ack)) => closing(ack, CLOSED),
        Err((Shut::Ended, ack)) => closing(ack, crate::client::ENDED),
    }
}
