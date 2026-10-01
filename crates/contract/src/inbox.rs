//! What a driver, an extension or another session sends to a session's
//! loop (`docs/architecture.md`, "One inbox"). The loop owns the receiving
//! end; everyone else holds a sender, so this type lives here and no sender
//! depends on `loop`.

use crate::shapes::{ContentPart, Sender};

/// A message for the loop. While the loop is idle it starts a turn; while a
/// turn runs it steers that turn (`docs/loop.md`, "Starting a turn" and "One
/// step").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The message.
    pub content: Vec<ContentPart>,
    /// Where it came from.
    pub sender: Sender,
}
