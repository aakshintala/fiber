//! A session message from another session (`docs/tools.md`, "Messaging other
//! sessions"): answered by the door once it is in the inbox, taken as a
//! steering message during a turn and as a turn's input between turns. It
//! never makes an unattended session attended, and it is never the person's
//! voice.

use std::sync::Arc;
use std::sync::mpsc::TryRecvError;

#[cfg(test)]
use contract::CommandId;
use contract::TurnId;
use contract::inbox::{Delivery, Message, SessionMessage};
use contract::shapes::{ContentPart, Origin, Sender};
use support::admission::Admission;

use super::TurnInput;
use crate::Loop;
use crate::jobs::Queued;
use crate::prompt::{fill, message};

/// The loop thread's hook around the last idle check, in tests only:
/// `Before` runs just before the check takes the admission lock, `After`
/// just after it returned with nothing found and the flag set.
#[cfg(test)]
pub(crate) enum Window {
    Before,
    After,
}

impl Loop {
    /// The admission the door and the loop share, beside `inbox_wake`: once
    /// it closes, no session message enters the inbox.
    pub fn admission(mut self, admission: Arc<Admission>) -> Self {
        self.admission = Some(admission);
        self
    }

    /// A session message taken while the loop is idle: its framed text
    /// joins the turn's input, after `close` too.
    pub(super) fn admit_message_idle(&mut self, message: SessionMessage, input: &mut TurnInput) {
        input.pieces.push(Queued::Steer(
            framed(message),
            crate::before_message::Hooked::default(),
        ));
    }

    /// A session message taken while a turn runs: queued for the next step
    /// boundary, in an after-`close` turn too, in arrival order. It never
    /// marks the session attended (`docs/invocation.md`, "Lifecycle").
    pub(super) fn admit_message_running(
        &mut self,
        message: SessionMessage,
        turn: &TurnId,
    ) -> Result<(), crate::Error> {
        let id = message.command_id.clone();
        let framed = framed(message);
        let emit = super::TurnEmit {
            log: &self.log,
            turn: Some(turn.clone()),
        };
        self.steering
            .admit_message(id, framed.content.clone(), framed.sender.clone(), &emit);
        self.queued.push_back(Queued::Steer(
            framed,
            crate::before_message::Hooked::default(),
        ));
        Ok(())
    }

    /// An idle wait's last `try_recv`, under `close_unless` when an
    /// admission is wired: it always reads, closed or not; `Empty` and
    /// `Disconnected` close admission first.
    pub(crate) fn last_idle_check(&mut self) -> Result<Delivery, TryRecvError> {
        #[cfg(test)]
        if let Some(window) = self.window.as_mut() {
            window(Window::Before);
        }
        let Some(admission) = self.admission.clone() else {
            return self.inbox.try_recv();
        };
        let mut missed: Option<TryRecvError> = None;
        let found = admission.close_unless(|| match self.inbox.try_recv() {
            Ok(delivery) => Some(delivery),
            Err(error) => {
                missed = Some(error);
                None
            }
        });
        #[cfg(test)]
        if found.is_none()
            && let Some(window) = self.window.as_mut()
        {
            window(Window::After);
        }
        match found {
            Some(delivery) => Ok(delivery),
            // `close_unless` runs its closure under the lock, so `missed`
            // is always set when nothing was found.
            None => Err(missed.unwrap_or(TryRecvError::Empty)),
        }
    }

    /// Closes admission when one is wired, before `rewound` is written.
    pub(crate) fn close_admission(&self) {
        if let Some(admission) = &self.admission {
            admission.close();
        }
    }

    /// A session message waits to join a turn: one is queued, held aside,
    /// or in the input the offer step is resolving. While one waits the
    /// session is not idle (`docs/invocation.md`, "Lifecycle").
    pub(crate) fn holds_message(&self) -> bool {
        self.queued
            .iter()
            .any(|piece| matches!(piece, Queued::Steer(message, _) if is_session(message)))
            || self.steering.has_session()
            || self
                .deferred
                .iter()
                .any(|held| matches!(held, Delivery::Message(_)))
            || self.message_waiting
    }
}

/// Whether `pieces` holds a session message.
pub(super) fn has_session_message(pieces: &[Queued]) -> bool {
    pieces
        .iter()
        .any(|piece| matches!(piece, Queued::Steer(message, _) if is_session(message)))
}

/// Whether `message` came from another session.
pub(super) fn is_session(message: &Message) -> bool {
    matches!(message.sender.origin, Origin::Session { .. })
}

/// `message` as the model sees it: framed with the sender's id and latest
/// `session_named` name, one text part. The fill runs once, so a name or
/// text holding a placeholder stays literal.
fn framed(sent: SessionMessage) -> Message {
    let SessionMessage {
        from_session_id,
        command_id,
        text,
        name,
    } = sent;
    let filled = match &name {
        Some(name) => fill(
            message("session-message").trim_end(),
            &[
                ("session_id", from_session_id.0.as_str()),
                ("name", name),
                ("text", text.as_str()),
            ],
        ),
        None => fill(
            message("session-message-unnamed").trim_end(),
            &[
                ("session_id", from_session_id.0.as_str()),
                ("text", text.as_str()),
            ],
        ),
    };
    Message {
        content: vec![ContentPart::Text { text: filled }],
        sender: Sender {
            origin: Origin::Session { from_session_id },
            command_id: Some(command_id),
        },
    }
}

/// Builds a session message for tests: from `from`, named `name`, sent by
/// `command`, saying `text`.
#[cfg(test)]
pub(crate) fn for_test(
    from: &str,
    name: Option<&str>,
    command: &str,
    text: &str,
) -> SessionMessage {
    SessionMessage {
        from_session_id: contract::SessionId(from.to_owned()),
        command_id: CommandId(command.to_owned()),
        text: text.to_owned(),
        name: name.map(str::to_owned),
    }
}

#[cfg(test)]
#[path = "session_message_tests.rs"]
mod tests;
