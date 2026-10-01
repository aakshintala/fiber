//! The conversation a request sends, rendered from the log's durable events
//! in log order (`docs/prompt-cache.md`, "What a request is built from";
//! `docs/loop.md`, "What the model is sent"). The loop renders each event as
//! it writes it, and a resume renders the log the same way, so the two never
//! differ.

use contract::events::{AssistantMessageCompleted, Event, InputItem, MessageOutcome};
use contract::provider::Input;
use contract::shapes::ContentPart;
use contract::{ActionId, Envelope};

use crate::Error;

/// The conversation `lines` render, for a session whose model reference is
/// `model`. Lines of kinds this build does not know are skipped.
pub fn rebuild(lines: &[Envelope], model: &str) -> Result<Vec<Input>, Error> {
    let mut conversation = Vec::new();
    for line in lines.iter().filter(|l| l.is_durable()) {
        if let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? {
            render(&mut conversation, &event, line.action_id.as_ref(), model);
        }
    }
    Ok(conversation)
}

/// Adds what `event`, about `action`, puts in the conversation. `model` is
/// the model reference in force, which produced any reasoning.
// ponytail: the model reference is the session's one model until `/model`
// switches it; then it comes from the log's `model_changed`.
pub(crate) fn render(
    conversation: &mut Vec<Input>,
    event: &Event,
    action: Option<&ActionId>,
    model: &str,
) {
    match event {
        Event::TurnStarted(started) => {
            for item in &started.input {
                match item {
                    InputItem::Message { content, .. } => conversation.push(user(content)),
                    // ponytail: these start a turn only once shell commands,
                    // jobs and handoff exist; each renders then.
                    InputItem::ShellCommand { .. }
                    | InputItem::Jobs { .. }
                    | InputItem::Handoff { .. }
                    | InputItem::Unknown => {}
                }
            }
        }
        Event::SteeringApplied(steering) => conversation.push(user(&steering.content)),
        Event::ReasoningCompleted(reasoning) => conversation.push(Input::Reasoning {
            model: model.to_owned(),
            text: reasoning.text.clone(),
            provider_item: reasoning.provider_item.clone(),
        }),
        Event::AssistantMessageCompleted(AssistantMessageCompleted {
            outcome: MessageOutcome::Completed,
            text,
            ..
        }) => conversation.push(Input::Assistant { text: text.clone() }),
        Event::ToolCallRequested(call) => {
            if let Some(action) = action {
                conversation.push(Input::ToolCall {
                    action_id: action.clone(),
                    call: call.clone(),
                });
            }
        }
        Event::ToolCallCompleted(completed) => {
            if let Some(action) = action {
                conversation.push(Input::ToolResult {
                    action_id: action.clone(),
                    text: text(&completed.content),
                });
            }
        }
        // A failed call sends nothing; its retry is a new action.
        Event::AssistantMessageCompleted(AssistantMessageCompleted {
            outcome: MessageOutcome::Failed,
            ..
        })
        // Every other kind adds nothing the model reads. Each is listed, so a
        // new kind does not compile until it is placed.
        | Event::FiberStarted(_)
        | Event::FiberExited(_)
        | Event::SessionStarted(_)
        | Event::Rewound(_)
        | Event::StepStarted(_)
        | Event::TurnCompleted(_)
        | Event::SteeringQueue(_)
        | Event::ShellCommand(_)
        | Event::SessionNamed(_)
        | Event::Clients(_)
        | Event::SessionStatus(_)
        | Event::ContextAdded(_)
        | Event::AssistantMessageStarted(_)
        | Event::AssistantMessageDelta(_)
        | Event::ToolCallArgumentsDelta(_)
        | Event::ReasoningStarted(_)
        | Event::ReasoningDelta(_)
        | Event::ToolCallStarted(_)
        | Event::ToolCallDelta(_)
        | Event::PermissionRequested(_)
        | Event::PermissionResolved(_)
        | Event::InteractionRequested(_)
        | Event::InteractionResolved(_)
        | Event::UsageRecorded(_)
        | Event::QuotaNoticed(_)
        | Event::RetryScheduled(_)
        | Event::Notice(_)
        | Event::PreambleBuilt(_)
        | Event::ModelChanged(_)
        | Event::OpeningMessage(_)
        | Event::InstructionFile(_)
        | Event::DateChanged(_)
        | Event::HandoffStarted(_)
        | Event::HandoffCompleted(_)
        | Event::ContextNudged(_)
        | Event::McpServerFailed(_)
        | Event::McpServerReady(_)
        | Event::Reloaded(_)
        | Event::ExtensionsLoaded(_)
        | Event::ExtensionStateSet(_)
        | Event::ExtensionStateUnset(_)
        | Event::ExtensionUi(_)
        | Event::ExtensionMessage(_)
        | Event::ExtensionExec(_)
        | Event::JobStarted(_)
        | Event::DelegateStarted(_)
        | Event::JobDelta(_)
        | Event::JobLine(_)
        | Event::DelegateFinished(_)
        | Event::JobCompleted(_)
        | Event::JobsPendingNotified(_)
        | Event::CommandAccepted(_)
        | Event::CommandRejected(_) => {}
    }
}

/// A message as the model reads it.
fn user(content: &[ContentPart]) -> Input {
    Input::User {
        text: text(content),
    }
}

/// The text parts of `content`, joined. Only text reaches the model yet.
pub(crate) fn text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
