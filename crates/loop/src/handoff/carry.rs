//! Owns the render state a handoff reads and the restart conversation.

use contract::ActionId;
use contract::events::{ContextNudged, Control, Event, HandoffCompleted, Note};
use contract::provider::Input;
use contract::shapes::Question;

use crate::prompt::{body, fill};

/// The render state a handoff reads: the same fold in the live loop and in a
/// rebuild, so the two never differ (`docs/loop.md`, "What the model is
/// sent").
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Carry {
    /// The conversation's length at the open `handoff_started`.
    pub(crate) window: Option<usize>,
    /// This turn's input: its message items and the steering messages
    /// applied, in log order. A handoff carries it verbatim.
    pub(crate) input: Vec<Input>,
    /// The jobs started and not completed, in start order, as
    /// `(job id, description)`.
    pub(crate) jobs: Vec<(String, String)>,
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
                    &body(crate::conversation::MESSAGES_MD, "handoff-jobs"),
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
        conversation
    }

    /// A call of the last reply was requested.
    pub(crate) fn call_requested(&mut self, action: &ActionId, call: &Input) {
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

    /// Tracks the jobs running: started adds, completed removes.
    pub(crate) fn fold_jobs(&mut self, event: &Event) {
        if let Event::JobStarted(job) = event {
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
            &body(crate::conversation::MESSAGES_MD, "nudge"),
            &[
                ("tokens", nudged.tokens.to_string().as_str()),
                ("trigger_at", nudged.trigger_at.to_string().as_str()),
                ("session_log", self.session_log.as_str()),
            ],
        )
    }
}
