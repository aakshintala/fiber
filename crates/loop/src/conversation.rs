//! The conversation a request sends, rendered from the log's durable events
//! in log order (`docs/prompt-cache.md`, "What a request is built from";
//! `docs/loop.md`, "What the model is sent"). The loop renders each event as
//! it writes it, and a resume renders the log the same way, so the two never
//! differ.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use contract::events::{
    CallStatus, Event, InputItem, InstructionFile, InstructionReason, InstructionSent,
};
use contract::provider::Input;
use contract::shapes::ContentPart;
use contract::{ActionId, Envelope};

use crate::Error;
use crate::prompt::{body, fill};

const MESSAGES_MD: &str = include_str!("../prompt/messages.md");

/// A call with no `tool_call_completed`, which only a crash can leave
/// (`docs/events.md`, "Resume"), is sent with a fixed result: it never
/// ran when the log shows no `tool_call_started` for it, and its outcome
/// is unknown when it does. Fiber never runs such a call again; the model
/// may make it again.
const NEVER_RAN: &str = "It never ran.";
/// A call that started but has no result is sent with this fixed result
/// (`docs/events.md`, "Resume").
const OUTCOME_UNKNOWN: &str = "Its outcome is unknown: it may have run.";

/// The conversation `lines` render, for a session whose model reference is
/// `model`. Lines of kinds this build does not know are skipped.
pub fn rebuild(lines: &[Envelope], model: &str) -> Result<Vec<Input>, Error> {
    Ok(rebuild_and_sent(lines, model)?.0)
}

/// The actions with a `tool_call_completed` in `lines`.
pub(crate) fn completed_actions(lines: &[Envelope]) -> Result<HashSet<ActionId>, Error> {
    let mut completed = HashSet::new();
    for line in lines.iter().filter(|l| l.is_durable()) {
        if let Some(Event::ToolCallCompleted(_)) =
            Event::from_envelope(line).map_err(Error::Unreadable)?
            && let Some(action) = &line.action_id
        {
            completed.insert(action.clone());
        }
    }
    Ok(completed)
}

/// The conversation `lines` render, with its length at the last
/// `assistant_message_started`: the previous request's end, for the cache
/// markers (`docs/prompt-cache.md`). `None` when the log holds none. The
/// length is read after the line renders, so the fixed results its flush
/// just added count.
pub(crate) fn rebuild_and_sent(
    lines: &[Envelope],
    model: &str,
) -> Result<(Vec<Input>, Option<usize>), Error> {
    let completed = completed_actions(lines)?;
    let mut rendered = Rendered::default();
    let mut sent = None;
    for line in lines.iter().filter(|l| l.is_durable()) {
        if let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? {
            rendered.push(&event, line.action_id.as_ref(), model, &completed);
            if matches!(event, Event::AssistantMessageStarted(_)) {
                sent = Some(rendered.conversation.len());
            }
        }
    }
    Ok((rendered.finish(), sent))
}

/// `rebuild`'s state: the conversation so far, and the calls requested but
/// not yet completed, in request order. A call with no `tool_call_completed`
/// gets its fixed result after the whole batch of calls it belongs to: the
/// whole reply plus its result lines. The next round, a `TurnStarted`,
/// `SteeringApplied` or `AssistantMessageStarted`, or the end of the log,
/// flushes them, so a `tool_call_started` written after the request is
/// always seen before the flush decides.
#[derive(Default)]
struct Rendered {
    conversation: Vec<Input>,
    pending: Vec<Pending>,
    /// The content the model last had per instruction file path: a diff
    /// renders from this and the line's content, both in the log.
    had: BTreeMap<String, String>,
}

struct Pending {
    action_id: ActionId,
    started: bool,
}

impl Rendered {
    fn push(
        &mut self,
        event: &Event,
        action: Option<&ActionId>,
        model: &str,
        completed: &HashSet<ActionId>,
    ) {
        match event {
            Event::ToolCallRequested(call) => {
                if let Some(action) = action {
                    self.conversation.push(Input::ToolCall {
                        action_id: action.clone(),
                        call: call.clone(),
                    });
                    // A call the log completes needs no fixed result.
                    if !completed.contains(action) {
                        self.pending.push(Pending {
                            action_id: action.clone(),
                            started: false,
                        });
                    }
                }
            }
            Event::ToolCallStarted(_) => {
                if let Some(action) = action
                    && let Some(pending) = self.pending.iter_mut().find(|p| &p.action_id == action)
                {
                    pending.started = true;
                }
            }
            Event::ToolCallCompleted(completed) => {
                if let Some(action) = action {
                    self.conversation.push(Input::ToolResult {
                        action_id: action.clone(),
                        text: text(&completed.content),
                        is_error: completed.status == CallStatus::Failed,
                    });
                    self.pending.retain(|p| &p.action_id != action);
                }
            }
            // The next round starts a new batch: every call of the last
            // one still without a result gets its fixed one first, before
            // the round's own input. Each is listed, so a new kind does
            // not compile until it is placed.
            Event::TurnStarted(_)
            | Event::SteeringApplied(_)
            | Event::OpeningMessage(_)
            | Event::InstructionFile(_)
            | Event::DateChanged(_)
            | Event::AssistantMessageStarted(_) => {
                self.flush();
                render(&mut self.conversation, event, action, model, &mut self.had);
            }
            // Every other kind either continues the batch or adds nothing.
            // Each is listed, so a new kind does not compile until it is
            // placed.
            Event::ReasoningCompleted(_)
            | Event::TextCompleted(_)
            | Event::AssistantMessageCompleted(_)
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
            | Event::AssistantMessageDelta(_)
            | Event::ToolCallArgumentsDelta(_)
            | Event::ReasoningStarted(_)
            | Event::ReasoningDelta(_)
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
            | Event::SkillsChanged(_)
            | Event::SkillsResent(_)
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
            | Event::CommandRejected(_) => {
                render(&mut self.conversation, event, action, model, &mut self.had);
            }
        }
    }

    fn flush(&mut self) {
        for pending in std::mem::take(&mut self.pending) {
            self.conversation.push(Input::ToolResult {
                action_id: pending.action_id,
                text: if pending.started {
                    OUTCOME_UNKNOWN
                } else {
                    NEVER_RAN
                }
                .to_owned(),
                is_error: true,
            });
        }
    }

    fn finish(mut self) -> Vec<Input> {
        self.flush();
        self.conversation
    }
}

/// Adds what `event`, about `action`, puts in the conversation. `model` is
/// the model reference in force, which produced any reasoning or text part.
// debt: the model reference is the session's one model until `/model`
// switches it; then it comes from the log's `model_changed`. A text part's
// `provider_item` is stamped with that same reference.
pub(crate) fn render(
    conversation: &mut Vec<Input>,
    event: &Event,
    action: Option<&ActionId>,
    model: &str,
    had: &mut BTreeMap<String, String>,
) {
    match event {
        Event::TurnStarted(started) => {
            for item in &started.input {
                match item {
                    InputItem::Message { content, .. } => conversation.push(user(content)),
                    // debt: these start a turn only once shell commands, jobs
                    // and handoff exist (#296, #299, #305); each renders then.
                    InputItem::ShellCommand { .. }
                    | InputItem::Jobs { .. }
                    | InputItem::Handoff { .. }
                    | InputItem::Unknown => {}
                }
            }
        }
        Event::SteeringApplied(steering) => conversation.push(user(&steering.content)),
        // The opening message is rendered from its logged fields only, so
        // a resume renders the identical bytes; it is the conversation's
        // first message (`docs/system-prompt.md`, "Recording").
        // debt: the skills listing is always empty; fixed by #511.
        Event::OpeningMessage(message) => {
            conversation.push(Input::User {
                text: crate::opening::render(message),
            });
            crate::changes::apply(had, event);
        }
        // An instruction file change appends what the model was sent:
        // a diff, the full text, or one line for a deletion. An own edit
        // and `sent: none` render nothing but still update what the model
        // has, so a later diff renders identically on resume.
        Event::InstructionFile(file) => {
            if let Some(text) = changed_message(file, had) {
                conversation.push(Input::User { text });
            }
            crate::changes::apply(had, event);
        }
        Event::DateChanged(changed) => {
            conversation.push(Input::User {
                text: fill(
                    &body(MESSAGES_MD, "date"),
                    &[("date", changed.date.as_str())],
                ),
            });
        }
        Event::ReasoningCompleted(reasoning) => conversation.push(Input::Reasoning {
            model: model.to_owned(),
            text: reasoning.text.clone(),
            provider_item: reasoning.provider_item.clone(),
        }),
        Event::TextCompleted(part) => conversation.push(Input::Assistant {
            model: model.to_owned(),
            text: part.text.clone(),
            provider_item: part.provider_item.clone(),
        }),
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
                    is_error: completed.status == CallStatus::Failed,
                });
            }
        }
        // A reply's closing line sends nothing: its text parts were already
        // rendered, and a failed call sends nothing. Its retry is a new action.
        Event::AssistantMessageCompleted(_)
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
        | Event::SkillsChanged(_)
        | Event::SkillsResent(_)
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

/// What an `instruction_file` line appends to the conversation, if
/// anything: `own_edit` and `sent: none` render nothing. A diff renders
/// from the content the model last had and the file's content now, both
/// in the log (`docs/system-prompt.md`, "Recording").
fn changed_message(file: &InstructionFile, had: &BTreeMap<String, String>) -> Option<String> {
    let content = file.content.as_deref().unwrap_or("");
    let text = match (&file.reason, &file.sent) {
        (InstructionReason::Changed, InstructionSent::Diff) => {
            let old = had.get(&file.path).map(String::as_str).unwrap_or("");
            let diff = crate::changes::unified_diff(old, content, &file.path);
            fill(
                &body(MESSAGES_MD, "diff-file"),
                &[("path", file.path.as_str()), ("diff", diff.as_str())],
            )
        }
        (InstructionReason::Changed, InstructionSent::Full) => fill(
            &body(MESSAGES_MD, "replaced-file"),
            &[("path", file.path.as_str()), ("content", content)],
        ),
        (InstructionReason::Created, InstructionSent::Full) => fill(
            &body(MESSAGES_MD, "created-file"),
            &[
                ("path", file.path.as_str()),
                ("dir", dir_of(&file.path).as_str()),
                ("content", content),
            ],
        ),
        (InstructionReason::Subdirectory, InstructionSent::Full) => fill(
            &body(MESSAGES_MD, "subdirectory-file"),
            &[
                ("path", file.path.as_str()),
                ("dir", dir_of(&file.path).as_str()),
                ("content", content),
            ],
        ),
        (InstructionReason::Deleted, InstructionSent::Deleted) => fill(
            &body(MESSAGES_MD, "deleted-file"),
            &[("path", file.path.as_str())],
        ),
        _ => return None,
    };
    Some(text)
}

/// The file's parent directory: `{dir}` for every section that has it.
fn dir_of(path: &str) -> String {
    Path::new(path)
        .parent()
        .map(|parent| parent.display().to_string())
        .unwrap_or_default()
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

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod tests;
