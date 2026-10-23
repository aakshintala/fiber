//! The conversation a request sends, rendered from the log's durable events
//! in log order (`docs/prompt-cache.md`, "What a request is built from";
//! `docs/loop.md`, "What the model is sent"). The loop renders each event as
//! it writes it, and a resume renders the log the same way, so the two never
//! differ.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use contract::events::{
    CallStatus, Class, Control, Event, InputItem, InstructionFile, InstructionReason,
    InstructionSent, KeptMessage, Outcome, ShellCommand, SkillLoad, ToolCallCompleted,
    ToolCallRequested,
};
use contract::provider::{ImageRef, Input, PdfRef};
use contract::shapes::{ContentPart, Origin};
use contract::{ActionId, Envelope, Seq};

use crate::Error;
use crate::handoff::Carry;
use crate::prompt::{fill, message};

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
/// `model`. Lines of kinds this build does not know are skipped. Only tests
/// rebuild a whole log directly; production resumes through
/// [`rebuild_and_sent`].
#[cfg(test)]
pub(crate) fn rebuild(lines: &[Envelope], model: &str) -> Result<Vec<Input>, Error> {
    let (mut conversation, _, mut held, _) =
        rebuild_and_sent(lines, model, &HashSet::new(), Carry::default())?;
    conversation.append(&mut held);
    Ok(conversation)
}

/// The actions with a `tool_call_completed` in `lines`.
pub(crate) fn completed_actions(lines: &[Envelope]) -> Result<HashSet<ActionId>, Error> {
    let mut completed = HashSet::new();
    // The kind is tested before the payload is parsed: only a completion
    // can fail this pass.
    for line in lines
        .iter()
        .filter(|l| l.is_durable() && l.kind == "tool_call_completed")
    {
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
/// a resume finishes, which releases them after its results. `seed` is the
/// render state before the first line: the default for a whole log, and
/// the jobs running and the session log's path at a resume window's start.
pub(crate) fn rebuild_and_sent(
    lines: &[Envelope],
    model: &str,
    open: &HashSet<ActionId>,
    seed: Carry,
) -> Result<Rebuilt, Error> {
    rebuild_with_cut(lines, model, open, seed, usize::MAX)
}

/// The cut path, keeping conversation across handoffs after `cut`.
pub(crate) fn summary_conversation(
    lines: &[Envelope],
    cut: usize,
    model: &str,
    seed: Carry,
) -> Result<(Vec<Input>, Option<usize>), Error> {
    let (mut conversation, sent, mut held, _) =
        rebuild_with_cut(lines, model, &HashSet::new(), seed, cut)?;
    conversation.append(&mut held);
    Ok((conversation, sent))
}

fn rebuild_with_cut(
    lines: &[Envelope],
    model: &str,
    open: &HashSet<ActionId>,
    seed: Carry,
    cut: usize,
) -> Result<Rebuilt, Error> {
    let completed = completed_actions(lines)?;
    // Live and rebuilt `Input`s carry the model in force at their line:
    // from the first `model_changed`'s `before`, each switch stamping
    // what follows with its `after`.
    let mut stamped = model.to_owned();
    for line in lines.iter().filter(|l| l.is_durable()) {
        if line.kind != "model_changed" {
            continue;
        }
        if let Some(Event::ModelChanged(changed)) =
            Event::from_envelope(line).map_err(Error::Unreadable)?
        {
            stamped = changed.before.model.clone();
            break;
        }
    }
    let mut rendered = Rendered {
        carry: seed,
        ..Rendered::default()
    };
    let mut sent = None;
    for (index, line) in lines.iter().enumerate().filter(|(_, l)| l.is_durable()) {
        if let Some(mut event) = Event::from_envelope(line).map_err(Error::Unreadable)? {
            if index >= cut {
                if let Event::HandoffCompleted(done) = &mut event {
                    done.outcome = Outcome::Failed;
                } else if matches!(
                    event,
                    Event::OpeningMessage(_) | Event::SkillsResent(_) | Event::ReviewerKept(_)
                ) {
                    continue;
                }
            }
            rendered.push(
                &event,
                line.action_id.as_ref(),
                line.seq.map(|seq| seq.0),
                &stamped,
                &completed,
                open,
            );
            if let Event::ModelChanged(changed) = &event {
                stamped = changed.after.model.clone();
            }
            if matches!(event, Event::AssistantMessageStarted(_)) && !rendered.carry.in_summary() {
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
        seq: Option<u64>,
        model: &str,
        completed: &HashSet<ActionId>,
        open: &HashSet<ActionId>,
    ) {
        let seq_key = seq.map(Seq);
        if inner_action(event, action, &mut self.carry) {
            self.carry.fold_jobs(event);
            crate::changes::apply(&mut self.had, event);
            return;
        }
        match event {
            // A call the provider ran has no fixed result and no pending
            // state. It renders with its result or not at all: sending the
            // call block alone would be refused.
            Event::ToolCallRequested(call) if call.provider_item.is_some() => {
                if action.is_some_and(|action| completed.contains(action)) {
                    self.render(event, action, model, seq_key);
                }
            }
            Event::ToolCallCompleted(done) if done.provider_item.is_some() => {
                self.render(event, action, model, seq_key);
            }
            Event::ToolCallRequested(call) => {
                if let Some(action) = action {
                    let input = Input::ToolCall {
                        action_id: action.clone(),
                        call: call.clone(),
                        model: model.to_owned(),
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
                        controlled(completed),
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
                self.render(event, action, model, seq_key);
            }
            // The next round starts a new batch: every call of the last
            // one still without a result gets its fixed one first, before
            // the round's own input. Each is listed, so a new kind does
            // not compile until it is placed.
            Event::TurnStarted(_)
            | Event::SteeringApplied(_)
            | Event::ContextAdded(_)
            | Event::OpeningMessage(_)
            | Event::SessionStarted(_)
            | Event::InstructionFile(_)
            | Event::SkillsChanged(_)
            | Event::DateChanged(_)
            | Event::HandoffStarted(_)
            | Event::AssistantMessageStarted(_) => {
                self.flush();
                self.render(event, action, model, seq_key);
            }
            // Every other kind either continues the batch or adds nothing.
            // Each is listed, so a new kind does not compile until it is
            // placed.
            Event::ReasoningCompleted(_)
            | Event::TextCompleted(_)
            | Event::AssistantMessageCompleted(_)
            | Event::JobsPendingNotified(_)
            | Event::FiberExited(_)
            | Event::Rewound(_)
            | Event::StepStarted(_)
            | Event::TurnCompleted(_)
            | Event::SteeringQueue(_)
            | Event::SessionNamed(_)
            | Event::Clients(_)
            | Event::SessionStatus(_)
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
            | Event::QuotaRearmed(_)
            | Event::RetryScheduled(_)
            | Event::Notice(_)
            | Event::PreambleBuilt(_)
            | Event::ModelChanged(_)
            | Event::SkillsResent(_)
            | Event::HandoffCompleted(_)
            | Event::ContextNudged(_)
            | Event::ReviewerKept(_)
            | Event::McpServerFailed(_)
            | Event::McpServerReady(_)
            | Event::Reloaded(_)
            | Event::ExtensionsLoaded(_)
            | Event::ExtensionStateSet(_)
            | Event::ExtensionStateUnset(_)
            | Event::ExtensionUi(_)
            | Event::ExtensionMessage(_)
            | Event::ExtensionLog(_)
            | Event::ExtensionExec(_)
            | Event::JobStarted(_)
            | Event::DelegateStarted(_)
            | Event::JobDelta(_)
            | Event::CommandAccepted(_)
            | Event::CommandRejected(_) => {
                self.render(event, action, model, seq_key);
            }
            // A sent shell waits for the next turn: held under its line's
            // `seq`, which the `turn_started` naming it releases. It
            // renders nothing here, so a turn never reads a shell taken
            // while it runs.
            Event::ShellCommand(line) => {
                if let Some(seq) = seq {
                    self.carry.hold(seq, line);
                }
            }
            // A monitor's batch joins at a step boundary, as a job's end
            // does, and waits for a suspended batch's results the same way.
            Event::JobLine(line) => {
                self.flush();
                if self.outstanding.is_empty() {
                    self.render(event, action, model, seq_key);
                } else {
                    self.held.push(Input::User {
                        text: crate::jobs::line_text(line),
                        images: Vec::new(),
                    });
                }
            }
            // A job notice joins at a step boundary, as a steer does: it
            // starts a new batch. A `wait` or `stop` record, under its
            // call's action, continues the batch. A delegate's finish
            // rides with its job's notice, in log order, and is held with
            // it.
            // A notice a resume logged while a suspended batch was open
            // waits for that batch's results.
            Event::JobCompleted(job) => {
                if self.carry.owned(&job.job_id.0) {
                    self.render(event, action, model, seq_key);
                    return;
                }
                if action.is_none() {
                    self.flush();
                }
                if action.is_none() && !self.outstanding.is_empty() {
                    self.carry.fold_jobs(event);
                    let text = crate::jobs::notice_text(job);
                    if !text.is_empty() {
                        self.held.push(Input::User {
                            text,
                            images: Vec::new(),
                        });
                    }
                } else {
                    self.render(event, action, model, seq_key);
                }
            }
            Event::DelegateFinished(finished) => {
                if self.carry.owned(&finished.job_id.0) {
                    self.render(event, action, model, seq_key);
                    return;
                }
                if action.is_none() {
                    self.flush();
                }
                if action.is_none() && !self.outstanding.is_empty() {
                    self.held.push(Input::User {
                        text: crate::jobs::delegate_text(finished),
                        images: Vec::new(),
                    });
                } else {
                    self.render(event, action, model, seq_key);
                }
            }
        }
    }

    fn render(&mut self, event: &Event, action: Option<&ActionId>, model: &str, seq: Option<Seq>) {
        render(
            &mut self.conversation,
            event,
            action,
            model,
            &mut self.had,
            &mut self.carry,
            seq,
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
                pdfs: Vec::new(),
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
/// the model reference in force, which produced any reasoning, text part or
/// tool call.
pub(crate) fn render(
    conversation: &mut Vec<Input>,
    event: &Event,
    action: Option<&ActionId>,
    model: &str,
    had: &mut BTreeMap<String, String>,
    carry: &mut Carry,
    seq: Option<Seq>,
) {
    // The opening slot lives only between an opening message and its
    // `session_start` contexts: every rendered durable event clears it,
    // so anything durable between them takes the contexts' place.
    // Ephemeral events leave it: the live loop never renders them, while
    // a rebuild still calls this function for them, and the two must
    // agree.
    let slot = if event.class() == Class::Durable {
        carry.take_after_opening()
    } else {
        None
    };
    carry.fold_jobs(event);
    if inner_action(event, action, carry) {
        crate::changes::apply(had, event);
        return;
    }
    match event {
        Event::TurnStarted(started) => {
            if crate::reviewer::summary_turn(started) {
                carry.open_summary(conversation.len());
                return;
            }
            carry.input.clear();
            carry.turn_from = carry.person.len();
            record_persons(carry, event, seq);
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
                    // A sent shell the turn names renders its held line as
                    // one user message; an item naming no held line renders
                    // nothing.
                    InputItem::ShellCommand { seq } => {
                        if let Some(held) = carry.release(seq.0) {
                            let text = shell_text(&held);
                            conversation.push(Input::User {
                                text: text.clone(),
                                images: Vec::new(),
                            });
                            carry.input.push(Input::User {
                                text,
                                images: Vec::new(),
                            });
                        }
                    }
                    InputItem::Rewind { .. } | InputItem::Unknown => {}
                }
            }
        }
        Event::SteeringApplied(steering) => {
            conversation.push(user(&steering.content));
            carry.input.push(user(&steering.content));
            if !carry.in_summary() {
                record_persons(carry, event, seq);
            }
        }
        // The person's kept messages join the context a completed handoff
        // started, between the opening and the carried input.
        Event::ReviewerKept(kept) => carry.keep(&kept.kept, conversation),
        // A hook's added context: its own user message, holding the text
        // alone. It joins neither the handoff's input nor what the
        // reviewer is shown (`docs/extensions.md`, "The hook points"). A
        // `session_start` context right after an opening message sits
        // between the opening and what follows; anywhere else it appends.
        Event::ContextAdded(added) => {
            let context = Input::User {
                text: added.text.clone(),
                images: Vec::new(),
            };
            match carry.place_context(slot, &added.hook) {
                Some(at) => conversation.insert(at.min(conversation.len()), context),
                None => conversation.push(context),
            }
        }
        // A rewind's note rides on its `session_started`: one user
        // message, so a live start and a resume render it the same way.
        Event::SessionStarted(started) => {
            if let Some(rewind) = &started.rewind {
                conversation.extend(crate::rewind::summary_input(rewind));
                conversation.push(crate::rewind::note_input(rewind));
            }
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
        Event::FiberStarted(_) => {
            carry.close_window(conversation);
            carry.close_summary(conversation);
        }
        Event::TurnCompleted(_) => carry.close_summary(conversation),
        Event::ContextNudged(nudged) => {
            conversation.push(Input::User {
                text: carry.nudge_text(nudged),
             images: Vec::new(),});
            carry.nudged = true;
        }
        // A job's end the model was not already given: one message. A
        // `wait` or `stop` record carries its call's action and renders
        // nothing; that call's result already said it.
        Event::JobCompleted(completed) if action.is_none() && !carry.owned(&completed.job_id.0) => {
            let text = crate::jobs::notice_text(completed);
            if !text.is_empty() {
                conversation.push(Input::User {
                    text,
                    images: Vec::new(),
                });
            }
        }
        // A delegate's finish the model was not already given: its final
        // message. A `wait` or `stop` record carries its call's action
        // and renders nothing; that call's result already said it.
        Event::DelegateFinished(finished) if action.is_none() && !carry.owned(&finished.job_id.0) => {
            conversation.push(Input::User {
            text: crate::jobs::delegate_text(finished),
         images: Vec::new(),})
        }
        // A monitor's batch: one message, rendered from the line alone.
        Event::JobLine(line) => conversation.push(Input::User {
            text: crate::jobs::line_text(line),
         images: Vec::new(),}),
        // The opening message is rendered from its logged fields only, so
        // a resume renders the identical bytes; it is the conversation's
        // first message (`docs/system-prompt.md`, "Recording").
        // It sits at index 0 of the context it opens: after a handoff the
        // carried input and the note are already there.
        // The skills the handoff armed are sent by their own event, before
        // this opening: the opening clears any arming the event did not
        // take, so nothing is sent twice.
        Event::SkillsResent(resent) => {
            carry.resend.clear();
            for sent in &resent.skills {
                conversation.push(Input::User {
                    text: format!("{}\n\n{}", sent.path, sent.content),
                 images: Vec::new(),});
                carry.loaded(SkillLoad {
                    name: sent.name.clone(),
                    path: sent.path.clone(),
                });
            }
        }
        Event::OpeningMessage(message) => {
            carry.resend.clear();
            conversation.insert(
                0,
                Input::User {
                    text: crate::opening::render(message),
                 images: Vec::new(),},
            );
            carry.opened();
            carry.session_log.clone_from(&message.environment.session_log);
            crate::changes::apply(had, event);
        }
        // An instruction file change appends what the model was sent:
        // a diff, the full text, or one line for a deletion. An own edit
        // and `sent: none` render nothing but still update what the model
        // has, so a later diff renders identically on resume.
        Event::InstructionFile(file) => {
            if let Some(text) = changed_message(file, had) {
                conversation.push(Input::User { text , images: Vec::new()});
            }
            crate::changes::apply(had, event);
        }
        Event::DateChanged(changed) => {
            conversation.push(Input::User {
                text: fill(
                    message("date"),
                    &[("date", changed.date.as_str())],
                ),
             images: Vec::new(),});
        }
        // An added or removed skill appends what the turn sent, rendered
        // from the event alone, so a resume renders the identical bytes
        // (`docs/system-prompt.md`, "Recording").
        Event::SkillsChanged(changed) => conversation.push(Input::User {
            text: crate::skill_set::SkillSet::changed_text(changed),
         images: Vec::new(),}),
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
                    model: model.to_owned(),
                };
                carry.call_requested(action, &input);
                conversation.push(input);
            }
        }
        Event::ToolCallCompleted(completed) => {
            if let Some(action) = action {
                let result = result_of(action, completed);
                carry.call_completed(
                    action,
                    &result,
                    completed.artifact.as_deref(),
                    controlled(completed),
                );
                conversation.push(result);
            }
        }
        // A new reply starts a new step's calls.
        Event::AssistantMessageStarted(_) => {
            if !carry.in_summary() {
                carry.step.clear();
            }
        }
        // A reply's closing line sends nothing: its text parts were already
        // rendered, and a failed call sends nothing. Its retry is a new action.
        Event::AssistantMessageCompleted(_)
        // Every other kind adds nothing the model reads. Each is listed, so a
        // new kind does not compile until it is placed. The ending notice's
        // text is its turn's `message` item; `jobs_pending_notified` only
        // records the jobs it named.
        | Event::JobsPendingNotified(_)
        | Event::FiberExited(_)
        | Event::Rewound(_)
        | Event::StepStarted(_)
        | Event::SteeringQueue(_)
        | Event::ShellCommand(_)
        | Event::SessionNamed(_)
        | Event::Clients(_)
        | Event::SessionStatus(_)
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
        | Event::QuotaNoticed(_) | Event::QuotaRearmed(_)
        | Event::RetryScheduled(_)
        | Event::Notice(_)
        | Event::PreambleBuilt(_)
        | Event::ModelChanged(_)
        | Event::McpServerFailed(_)
        | Event::McpServerReady(_)
        | Event::Reloaded(_)
        | Event::ExtensionsLoaded(_)
        | Event::ExtensionStateSet(_)
        | Event::ExtensionStateUnset(_)
        | Event::ExtensionUi(_)
        | Event::ExtensionMessage(_)
        | Event::ExtensionLog(_)
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

/// Records the person's messages `event` holds, under the line's `seq`:
/// each driver `message` item of a `turn_started`, keyed by its index among
/// every item, and a driver `steering_applied`. The same predicate as the
/// reviewer's transcript (`reviewer::render_reviewed`), so the two hold the
/// same messages. A line with no `seq` holds none. The caller skips a
/// rewind summary turn.
pub(crate) fn record_persons(carry: &mut Carry, event: &Event, seq: Option<Seq>) {
    let Some(seq) = seq else { return };
    if let Event::TurnStarted(started) = event {
        for (item, entry) in started.input.iter().enumerate() {
            if let InputItem::Message {
                content, sender, ..
            } = entry
                && sender.origin == Origin::Driver
            {
                let key = KeptMessage {
                    seq,
                    item: item as u64,
                };
                carry.record_person(key, user(content));
            }
        }
    } else if let Event::SteeringApplied(steering) = event
        && steering.sender.origin == Origin::Driver
    {
        carry.record_person(KeptMessage { seq, item: 0 }, user(&steering.content));
    }
}

/// Records inner requests before filtering their later lines. Unknown
/// completions still render: a resume window can begin after a request.
pub(crate) fn inner_action(event: &Event, action: Option<&ActionId>, carry: &mut Carry) -> bool {
    if let Event::ToolCallRequested(call) = event
        && call.ran_by.is_some()
    {
        if let Some(action) = action {
            carry.inner.insert(action.clone());
        }
        return true;
    }
    action.is_some_and(|action| carry.is_inner(action))
}

/// The tool result `completed` renders for `action`: a completed search
/// carrying a non-empty `loaded` renders natively as a `ToolSearchResult`
/// (`docs/tools.md`, "Tool search"). Every other completion, a failed,
/// denied or cancelled call, a withheld result, or an empty load, renders
/// as an ordinary `ToolResult` from its content.
fn result_of(action: &ActionId, completed: &ToolCallCompleted) -> Input {
    if completed.status == CallStatus::Completed
        && let Some(loaded) = completed.loaded.as_ref()
        && !loaded.is_empty()
    {
        return Input::ToolSearchResult {
            action_id: action.clone(),
            loaded: loaded.clone(),
        };
    }
    Input::ToolResult {
        action_id: action.clone(),
        text: text(&completed.content),
        is_error: completed.status == CallStatus::Failed,
        images: images(&completed.content),
        pdfs: pdfs(&completed.content),
    }
}

/// What counts as a load: a call Fiber ran to completion, whichever tool
/// wrote it (`docs/tools.md`, "What a result carries"). A failed, denied
/// or cancelled call's `control` is ignored, and a provider-run record
/// never counts: the live path renders it as a raw block and ignores its
/// `control`.
pub(crate) fn admitted(
    status: CallStatus,
    provider_run: bool,
    control: Option<&Control>,
) -> Option<&Control> {
    (!provider_run && status == CallStatus::Completed).then_some(control)?
}

/// A completed call's `control`; a failed or denied call's is ignored. The
/// loop acts on the fields, never on the tool that set them (`docs/tools.md`,
/// "What a result carries").
fn controlled(completed: &ToolCallCompleted) -> Option<&Control> {
    admitted(
        completed.status,
        completed.provider_item.is_some(),
        completed.control.as_ref(),
    )
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
                message("diff-file"),
                &[("path", file.path.as_str()), ("diff", diff.as_str())],
            )
        }
        (InstructionReason::Changed, InstructionSent::Full) => fill(
            message("replaced-file"),
            &[("path", file.path.as_str()), ("content", content)],
        ),
        (InstructionReason::Created, InstructionSent::Full) => match &file.extension {
            // A section file's creation names its section: the
            // instruction-file template's "applies to {dir}" is wrong
            // for it.
            Some(extension) => fill(
                message("created-section-file"),
                &[
                    ("extension", extension.as_str()),
                    ("path", file.path.as_str()),
                    ("content", content),
                ],
            ),
            None => fill(
                message("created-file"),
                &[
                    ("path", file.path.as_str()),
                    ("dir", dir_of(&file.path).as_str()),
                    ("content", content),
                ],
            ),
        },
        (InstructionReason::Subdirectory, InstructionSent::Full) => fill(
            message("subdirectory-file"),
            &[
                ("path", file.path.as_str()),
                ("dir", dir_of(&file.path).as_str()),
                ("content", content),
            ],
        ),
        (InstructionReason::Deleted, InstructionSent::Deleted) => {
            fill(message("deleted-file"), &[("path", file.path.as_str())])
        }
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

/// What the model reads for a sent shell: `! <command>`, its output,
/// then the exit code when non-zero, or the signal. The person reads the
/// same shape from the `shell_command` line (`crates/tui/src/shell.rs`).
fn shell_text(line: &ShellCommand) -> String {
    let mut text = format!("! {}", line.command);
    let output = line.output.trim_end();
    if !output.is_empty() {
        text.push('\n');
        text.push_str(output);
    }
    match (&line.process.signal, line.process.exit_code) {
        (Some(signal), _) => text.push_str(&format!("\nsignal {signal}")),
        (None, Some(code)) if code != 0 => text.push_str(&format!("\nexit {code}")),
        (None, Some(_) | None) => {}
    }
    text
}

/// A message as the model reads it.
fn user(content: &[ContentPart]) -> Input {
    Input::User {
        text: text(content),
        images: images(content),
    }
}

/// The text parts of `content`, joined.
pub(crate) fn text(content: &[ContentPart]) -> String {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
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
            ContentPart::Text { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
        })
        .collect()
}

/// The PDF parts of `content`, in order, as the references a request reads
/// the stored files by.
pub(crate) fn pdfs(content: &[ContentPart]) -> Vec<PdfRef> {
    content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Pdf(part) => Some(PdfRef {
                path: part.path().to_owned(),
                page_count: part.page_count(),
                pages: part
                    .pages()
                    .map(|pages| pages.iter().cloned().map(ImageRef::from).collect()),
            }),
            ContentPart::Text { .. } | ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .collect()
}

#[cfg(test)]
#[path = "conversation_tests.rs"]
mod tests;
