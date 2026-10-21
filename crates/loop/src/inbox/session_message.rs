//! A session message from another session (`docs/tools.md`, "Messaging other
//! sessions"): the `before_message` hook runs on its raw text before the
//! sender is answered, so a refusal or a blocking failure rejects the
//! sender and nothing is logged; a passed or replaced text is taken as a
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
use super::accept;
use crate::Loop;
use crate::before_message::Hooked;
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

    /// A session message taken while the loop is idle: the hook runs on
    /// the raw text before it answers, so a refusal or a blocking failure
    /// rejects the sender and nothing joins the turn's input; a passed or
    /// replaced text is framed and joins the input with its verdict, after
    /// `close` too. Answered exactly once, on every path.
    pub(super) fn admit_message_idle(
        &mut self,
        sent: SessionMessage,
        ack: contract::inbox::Ack,
        input: &mut TurnInput,
    ) -> Result<(), crate::Error> {
        let Some((passed, hooked, ack)) = self.hooked_message(&sent, ack, None)? else {
            return Ok(());
        };
        input.pieces.push(Queued::Steer(passed, hooked));
        accept(ack);
        Ok(())
    }

    /// A session message taken while a turn runs: the hook runs on the
    /// raw text before it answers, and the passed or replaced text queues
    /// for the next step boundary with its verdict, in an after-`close`
    /// turn too, in arrival order. The verdict's staged metadata rides the
    /// shared queue entry, so the staged claim keeps the queued holder and
    /// `changed_by`, context and state reach the apply. It never marks the
    /// session attended (`docs/invocation.md`, "Lifecycle"). Answered
    /// exactly once, on every path.
    pub(super) fn admit_message_running(
        &mut self,
        sent: SessionMessage,
        ack: contract::inbox::Ack,
        turn: &TurnId,
    ) -> Result<(), crate::Error> {
        let id = sent.command_id.clone();
        let Some((passed, hooked, ack)) = self.hooked_message(&sent, ack, Some(turn))? else {
            return Ok(());
        };
        let staged = contract::steering::Staged {
            state: hooked.state.clone(),
            changed_by: hooked.changed_by.clone(),
            context: hooked.context.clone(),
        };
        let emit = super::TurnEmit {
            log: &self.log,
            turn: Some(turn.clone()),
        };
        // The entry carries the same post-hook content as the queued
        // holder, so the staged claim keeps the holder. A same-id entry
        // from the same sender is already queued: its answer stands and
        // this ack answers idempotently.
        if self
            .steering
            .admit_session_hooked(
                id,
                passed.content.clone(),
                passed.sender.clone(),
                staged,
                &emit,
            )
            .is_err()
        {
            accept(ack);
            return Ok(());
        }
        self.queued.push_back(Queued::Steer(passed, hooked));
        accept(ack);
        Ok(())
    }

    /// Runs the `before_message` hook on the raw text and frames the
    /// passed or replaced text: the ack travels with the verdict. `None`
    /// when the hook refused, failed or the loop is closing, with the ack
    /// already answered.
    fn hooked_message(
        &mut self,
        sent: &SessionMessage,
        ack: contract::inbox::Ack,
        turn: Option<&TurnId>,
    ) -> Result<Option<(Message, Hooked, contract::inbox::Ack)>, crate::Error> {
        let from = sent.from_session_id.clone();
        let parent = sent.parent_session_id.clone();
        let raw = Message {
            content: vec![ContentPart::Text {
                text: sent.text.clone(),
            }],
            sender: Sender {
                origin: Origin::Session {
                    from_session_id: from.clone(),
                },
                command_id: Some(sent.command_id.clone()),
            },
        };
        let session = contract::hook::MessageSender {
            from_session_id: &from,
            parent_session_id: parent.as_ref(),
        };
        let Some((passed, hooked, ack)) = self.admitted(raw, Some(session), ack, turn)? else {
            return Ok(None);
        };
        // The hook sees the raw text as one text part; a replacement is
        // text-only by construction, so joining its text parts loses
        // nothing and the model always sees the framed sender id and name.
        let text: String = passed
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
            })
            .collect();
        Ok(Some((framed(sent, &text), hooked, ack)))
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
                .any(|held| matches!(held, Delivery::Message(..)))
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

/// `message` as the model sees it: the post-hook `text` framed with the
/// sender's id and latest `session_named` name, one text part. The fill
/// runs once, so a name or text holding a placeholder stays literal.
fn framed(sent: &SessionMessage, text: &str) -> Message {
    let SessionMessage {
        from_session_id,
        command_id,
        name,
        ..
    } = sent;
    let filled = match name {
        Some(name) => fill(
            message("session-message").trim_end(),
            &[
                ("session_id", from_session_id.0.as_str()),
                ("name", name),
                ("text", text),
            ],
        ),
        None => fill(
            message("session-message-unnamed").trim_end(),
            &[("session_id", from_session_id.0.as_str()), ("text", text)],
        ),
    };
    Message {
        content: vec![ContentPart::Text { text: filled }],
        sender: Sender {
            origin: Origin::Session {
                from_session_id: from_session_id.clone(),
            },
            command_id: Some(command_id.clone()),
        },
    }
}

/// Builds a session message for tests: from `from` with parent `parent`,
/// named `name`, sent by `command`, saying `text`.
#[cfg(test)]
pub(crate) fn for_test(
    from: &str,
    parent: Option<&str>,
    name: Option<&str>,
    command: &str,
    text: &str,
) -> SessionMessage {
    SessionMessage {
        from_session_id: contract::SessionId(from.to_owned()),
        parent_session_id: parent.map(|parent| contract::SessionId(parent.to_owned())),
        command_id: CommandId(command.to_owned()),
        text: text.to_owned(),
        name: name.map(str::to_owned),
    }
}

#[cfg(test)]
#[path = "session_message_tests.rs"]
mod tests;
