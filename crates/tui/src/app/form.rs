//! Answering and declining the request the panel shows (`docs/tui.md`,
//! "Approvals and questions", "A question form").

use contract::Envelope;
use contract::events::EventKind;
use contract::events::{CommandAccepted, CommandRejected};

use super::{App, Effect, Kind, mint, read, session_command};
use crate::approvals::form::Spot;
use crate::approvals::{PanelKey, Queue};
use crate::input::Draft;

impl App {
    /// Sends the shown request's answer. With the link down nothing goes
    /// out and the request stays.
    pub(super) fn answer(&mut self) -> Effect {
        self.reply(Queue::answer)
    }

    /// Declines the shown form with `reply` `declined` (`docs/tui.md`, "A
    /// question form"). With the link down nothing goes out and the form
    /// stays.
    pub(super) fn decline(&mut self) -> Effect {
        self.reply(Queue::decline)
    }

    /// A click on the shown form's `spot` (`docs/tui.md`, "A question
    /// form"): it answers or declines as the key it stands for does.
    pub(super) fn form_click(&mut self, spot: Spot) -> Effect {
        match self.queue.click(spot) {
            Some(PanelKey::Answer) => self.answer(),
            Some(PanelKey::Decline) => self.decline(),
            Some(PanelKey::Handled) | None => Effect::None,
        }
    }

    /// Sends the `reply` line `make` builds for the shown request under a
    /// new command id, pending until the hub answers it.
    fn reply(&mut self, make: fn(&mut Queue, &str) -> Option<String>) -> Effect {
        if !self.outbox.is_up() {
            return Effect::None;
        }
        let id = mint();
        let Some(line) = make(&mut self.queue, &id) else {
            return Effect::None;
        };
        self.outbox.wait(&id, super::Waiting::Reply);
        self.outbox
            .pending
            .insert(id, (Kind::Reply, Draft::default()));
        Effect::Send(vec![line])
    }

    /// A `reply`'s acknowledgement, from any session, whether or not it is
    /// on screen: a rejection puts its request back with a notice; an
    /// accepted decline returns the `cancel` that ends its session's turn
    /// (`docs/tui.md`, "A question form"). Nothing for any other line.
    pub(super) fn reply_ack(&mut self, envelope: &Envelope) -> Vec<String> {
        let (id, rejection) = match envelope.kind() {
            EventKind::CommandAccepted => {
                let Some(accepted) = read!(envelope, CommandAccepted) else {
                    return Vec::new();
                };
                (accepted.command_id.0, None)
            }
            EventKind::CommandRejected => {
                let Some((Some(id), code, message)) = read!(envelope, CommandRejected)
                    .map(|rejected| (rejected.command_id, rejected.code, rejected.message))
                else {
                    return Vec::new();
                };
                (id.0, Some((code, message)))
            }
            EventKind::FiberStarted
            | EventKind::FiberExited
            | EventKind::SessionStarted
            | EventKind::Rewound
            | EventKind::Moved
            | EventKind::TurnStarted
            | EventKind::StepStarted
            | EventKind::TurnCompleted
            | EventKind::SteeringApplied
            | EventKind::SteeringQueue
            | EventKind::ShellCommand
            | EventKind::ShellCommandStarted
            | EventKind::ShellCommandDelta
            | EventKind::DirectoryAdded
            | EventKind::SessionNamed
            | EventKind::Archived
            | EventKind::Clients
            | EventKind::SessionStatus
            | EventKind::ContextAdded
            | EventKind::AssistantMessageStarted
            | EventKind::AssistantMessageDelta
            | EventKind::AssistantMessageCompleted
            | EventKind::TextCompleted
            | EventKind::ToolCallArgumentsDelta
            | EventKind::ReasoningStarted
            | EventKind::ReasoningDelta
            | EventKind::ReasoningCompleted
            | EventKind::ToolCallRequested
            | EventKind::ToolCallStarted
            | EventKind::ToolCallDelta
            | EventKind::ToolCallCompleted
            | EventKind::PermissionRequested
            | EventKind::PermissionResolved
            | EventKind::InteractionRequested
            | EventKind::InteractionResolved
            | EventKind::RepositoryCodeOffered
            | EventKind::RepositoryCodeResolved
            | EventKind::UsageRecorded
            | EventKind::QuotaNoticed
            | EventKind::QuotaRearmed
            | EventKind::RetryScheduled
            | EventKind::Notice
            | EventKind::PreambleBuilt
            | EventKind::ModelChanged
            | EventKind::OpeningMessage
            | EventKind::InstructionFile
            | EventKind::DateChanged
            | EventKind::SkillsChanged
            | EventKind::CompactionStarted
            | EventKind::CompactionCompleted
            | EventKind::SkillsResent
            | EventKind::ContextNudged
            | EventKind::ReviewerKept
            | EventKind::McpServerFailed
            | EventKind::McpServerReady
            | EventKind::Reloaded
            | EventKind::ExtensionsLoaded
            | EventKind::ExtensionStateSet
            | EventKind::ExtensionStateUnset
            | EventKind::ExtensionUi
            | EventKind::ExtensionMessage
            | EventKind::ExtensionLog
            | EventKind::ExtensionExec
            | EventKind::ExtensionHello
            | EventKind::ExtensionRequest
            | EventKind::JobStarted
            | EventKind::DelegateStarted
            | EventKind::JobDelta
            | EventKind::JobLine
            | EventKind::DelegateFinished
            | EventKind::JobCompleted
            | EventKind::JobsPendingNotified
            | EventKind::Unknown(_) => return Vec::new(),
        };
        if !matches!(self.outbox.pending.get(&id), Some((Kind::Reply, _))) {
            return Vec::new();
        }
        if let Some((code, message)) = rejection {
            self.rejected(&id, code, message);
            self.outbox.settled(&id);
            return Vec::new();
        }
        self.outbox.pending.remove(&id);
        self.outbox.settled(&id);
        let Some(session) = self.queue.declined(&id) else {
            return Vec::new();
        };
        let cancel = mint();
        let line = session_command(&cancel, "cancel", &session, None).to_string();
        self.outbox
            .pending
            .insert(cancel, (Kind::Cancel, Draft::default()));
        vec![line]
    }
}

#[cfg(test)]
#[path = "form_tests.rs"]
mod tests;
