//! What the reviewer is shown (`docs/permissions.md`, "What it is shown"):
//! each person's message and each tool call, nothing else.

use contract::events::{Event, InputItem, KeptMessage};
use contract::provider::Input;
use contract::shapes::{ContentPart, Origin};
use contract::{ActionId, Seq};
use serde_json::Value;

/// What one reviewed item is.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Shown {
    /// The agent's tool call, by its action.
    Call(ActionId),
    /// A person's message, by the line that holds it.
    Person(KeptMessage),
}

/// One item of what the reviewer is shown (`docs/permissions.md`, "What it
/// is shown").
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Reviewed {
    /// What this item is: a call or a person's message.
    pub shown: Shown,
    /// The item as the reviewer reads it.
    pub input: Input,
}

/// Adds what `event` puts in what the reviewer is shown: each person's
/// message and each tool call, nothing else. `seq` is the durable line's
/// own `seq`: a person message with none is skipped, so every person item
/// carries the identity a later `reviewer_kept` names. A `reviewer_kept`
/// keeps only the person items it names, in their current order, and drops
/// every tool call.
pub(crate) fn render_reviewed(
    reviewed: &mut Vec<Reviewed>,
    event: &Event,
    action: Option<&ActionId>,
    seq: Option<Seq>,
) {
    match event {
        Event::TurnStarted(started) => {
            for (item, entry) in started.input.iter().enumerate() {
                if let InputItem::Message {
                    content, sender, ..
                } = entry
                    && sender.origin == Origin::Driver
                    && let Some(seq) = seq
                {
                    reviewed.push(person(
                        content,
                        Shown::Person(KeptMessage {
                            seq,
                            item: item as u64,
                        }),
                    ));
                }
            }
        }
        Event::SteeringApplied(steering) => {
            if steering.sender.origin == Origin::Driver
                && let Some(seq) = seq
            {
                reviewed.push(person(
                    &steering.content,
                    Shown::Person(KeptMessage { seq, item: 0 }),
                ));
            }
        }
        Event::ToolCallRequested(call) => {
            if let Some(action) = action {
                let arguments = match &call.repair {
                    Some(repair) => Value::Object(repair.repaired.clone()),
                    None => call.arguments.clone(),
                };
                let rendered = format!(
                    "{{\"tool\":{},\"arguments\":{}}}",
                    serde_json::to_string(&call.name).unwrap_or_default(),
                    serde_json::to_string(&arguments).unwrap_or_default(),
                );
                reviewed.push(Reviewed {
                    shown: Shown::Call(action.clone()),
                    input: Input::User {
                        text: format!("Tool call: {rendered}"),
                        images: Vec::new(),
                    },
                });
            }
        }
        Event::ReviewerKept(kept) => {
            reviewed.retain(|item| match &item.shown {
                Shown::Person(message) => kept.kept.contains(message),
                Shown::Call(_) => false,
            });
        }
        // Every other kind adds nothing the reviewer reads. Each is
        // listed, so a new kind does not compile until it is placed.
        Event::FiberStarted(_) | Event::FiberExited(_) | Event::SessionStarted(_) => {}
        Event::Rewound(_) | Event::StepStarted(_) | Event::TurnCompleted(_) => {}
        Event::SteeringQueue(_) | Event::ShellCommand(_) | Event::SessionNamed(_) => {}
        Event::Clients(_) | Event::SessionStatus(_) | Event::ContextAdded(_) => {}
        Event::AssistantMessageStarted(_)
        | Event::AssistantMessageDelta(_)
        | Event::AssistantMessageCompleted(_) => {}
        Event::TextCompleted(_) | Event::ToolCallArgumentsDelta(_) | Event::ReasoningStarted(_) => {
        }
        Event::ReasoningDelta(_) | Event::ReasoningCompleted(_) | Event::ToolCallStarted(_) => {}
        Event::ToolCallDelta(_) | Event::ToolCallCompleted(_) | Event::PermissionRequested(_) => {}
        Event::PermissionResolved(_)
        | Event::InteractionRequested(_)
        | Event::InteractionResolved(_) => {}
        Event::RepositoryCodeOffered(_) | Event::RepositoryCodeResolved(_) => {}
        Event::UsageRecorded(_) | Event::QuotaNoticed(_) | Event::RetryScheduled(_) => {}
        Event::Notice(_) | Event::PreambleBuilt(_) | Event::ModelChanged(_) => {}
        Event::OpeningMessage(_) | Event::InstructionFile(_) | Event::DateChanged(_) => {}
        Event::SkillsChanged(_) | Event::SkillsResent(_) => {}
        Event::HandoffStarted(_) | Event::HandoffCompleted(_) | Event::ContextNudged(_) => {}
        Event::McpServerFailed(_) | Event::McpServerReady(_) | Event::Reloaded(_) => {}
        Event::ExtensionsLoaded(_)
        | Event::ExtensionStateSet(_)
        | Event::ExtensionStateUnset(_) => {}
        Event::ExtensionUi(_)
        | Event::ExtensionMessage(_)
        | Event::ExtensionLog(_)
        | Event::ExtensionExec(_) => {}
        Event::JobStarted(_) | Event::DelegateStarted(_) | Event::JobDelta(_) => {}
        Event::JobLine(_) | Event::DelegateFinished(_) | Event::JobCompleted(_) => {}
        Event::JobsPendingNotified(_) | Event::CommandAccepted(_) | Event::CommandRejected(_) => {}
    }
}

/// A person's message as the reviewer reads it.
fn person(content: &[ContentPart], shown: Shown) -> Reviewed {
    Reviewed {
        shown,
        input: Input::User {
            text: format!("The person: {}", crate::conversation::text(content)),
            images: Vec::new(),
        },
    }
}

#[cfg(test)]
#[path = "shown_tests.rs"]
mod tests;
