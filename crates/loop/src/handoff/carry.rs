//! Owns the render state a handoff reads and the restart conversation.

use std::collections::HashSet;

use contract::ActionId;
use contract::events::{
    ContextNudged, Control, Event, HandoffCompleted, KeptMessage, Note, ShellCommand, SkillLoad,
};
use contract::provider::Input;
use contract::shapes::Question;

use crate::prompt::{fill, message};

/// The render state a handoff reads: the same fold in the live loop and in a
/// rebuild, so the two never differ (`docs/loop.md`, "What the model is
/// sent").
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Carry {
    /// Inner actions never enter the model's conversation or handoff carry.
    pub(crate) inner: HashSet<ActionId>,
    /// The conversation's length at the open `handoff_started`.
    pub(crate) window: Option<usize>,
    /// The conversation's length before the rewind summary turn.
    pub(crate) summary: Option<usize>,
    /// This turn's input: its message items and the steering messages
    /// applied, in log order. A handoff carries it verbatim.
    pub(crate) input: Vec<Input>,
    /// The jobs started and not completed, in start order, as
    /// `(job id, description)`.
    pub(crate) jobs: Vec<(String, String)>,
    /// Extension-owned jobs, retained after completion so later ends and
    /// orphan records never render for the model.
    pub(crate) extension_jobs: HashSet<String>,
    /// The session log's path, from the latest opening message.
    pub(crate) session_log: String,
    /// Whether this context was nudged.
    pub(crate) nudged: bool,
    /// The text parts the actions of a handoff window completed, by action:
    /// a completed handoff takes them; a window that ends otherwise leaves
    /// its few lines, which no later note names.
    pub(crate) texts: Vec<(ActionId, String)>,
    /// The calls of the last reply, in call order, with their results.
    pub(crate) step: Vec<StepCall>,
    /// The artifact of each tool result in this context that has one, by
    /// call, as a path relative to the session directory.
    pub(crate) artifacts: Vec<(ActionId, String)>,
    /// The skills this context loaded, in first-load order, one per name,
    /// each with the path of its latest load: every completed call's
    /// `control.skill` (`docs/tools.md`, "Skills"). A completed handoff
    /// moves it into `resend`.
    pub(crate) skills: Vec<SkillLoad>,
    /// The previous context's loads, in first-load order, armed by a
    /// completed handoff: the next opening sends each one again
    /// (`docs/handoff.md`, "What the model sees after a handoff"). A
    /// completed handoff replaces it, and the `skills_resent` render, the
    /// `opening_message` render and `write_opening` taking it clear it.
    pub(crate) resend: Vec<SkillLoad>,
    /// Where the next `session_start` context goes, once an opening
    /// message rendered: right after it, so the hook's contexts sit
    /// between the opening and the turn's input and the note
    /// (`docs/handoff.md`, "What the model sees after a handoff").
    /// `None` anywhere else: any other rendered event clears it, and only
    /// a `session_start` context honours it.
    pub(crate) after_opening: Option<usize>,
    /// The `shell_command` lines written since the last turn, with their
    /// line `seq`s, in written order: the next `turn_started` names each
    /// one once (`docs/events.md`, `turn_started`).
    pub(crate) shells: Vec<(u64, ShellCommand)>,
    /// Where the person's kept messages go in the context a completed
    /// handoff started: right after the opening message and its
    /// `session_start` contexts, before the carried input
    /// (`docs/handoff.md`, "What the model sees after a handoff"). Only a
    /// completed handoff arms it, at 0; the opening and each
    /// `session_start` context placed while it is armed move it on by one;
    /// the `reviewer_kept` line takes it.
    pub(crate) input_at: Option<usize>,
    /// The person's messages the reviewer still holds, oldest first, each
    /// under the `seq` and item of the line that holds it: the driver
    /// messages outside a rewind summary turn, pruned to the kept ones at
    /// every `reviewer_kept`, as the reviewer's own transcript is.
    pub(crate) person: Vec<(KeptMessage, Input)>,
    /// The index in `person` where this turn's messages start: the turn's
    /// input is carried whole, so a kept message at or after it is not
    /// inserted again. A position, never a key: a fork's log counts `seq`
    /// from its own start, so a parent's message can share a key with this
    /// turn's, and the reviewer keeps both.
    pub(crate) turn_from: usize,
}

impl Carry {
    /// Opens the summary's temporary conversation without replacing turn input.
    pub(crate) fn open_summary(&mut self, at: usize) {
        self.summary = Some(at);
    }

    /// Removes the summary turn's context on completion or after a crash.
    pub(crate) fn close_summary(&mut self, conversation: &mut Vec<Input>) {
        if let Some(at) = self.summary.take() {
            conversation.truncate(at);
        }
    }

    /// Whether rendered lines belong to a rewind summary turn.
    pub(crate) fn in_summary(&self) -> bool {
        self.summary.is_some()
    }

    /// Takes the opening slot: every rendered durable event clears it,
    /// and the opening and context arms below set what follows.
    pub(crate) fn take_after_opening(&mut self) -> Option<usize> {
        self.after_opening.take()
    }

    /// Holds a written `shell_command` line under its line `seq` for the
    /// next `turn_started` to name.
    pub(crate) fn hold(&mut self, seq: u64, line: &ShellCommand) {
        self.shells.push((seq, line.clone()));
    }

    /// The held lines' `seq`s, in held order, which is `seq` order: takes
    /// hold lines in the order the log wrote them.
    pub(crate) fn pending(&self) -> Vec<u64> {
        self.shells.iter().map(|(seq, _)| *seq).collect()
    }

    /// Takes the held line `seq` names, once: each held line is named by
    /// exactly one `turn_started`. `None` when no held line has `seq`.
    pub(crate) fn release(&mut self, seq: u64) -> Option<ShellCommand> {
        self.shells
            .iter()
            .position(|(held, _)| *held == seq)
            .map(|at| self.shells.remove(at).1)
    }
    /// The opening message rendered: the next `session_start` context
    /// goes right after it, at index 1.
    pub(crate) fn opened(&mut self) {
        self.after_opening = Some(1);
        if let Some(at) = &mut self.input_at {
            *at += 1;
        }
    }

    /// Where a `context_added` with hook `hook` goes given the taken
    /// `slot`, and the slot after it: a `session_start` context takes the
    /// slot and advances it past itself; anything else appends and leaves
    /// the slot cleared.
    pub(crate) fn place_context(&mut self, slot: Option<usize>, hook: &str) -> Option<usize> {
        match (slot, hook) {
            (Some(at), "session_start") => {
                self.after_opening = Some(at + 1);
                if let Some(input_at) = &mut self.input_at {
                    *input_at += 1;
                }
                Some(at)
            }
            _ => None,
        }
    }
}

/// One call of the last reply: the call, its result once written, and the
/// note and questions its result set as `control.handoff` and
/// `control.questions`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct StepCall {
    pub(crate) action: ActionId,
    pub(crate) call: Input,
    pub(crate) result: Option<Input>,
    pub(crate) note: Option<String>,
    pub(crate) questions: Vec<Question>,
}

impl Carry {
    /// The conversation after a completed handoff: the carried input, the
    /// note, a line for the jobs still running and, when tools set the note,
    /// the other calls of that step with their results.
    pub(crate) fn restart(&mut self, done: &HandoffCompleted) -> Vec<Input> {
        let mut conversation = self.input.clone();
        let texts = std::mem::take(&mut self.texts);
        let step = std::mem::take(&mut self.step);
        // The loads move into the resend the next opening sends again: a
        // skill sent again counts as loaded in the new context, so a later
        // handoff sends it again too (`docs/handoff.md`, "What the model
        // sees after a handoff").
        self.resend = std::mem::take(&mut self.skills);
        let ids: &[ActionId] = match &done.note {
            Some(Note::Actions { note }) => note,
            Some(Note::Hook { .. }) | None => &[],
        };
        let note = match &done.note {
            Some(Note::Hook { note_text, .. }) => note_text.clone(),
            Some(Note::Actions { .. }) | None => ids
                .iter()
                .map(|id| {
                    let parts: Vec<&str> = texts
                        .iter()
                        .filter(|(action, _)| action == id)
                        .map(|(_, text)| text.as_str())
                        .collect();
                    if parts.is_empty() {
                        step.iter()
                            .find(|call| call.action == *id)
                            .and_then(|call| call.note.clone())
                            .unwrap_or_default()
                    } else {
                        parts.join("\n")
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
        };
        conversation.push(Input::User {
            text: note,
            images: Vec::new(),
        });
        if !self.jobs.is_empty() {
            let listed: Vec<String> = self
                .jobs
                .iter()
                .map(|(id, description)| format!("- {id}: {description}"))
                .collect();
            conversation.push(Input::User {
                text: fill(
                    message("handoff-jobs"),
                    &[("jobs", listed.join("\n").as_str())],
                ),
                images: Vec::new(),
            });
        }
        // The model had not seen the other calls of a step a tool ended: they
        // follow the note, the calls then their results, in call order. A
        // tool ended the step when a note action is one of its calls; a
        // note request's action is a reply, never a call.
        if step.iter().any(|call| ids.contains(&call.action)) {
            let others: Vec<&StepCall> = step
                .iter()
                .filter(|call| !ids.contains(&call.action))
                .collect();
            conversation.extend(others.iter().map(|call| call.call.clone()));
            conversation.extend(others.iter().filter_map(|call| call.result.clone()));
        }
        // Only a result still in this context can be moved out of it.
        self.artifacts.retain(|(id, _)| {
            conversation.iter().any(
                |input| matches!(input, Input::ToolResult { action_id, .. } if action_id == id),
            )
        });
        self.nudged = false;
        self.input_at = Some(0);
        conversation
    }

    /// Records a person's message the reviewer holds, under its line's key.
    pub(crate) fn record_person(&mut self, key: KeptMessage, input: Input) {
        self.person.push((key, input));
    }

    /// Keeps only the entries `kept` names, in their current order, and
    /// moves `turn_from` to the count kept from before it.
    pub(crate) fn prune(&mut self, kept: &[KeptMessage]) {
        let before = self
            .person
            .iter()
            .take(self.turn_from)
            .filter(|(key, _)| kept.contains(key))
            .count();
        self.person.retain(|(key, _)| kept.contains(key));
        self.turn_from = before;
    }

    /// Carries the messages a `reviewer_kept` line names into the context a
    /// completed handoff started, at the armed spot and in the reviewer's
    /// order, then prunes the index to them. Nothing is inserted when no
    /// completed handoff armed the spot, a message of this turn stays where
    /// the turn's input put it, and a key naming no entry matches nothing.
    pub(crate) fn keep(&mut self, kept: &[KeptMessage], conversation: &mut Vec<Input>) {
        if let Some(mut at) = self.input_at.take() {
            for (key, input) in self.person.iter().take(self.turn_from) {
                if kept.contains(key) {
                    conversation.insert(at.min(conversation.len()), input.clone());
                    at += 1;
                }
            }
        }
        self.prune(kept);
    }

    /// A call of the last reply was requested.
    pub(crate) fn call_requested(&mut self, action: &ActionId, call: &Input) {
        if self.in_summary() {
            return;
        }
        self.step.push(StepCall {
            action: action.clone(),
            call: call.clone(),
            result: None,
            note: None,
            questions: Vec::new(),
        });
    }

    /// A call of the last reply completed, with its result, its artifact and
    /// its `control`, when it completed.
    pub(crate) fn call_completed(
        &mut self,
        action: &ActionId,
        result: &Input,
        artifact: Option<&str>,
        control: Option<&Control>,
    ) {
        if let Some(path) = artifact {
            self.artifacts.push((action.clone(), path.to_owned()));
        }
        if let Some(call) = self.step.iter_mut().find(|call| call.action == *action) {
            call.result = Some(result.clone());
            call.note = control.and_then(|control| control.handoff.clone());
            call.questions = control
                .and_then(|control| control.questions.clone())
                .unwrap_or_default();
        }
        if let Some(load) = control.and_then(|control| control.skill.clone()) {
            self.loaded(load);
        }
    }

    /// Records one skill load, in first-load order, one per name, with the
    /// path of its latest load: a reload keeps its position and takes the
    /// new path.
    pub(crate) fn loaded(&mut self, load: SkillLoad) {
        match self.skills.iter_mut().find(|kept| kept.name == load.name) {
            Some(kept) => kept.path = load.path,
            None => self.skills.push(load),
        }
    }

    /// Whether `action` is an inner call's: its request ran under an
    /// extension, so neither it nor its later lines enter the model's
    /// conversation or the handoff carry.
    pub(crate) fn is_inner(&self, action: &ActionId) -> bool {
        self.inner.contains(action)
    }

    /// The questions the last reply's completed calls set, concatenated in
    /// call order (`docs/tools.md`, "What a result carries").
    pub(crate) fn asked(&self) -> Vec<Question> {
        self.step
            .iter()
            .flat_map(|call| call.questions.iter().cloned())
            .collect()
    }

    /// The calls of the last reply whose result set `control.handoff`, in
    /// call order.
    pub(crate) fn noted(&self) -> Vec<ActionId> {
        self.step
            .iter()
            .filter(|call| call.note.is_some())
            .map(|call| call.action.clone())
            .collect()
    }

    /// Closes a window left open: truncates `conversation` back to where
    /// the handoff began, so the note request's lines never stay.
    pub(crate) fn close_window(&mut self, conversation: &mut Vec<Input>) {
        if let Some(length) = self.window.take() {
            conversation.truncate(length);
        }
    }

    /// Whether an extension, rather than the model, owns this job's end.
    pub(crate) fn owned(&self, job_id: &str) -> bool {
        self.extension_jobs.contains(job_id)
    }

    /// Tracks the jobs running: started adds, completed removes.
    pub(crate) fn fold_jobs(&mut self, event: &Event) {
        if let Event::JobStarted(job) = event {
            if job.extension.is_some() {
                self.extension_jobs.insert(job.job_id.0.clone());
            }
            self.jobs
                .push((job.job_id.0.clone(), job.description.clone()));
        }
        if let Event::JobCompleted(job) = event {
            self.jobs.retain(|(id, _)| *id != job.job_id.0);
        }
    }

    /// The nudge's text, from its payload.
    pub(crate) fn nudge_text(&self, nudged: &ContextNudged) -> String {
        fill(
            message("nudge"),
            &[
                ("tokens", nudged.tokens.to_string().as_str()),
                ("trigger_at", nudged.trigger_at.to_string().as_str()),
                ("session_log", self.session_log.as_str()),
            ],
        )
    }
}
