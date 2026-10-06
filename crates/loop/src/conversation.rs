//! The conversation a request sends, rendered from the log's durable events
//! in log order (`docs/prompt-cache.md`, "What a request is built from";
//! `docs/loop.md`, "What the model is sent"). The loop renders each event as
//! it writes it, and a resume renders the log the same way, so the two never
//! differ.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use contract::events::{
    CallStatus, Event, InputItem, InstructionFile, InstructionReason, InstructionSent, Outcome,
    ToolCallCompleted, ToolCallRequested,
};
use contract::provider::{ImageRef, Input};
use contract::shapes::ContentPart;
use contract::{ActionId, Envelope};

use crate::Error;
use crate::handoff::Carry;
use crate::prompt::{body, fill};

pub(crate) const MESSAGES_MD: &str = include_str!("../prompt/messages.md");

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
    let (mut conversation, _, mut held, _) = rebuild_and_sent(lines, model, &HashSet::new())?;
    conversation.append(&mut held);
    Ok(conversation)
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

/// What [`rebuild_and_sent`] returns: the conversation, the previous
/// request's end, the notices held behind the open batch, and the render
/// state a handoff reads.
pub(crate) type Rebuilt = (Vec<Input>, Option<usize>, Vec<Input>, Carry);

/// The conversation `lines` render, with its length at the last
/// `assistant_message_started`: the previous request's end, for the cache
/// markers (`docs/prompt-cache.md`). `None` when the log holds none. The
/// length is read after the line renders, so the fixed results its flush
/// just added count. The third part is the job notices still waiting
/// behind calls whose results the log does not hold yet: the `open` batch
/// a resume finishes, which releases them after its results.
pub(crate) fn rebuild_and_sent(
    lines: &[Envelope],
    model: &str,
    open: &HashSet<ActionId>,
) -> Result<Rebuilt, Error> {
    let completed = completed_actions(lines)?;
    let mut rendered = Rendered::default();
    let mut sent = None;
    for line in lines.iter().filter(|l| l.is_durable()) {
        if let Some(event) = Event::from_envelope(line).map_err(Error::Unreadable)? {
            rendered.push(&event, line.action_id.as_ref(), model, &completed, open);
            if matches!(event, Event::AssistantMessageStarted(_)) {
                sent = Some(rendered.conversation.len());
            }
            // The context a completed handoff starts has had no request.
            if let Event::HandoffCompleted(done) = &event
                && done.outcome == Outcome::Completed
            {
                sent = None;
            }
        }
    }
    let (conversation, held, carry) = rendered.finish();
    Ok((conversation, sent, held, carry))
}

/// `rebuild`'s state: the conversation so far, and the calls requested but
/// not yet completed, in request order. A call with no `tool_call_completed`
/// gets its fixed result after the whole batch of calls it belongs to: the
/// whole reply plus its result lines. The next round, a `TurnStarted`,
/// `SteeringApplied` or `AssistantMessageStarted`, or the end of the log,
/// flushes them, as does a `job_completed` with no action, so a
/// `tool_call_started` written after the request is always seen before the
/// flush decides.
#[derive(Default)]
struct Rendered {
    conversation: Vec<Input>,
    pending: Vec<Pending>,
    /// Calls requested whose result is still to come: later in the log,
    /// or from the turn a resume finishes.
    outstanding: Vec<ActionId>,
    /// Job notices logged while calls were outstanding: they render after
    /// the last of those calls' results, so no message separates a call
    /// from its result.
    held: Vec<Input>,
    /// The content the model last had per instruction file path: a diff
    /// renders from this and the line's content, both in the log.
    had: BTreeMap<String, String>,
    /// What a handoff reads: this turn's input, the jobs running, the
    /// window a handoff has open.
    carry: Carry,
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
        open: &HashSet<ActionId>,
    ) {
        match event {
            // A call the provider ran has no fixed result and no pending
            // state. It renders with its result or not at all: sending the
            // call block alone would be refused.
            Event::ToolCallRequested(call) if call.provider_item.is_some() => {
                if action.is_some_and(|action| completed.contains(action)) {
                    self.render(event, action, model);
                }
            }
            Event::ToolCallCompleted(done) if done.provider_item.is_some() => {
                self.render(event, action, model);
            }
            Event::ToolCallRequested(call) => {
                if let Some(action) = action {
                    let input = Input::ToolCall {
                        action_id: action.clone(),
                        call: call.clone(),
                    };
                    self.carry.call_requested(action, &input);
                    self.conversation.push(input);
                    // A call the log completes needs no fixed result, nor
                    // does one the finishing turn completes on resume.
                    if !completed.contains(action) && !open.contains(action) {
                        self.pending.push(Pending {
                            action_id: action.clone(),
                            started: false,
                        });
                    } else {
                        self.outstanding.push(action.clone());
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
                    let result = result_of(action, completed);
                    self.carry.call_completed(
                        action,
                        &result,
                        completed.artifact.as_deref(),
                        noted(completed),
                    );
                    self.conversation.push(result);
                    self.pending.retain(|p| &p.action_id != action);
                    self.outstanding.retain(|id| id != action);
                    if self.outstanding.is_empty() {
                        self.conversation.append(&mut self.held);
                    }
                }
            }
            // A new process: no call the old one left without a result gets
            // one later, so each gets its fixed one now. A handoff window the
            // old process left open then closes, and its truncation takes
            // each call with its result, so no result outlives its call.
            Event::FiberStarted(_) => {
                self.flush();
                self.render(event, action, model);
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
            | Event::HandoffStarted(_)
            | Event::AssistantMessageStarted(_) => {
                self.flush();
                self.render(event, action, model);
            }
            // Every other kind either continues the batch or adds nothing.
            // Each is listed, so a new kind does not compile until it is
            // placed.
            Event::ReasoningCompleted(_)
            | Event::TextCompleted(_)
            | Event::AssistantMessageCompleted(_)
            | Event::JobsPendingNotified(_)
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
            | Event::RepositoryCodeOffered(_)
            | Event::RepositoryCodeResolved(_)
            | Event::UsageRecorded(_)
            | Event::QuotaNoticed(_)
            | Event::RetryScheduled(_)
            | Event::Notice(_)
            | Event::PreambleBuilt(_)
            | Event::ModelChanged(_)
            | Event::SkillsChanged(_)
            | Event::SkillsResent(_)
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
            | Event::DelegateFinished(_)
            | Event::CommandAccepted(_)
            | Event::CommandRejected(_) => {
                self.render(event, action, model);
            }
            // A monitor's batch joins at a step boundary, as a job's end
            // does, and waits for a suspended batch's results the same way.
            Event::JobLine(line) => {
                self.flush();
                if self.outstanding.is_empty() {
                    self.render(event, action, model);
                } else {
                    self.held.push(Input::User {
                        text: crate::jobs::line_text(line),
                    });
                }
            }
            // A job notice joins at a step boundary, as a steer does: it
            // starts a new batch. A `wait` or `stop` record, under its
            // call's action, continues the batch.
            // A notice a resume logged while a suspended batch was open
            // waits for that batch's results.
            Event::JobCompleted(job) => {
                if action.is_none() {
                    self.flush();
                }
                if action.is_none() && !self.outstanding.is_empty() {
                    self.carry.fold_jobs(event);
                    self.held.push(Input::User {
                        text: crate::jobs::notice_text(job),
                    });
                } else {
                    self.render(event, action, model);
                }
            }
        }
    }

    fn render(&mut self, event: &Event, action: Option<&ActionId>, model: &str) {
        render(
            &mut self.conversation,
            event,
            action,
            model,
            &mut self.had,
            &mut self.carry,
        );
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
                images: Vec::new(),
            });
        }
    }

    /// The conversation, the notices still held behind calls with no
    /// result yet, and the render state. A handoff window still open at the
    /// end is closed: the process died during it, and the handoff did not
    /// take effect.
    fn finish(mut self) -> (Vec<Input>, Vec<Input>, Carry) {
        self.flush();
        self.carry.close_window(&mut self.conversation);
        (self.conversation, self.held, self.carry)
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
    carry: &mut Carry,
) {
    carry.fold_jobs(event);
    match event {
        Event::TurnStarted(started) => {
            carry.input.clear();
            for item in &started.input {
                match item {
                    InputItem::Message { content, .. } => {
                        conversation.push(user(content));
                        carry.input.push(user(content));
                    }
                    // The jobs' own `job_completed` lines follow at the first
                    // step boundary and render there.
                    InputItem::Jobs { .. } => {}
                    // The handoff's own lines render what it does.
                    InputItem::Handoff { .. } => {}
                    // debt: a shell command starts a turn only once the
                    // shell exists (#296); it renders then.
                    InputItem::ShellCommand { .. } | InputItem::Unknown => {}
                }
            }
        }
        Event::SteeringApplied(steering) => {
            conversation.push(user(&steering.content));
            carry.input.push(user(&steering.content));
        }
        // The window opens at the conversation's length; a completed handoff
        // replaces the conversation, and any other end truncates it back, so
        // the note request's own lines never stay.
        Event::HandoffStarted(_) => {
            carry.window = Some(conversation.len());
        }
        Event::HandoffCompleted(done) => {
            if done.outcome == Outcome::Completed {
                carry.window = None;
                *conversation = carry.restart(done);
            } else {
                carry.close_window(conversation);
            }
        }
        // A new process, so a window left open was cut short by the death of
        // the old one.
        Event::FiberStarted(_) => carry.close_window(conversation),
        Event::ContextNudged(nudged) => {
            conversation.push(Input::User {
                text: carry.nudge_text(nudged),
            });
            carry.nudged = true;
        }
        // A job's end the model was not already given: one message. A
        // `wait` or `stop` record carries its call's action and renders
        // nothing; that call's result already said it.
        Event::JobCompleted(completed) if action.is_none() => conversation.push(Input::User {
            text: crate::jobs::notice_text(completed),
        }),
        // A monitor's batch: one message, rendered from the line alone.
        Event::JobLine(line) => conversation.push(Input::User {
            text: crate::jobs::line_text(line),
        }),
        // The opening message is rendered from its logged fields only, so
        // a resume renders the identical bytes; it is the conversation's
        // first message (`docs/system-prompt.md`, "Recording").
        // It sits at index 0 of the context it opens: after a handoff the
        // carried input and the note are already there.
        Event::OpeningMessage(message) => {
            conversation.insert(
                0,
                Input::User {
                    text: crate::opening::render(message),
                },
            );
            carry.session_log.clone_from(&message.environment.session_log);
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
        Event::TextCompleted(part) => {
            if carry.window.is_some()
                && let Some(action) = action
            {
                carry.texts.push((action.clone(), part.text.clone()));
            }
            conversation.push(Input::Assistant {
                model: model.to_owned(),
                text: part.text.clone(),
                provider_item: part.provider_item.clone(),
            });
        }
        // A call the provider ran, and its result, render as the raw blocks
        // they arrived as; the handoff carry does not record them.
        Event::ToolCallRequested(ToolCallRequested {
            provider_item: Some(item),
            ..
        })
        | Event::ToolCallCompleted(ToolCallCompleted {
            provider_item: Some(item),
            ..
        }) => conversation.push(Input::Assistant {
            model: model.to_owned(),
            text: String::new(),
            provider_item: Some(item.clone()),
        }),
        Event::ToolCallRequested(call) => {
            if let Some(action) = action {
                let input = Input::ToolCall {
                    action_id: action.clone(),
                    call: call.clone(),
                };
                carry.call_requested(action, &input);
                conversation.push(input);
            }
        }
        Event::ToolCallCompleted(completed) => {
            if let Some(action) = action {
                let result = result_of(action, completed);
                carry.call_completed(action, &result, completed.artifact.as_deref(), noted(completed));
                conversation.push(result);
            }
        }
        // A new reply starts a new step's calls.
        Event::AssistantMessageStarted(_) => carry.step.clear(),
        // A reply's closing line sends nothing: its text parts were already
        // rendered, and a failed call sends nothing. Its retry is a new action.
        Event::AssistantMessageCompleted(_)
        // Every other kind adds nothing the model reads. Each is listed, so a
        // new kind does not compile until it is placed. The ending notice's
        // text is its turn's `message` item; `jobs_pending_notified` only
        // records the jobs it named.
        | Event::JobsPendingNotified(_)
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
        | Event::ToolCallStarted(_)
        | Event::ToolCallDelta(_)
        | Event::PermissionRequested(_)
        | Event::PermissionResolved(_)
        | Event::InteractionRequested(_)
        | Event::InteractionResolved(_)
            | Event::RepositoryCodeOffered(_)
            | Event::RepositoryCodeResolved(_)
        | Event::UsageRecorded(_)
        | Event::QuotaNoticed(_)
        | Event::RetryScheduled(_)
        | Event::Notice(_)
        | Event::PreambleBuilt(_)
        | Event::ModelChanged(_)
        | Event::SkillsChanged(_)
        | Event::SkillsResent(_)
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
        | Event::DelegateFinished(_)
        | Event::JobCompleted(_)
        | Event::CommandAccepted(_)
        | Event::CommandRejected(_) => {}
    }
}

/// The tool result `completed` renders for `action`.
fn result_of(action: &ActionId, completed: &ToolCallCompleted) -> Input {
    Input::ToolResult {
        action_id: action.clone(),
        text: text(&completed.content),
        is_error: completed.status == CallStatus::Failed,
        images: images(&completed.content),
    }
}

/// The note a completed call's `control.handoff` carries. The loop acts on
/// the field, never on the tool that set it (`docs/handoff.md`, "A tool").
fn noted(completed: &ToolCallCompleted) -> Option<&str> {
    completed
        .control
        .as_ref()
        .filter(|_| completed.status == CallStatus::Completed)
        .map(|control| control.handoff.as_str())
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

/// The text parts of `content`, joined.
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

/// The image parts of `content`, in order, as the references a request reads
/// the stored files by.
pub(crate) fn images(content: &[ContentPart]) -> Vec<ImageRef> {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Image {
                path,
                mime_type,
                width,
                height,
            } => Some(ImageRef {
                path: path.clone(),
                mime_type: mime_type.clone(),
                width: *width,
                height: *height,
            }),
            ContentPart::Text { .. } | ContentPart::Unknown => None,
        })
        .collect()
}

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod tests;
