//! The approval queue: approvals and question forms from every session in
//! the order they arrived, the panel that shows one, and the `reply` that
//! answers it (`docs/tui.md`, "Approvals and questions").

use std::collections::HashMap;

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{
    AskStep, Interaction, InteractionRequested, PermissionRequested, ToolCallRequested,
};
use contract::shapes::True;
use contract::{Envelope, RequestId, SessionId};
use ratatui::style::Style;
use serde_json::Value;

use crate::app::session_command;
use crate::format;
use crate::keys::{Edit, Key};

use approval::{Approval, Choice};

mod approval;
mod builder;
pub(crate) mod form;
mod one_question;
mod text;

/// The envelope kinds the queue folds, from any session.
pub(crate) const KINDS: [&str; 5] = [
    "tool_call_requested",
    "permission_requested",
    "permission_resolved",
    "interaction_requested",
    "interaction_resolved",
];

/// One unanswered request in the queue: an approval, a question form or a
/// one-question interaction.
#[derive(Debug, Clone)]
struct Request {
    session: SessionId,
    request_id: String,
    /// What it asks.
    ask: Ask,
    /// Put aside with Esc and not shown since.
    aside: bool,
    /// The `reply` sent for it; it leaves the visible queue until a
    /// rejection puts it back.
    answered_by: Option<String>,
    /// A tool call in the session's running turn raised it
    /// (`interaction_requested` carries `action_ids`): declining it cancels
    /// that turn.
    by_call: bool,
}

/// What a request asks: an approval, a question form or a one-question
/// interaction.
#[derive(Debug, Clone)]
enum Ask {
    Approval(Approval),
    Form(form::Form),
    One(one_question::OneQuestion),
}

impl Request {
    /// The queue key: session and request id.
    fn key(&self) -> (String, String) {
        (self.session.0.clone(), self.request_id.clone())
    }

    /// Whether it waits for an answer.
    fn waiting(&self) -> bool {
        self.answered_by.is_none()
    }
}

/// One styled run of a panel's line: the bytes `bytes` of `lines[line]`
/// draw in `style`; unmarked text draws plain. `bytes` lies on char
/// boundaries of the line, and marks on one line do not overlap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mark {
    pub(crate) line: usize,
    pub(crate) bytes: std::ops::Range<usize>,
    pub(crate) style: Style,
}

/// The request panel as it draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Panel {
    /// Its lines, before wrapping: the header, then the request's rows.
    pub(crate) lines: Vec<String>,
    /// Their styled runs, on an approval and on a question alike: the
    /// header's runs and each row's roles, so the draw needs no
    /// hand-counted offsets.
    pub(crate) marks: Vec<Mark>,
    /// Whether it takes the alert tint: the reviewer escalated. Never on a
    /// question.
    pub(crate) alert: bool,
    /// The question's click targets; none on an approval.
    pub(crate) spots: Vec<PanelSpot>,
    /// The line holding the question's cursor; `None` on an approval, which
    /// keeps its top when it does not fit.
    pub(crate) cursor: Option<usize>,
    /// The words row's line and the text cursor's column on it, while the
    /// question's cursor is on that row.
    pub(crate) caret: Option<(usize, u16)>,
}

/// A click target on the panel's line `line`: the columns `cols` in display
/// cells, start inclusive and end exclusive, or every row the line wraps to
/// when `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PanelSpot {
    pub(crate) line: usize,
    pub(crate) cols: Option<(u16, u16)>,
    pub(crate) spot: form::Spot,
}

/// Who a question panel asks for and where it sits in the queue: the
/// asking session's name when it has one, else its id, `place` only when
/// more than one request waits, and the form's question count, `None` on
/// a one-question panel (`docs/tui.md`, "A question form").
#[derive(Debug, Clone)]
pub(super) struct Title {
    pub(super) session: String,
    pub(super) place: Option<(usize, usize)>,
    pub(super) questions: Option<usize>,
}

/// What a key did on the open panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelKey {
    /// The panel took it; nothing to send.
    Handled,
    /// Answer the shown request.
    Answer,
    /// Decline the shown question: Esc, or "Chat about this".
    Decline,
}

/// The approval queue and the panel's place in it.
#[derive(Debug, Default)]
pub(crate) struct Queue {
    /// Requests in the order they arrived, from any session.
    requests: Vec<Request>,
    /// The request the panel shows, by key; `None` when the panel is
    /// closed.
    shown: Option<(String, String)>,
    /// Each call's tool and arguments by session and action, until a
    /// request about it arrives. debt: a call never asked about stays until
    /// the terminal exits, bounded by the sessions' calls; upgrade when tool
    /// groups (see #670) fold calls for display.
    calls: HashMap<(String, String), (String, String)>,
    /// Declines sent and not yet answered, by `reply` command id: the
    /// session whose turn a `cancel` ends once the decline is accepted.
    declines: HashMap<String, SessionId>,
}

impl Queue {
    /// The request panel, while it is open: `approval` or `question`, the
    /// session asking, and its place among the requests waiting. A question
    /// fits its rows to `width` columns. The session's name comes from
    /// `name`, or its id when the lookup yields nothing or an empty name.
    /// Tests only: the terminal passes its feed-row lookup.
    #[cfg(test)]
    pub(crate) fn panel(&self, width: u16) -> Option<Panel> {
        self.panel_with(width, &|_| None)
    }

    /// [`Queue::panel`], with the session's name from `name`.
    pub(crate) fn panel_with(
        &self,
        width: u16,
        name: &dyn Fn(&SessionId) -> Option<String>,
    ) -> Option<Panel> {
        let at = self.shown_index()?;
        let request = self.requests.get(at)?;
        let waiting: Vec<usize> = self.waiting_indices().collect();
        let k = waiting
            .iter()
            .position(|index| *index == at)
            .unwrap_or_default()
            .saturating_add(1);
        let session = name(&request.session)
            .filter(|name| !name.is_empty())
            .map(|name| crate::text::controls_to_space(&name))
            .unwrap_or_else(|| request.session.0.clone());
        Some(match &request.ask {
            Ask::Approval(approval) => {
                let (lines, marks, caret) = approval.render(k, waiting.len(), &session, width);
                Panel {
                    lines,
                    marks,
                    alert: matches!(
                        approval.step,
                        AskStep::Review {
                            escalation: Some(_),
                            ..
                        }
                    ),
                    spots: Vec::new(),
                    cursor: None,
                    caret,
                }
            }
            Ask::Form(form) => {
                let title = Title {
                    session,
                    place: (waiting.len() > 1).then_some((k, waiting.len())),
                    questions: Some(form.question_count()),
                };
                form.panel(&title, width)
            }
            Ask::One(question) => {
                let title = Title {
                    session,
                    place: (waiting.len() > 1).then_some((k, waiting.len())),
                    questions: None,
                };
                question.panel(&title, width)
            }
        })
    }

    /// Whether the panel shows a request.
    pub(crate) fn open(&self) -> bool {
        self.shown_index().is_some()
    }

    /// The badge row while the panel is closed and requests wait,
    /// counting `extra` requests held elsewhere with them
    /// (`docs/tui.md`, "An approval").
    pub(crate) fn badge(&self, extra: usize) -> Option<String> {
        if self.shown.is_some() {
            return None;
        }
        let waiting = self.waiting_indices().count().saturating_add(extra);
        if waiting == 0 {
            return None;
        }
        Some(format!("  ⚠ {waiting} approvals waiting · click to reopen"))
    }

    /// Handles a key while the panel is open; `None` when the panel is
    /// closed or the key is not the panel's. ⌥A moves to the next request
    /// waiting; Esc puts an approval aside and declines a question.
    pub(crate) fn on_key(&mut self, key: &Key) -> Option<PanelKey> {
        let at = self.shown_index()?;
        if *key == Key::AltA {
            let next = (at.saturating_add(1)..self.requests.len())
                .chain(0..at)
                .find(|index| self.requests.get(*index).is_some_and(Request::waiting));
            if let Some(next) = next {
                self.show(next);
            }
            return Some(PanelKey::Handled);
        }
        let request = self.requests.get_mut(at)?;
        match &mut request.ask {
            Ask::Approval(_) if *key == Key::Esc => {
                request.aside = true;
                self.show_after(at);
                Some(PanelKey::Handled)
            }
            Ask::Approval(approval) => approval.on_key(key),
            Ask::Form(form) => form.on_key(key),
            Ask::One(question) => question.on_key(key),
        }
    }

    /// Hands an editing key to the shown request (`docs/tui.md`, "A
    /// question form").
    pub(crate) fn on_edit(&mut self, edit: &Edit) {
        let Some(at) = self.shown_index() else {
            return;
        };
        match self.requests.get_mut(at).map(|request| &mut request.ask) {
            Some(Ask::Approval(approval)) => approval.on_edit(edit),
            Some(Ask::Form(form)) => form.on_edit(edit),
            Some(Ask::One(question)) => question.on_edit(edit),
            None => {}
        }
    }

    /// A click on the shown question's `spot`; `None` when no question is shown.
    pub(crate) fn click(&mut self, spot: form::Spot) -> Option<PanelKey> {
        let at = self.shown_index()?;
        match &mut self.requests.get_mut(at)?.ask {
            Ask::Form(form) => Some(form.click(spot)),
            Ask::One(question) => Some(question.click(spot)),
            Ask::Approval(_) => None,
        }
    }

    /// Answers the shown request with the `reply` command `id`, returning
    /// its line, and moves the panel on. `None` when the panel is closed.
    pub(crate) fn answer(&mut self, id: &str) -> Option<String> {
        let at = self.shown_index()?;
        let answer = match &self.requests.get(at)?.ask {
            Ask::Approval(approval) => Some(approval.answer()),
            Ask::Form(form) => Some(form.answer()),
            Ask::One(question) => question.answer(),
        }?;
        self.reply(at, id, answer)
    }

    /// Declines the shown question with the `reply` command `id`, returning
    /// its line, and moves the panel on. `None` when no question is shown.
    pub(crate) fn decline(&mut self, id: &str) -> Option<String> {
        let at = self.shown_index()?;
        let request = self.requests.get(at)?;
        let session = match request.ask {
            Ask::Form(_) | Ask::One(_) => request.session.clone(),
            Ask::Approval(_) => return None,
        };
        let by_call = request.by_call;
        let line = self.reply(at, id, ReplyAnswer::Declined { declined: True })?;
        if by_call {
            self.declines.insert(id.to_owned(), session);
        }
        Some(line)
    }

    /// The session whose turn a `cancel` ends now that the decline `id` was
    /// accepted (`docs/tui.md`, "A question form"); `None` when `id` is no
    /// decline. It is answered once.
    pub(crate) fn declined(&mut self, id: &str) -> Option<SessionId> {
        self.declines.remove(id)
    }

    /// Sends `answer` for the request at `at` as `reply` command `id`: it
    /// leaves the visible queue until a rejection puts it back.
    fn reply(&mut self, at: usize, id: &str, answer: ReplyAnswer) -> Option<String> {
        let request = self.requests.get_mut(at)?;
        let reply = Reply {
            request_id: RequestId(request.request_id.clone()),
            answer,
        };
        let args = serde_json::to_value(reply).ok();
        let line = session_command(id, "reply", &request.session, args).to_string();
        request.answered_by = Some(id.to_owned());
        self.show_after(at);
        Some(line)
    }

    /// The `reply` command `id` failed: its request, if it is still
    /// pending, is back where it was, unanswered, and a failed decline
    /// cancels nothing.
    pub(crate) fn restore(&mut self, id: &str) {
        self.declines.remove(id);
        let Some(at) = self
            .requests
            .iter()
            .position(|request| request.answered_by.as_deref() == Some(id))
        else {
            return;
        };
        if let Some(request) = self.requests.get_mut(at) {
            request.answered_by = None;
        }
        self.surface(at);
    }

    /// `/approvals` and Alt+A with the panel closed: the panel opens at the
    /// first request waiting. False when none waits.
    pub(crate) fn open_first(&mut self) -> bool {
        let first = self.waiting_indices().next();
        if let Some(first) = first {
            self.show(first);
        }
        first.is_some()
    }

    /// Shows the request `request_id` from `session` while it waits; a
    /// request answered or not queued shows nothing.
    pub(crate) fn open_request(&mut self, session: &SessionId, request_id: &str) {
        let at = self.requests.iter().position(|request| {
            request.session == *session && request.request_id == request_id && request.waiting()
        });
        if let Some(at) = at {
            self.show(at);
        }
    }

    /// Folds one of [`KINDS`] from any session: the call, the request and
    /// its resolution.
    pub(crate) fn fold(&mut self, envelope: &Envelope) {
        let session = &envelope.session_id;
        let action = envelope.action_id.as_ref().map(|id| id.0.clone());
        let payload = Value::Object(envelope.payload.clone());
        match envelope.kind.as_str() {
            "tool_call_requested" => {
                if let (Some(action), Ok(call)) =
                    (action, serde_json::from_value::<ToolCallRequested>(payload))
                {
                    // The main argument, never JSON: raw text that was not
                    // JSON shows as the model sent it.
                    let summary = match &call.arguments {
                        Value::String(text) => crate::text::controls_to_space(text),
                        Value::Null
                        | Value::Bool(_)
                        | Value::Number(_)
                        | Value::Array(_)
                        | Value::Object(_) => crate::text::controls_to_space(&format::summary(
                            &call.name,
                            &call.arguments,
                        )),
                    };
                    self.calls
                        .insert((session.0.clone(), action), (call.name, summary));
                }
            }
            "permission_requested" => {
                let Ok(asked) = serde_json::from_value::<PermissionRequested>(payload) else {
                    return;
                };
                self.queue(session, asked.request_id.0, false, |calls| {
                    let call = action.and_then(|action| calls.remove(&(session.0.clone(), action)));
                    Ask::Approval(Approval {
                        call,
                        reversible: asked.declared.reversible,
                        effects: asked.declared.effects.clone(),
                        paths: asked
                            .declared
                            .paths
                            .clone()
                            .unwrap_or_default()
                            .iter()
                            .map(|path| crate::text::controls_to_space(path))
                            .collect(),
                        step: asked.step,
                        feedback: String::new(),
                        cursor: Choice::Once,
                    })
                });
            }
            "interaction_requested" => {
                let Ok(asked) = serde_json::from_value::<InteractionRequested>(payload) else {
                    return;
                };
                let by_call = asked.action_ids.is_some();
                match asked.interaction {
                    Interaction::Form { fields } => {
                        self.queue(session, asked.request_id.0, by_call, |_| {
                            Ask::Form(form::Form::new(fields))
                        });
                    }
                    Interaction::Confirm { prompt } => {
                        self.queue(session, asked.request_id.0, by_call, |_| {
                            Ask::One(one_question::OneQuestion::confirm(prompt))
                        });
                    }
                    Interaction::Select { prompt, options } => {
                        self.queue(session, asked.request_id.0, by_call, |_| {
                            Ask::One(one_question::OneQuestion::select(prompt, options))
                        });
                    }
                    Interaction::MultiSelect { prompt, options } => {
                        self.queue(session, asked.request_id.0, by_call, |_| {
                            Ask::One(one_question::OneQuestion::multi_select(prompt, options))
                        });
                    }
                    Interaction::TextInput { prompt } => {
                        self.queue(session, asked.request_id.0, by_call, |_| {
                            Ask::One(one_question::OneQuestion::text_input(prompt))
                        });
                    }
                }
            }
            "permission_resolved" | "interaction_resolved" => {
                let Some(request_id) = envelope.payload.get("request_id").and_then(Value::as_str)
                else {
                    return;
                };
                let key = (session.0.clone(), request_id.to_owned());
                let Some(at) = self
                    .requests
                    .iter()
                    .position(|request| request.key() == key)
                else {
                    return;
                };
                if self.shown.as_ref() == Some(&key) {
                    self.show_after(at);
                }
                self.requests.remove(at);
            }
            _ => {}
        }
    }

    /// Queues the request `request_id` from `session`, made by `ask` from
    /// the calls seen, and surfaces it. A request already queued, such as a
    /// interaction raised again on resume, keeps what the person typed.
    fn queue(
        &mut self,
        session: &SessionId,
        request_id: String,
        by_call: bool,
        ask: impl FnOnce(&mut HashMap<(String, String), (String, String)>) -> Ask,
    ) {
        let key = (session.0.clone(), request_id.clone());
        if self.requests.iter().any(|request| request.key() == key) {
            return;
        }
        self.requests.push(Request {
            session: session.clone(),
            request_id,
            ask: ask(&mut self.calls),
            aside: false,
            answered_by: None,
            by_call,
        });
        self.surface(self.requests.len().saturating_sub(1));
    }

    /// The index of the request the panel shows.
    fn shown_index(&self) -> Option<usize> {
        let shown = self.shown.as_ref()?;
        self.requests
            .iter()
            .position(|request| request.key() == *shown)
    }

    /// The indices of the requests waiting, in arrival order.
    fn waiting_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.requests
            .iter()
            .enumerate()
            .filter(|(_, request)| request.waiting())
            .map(|(at, _)| at)
    }

    /// Shows the request at `at`; it is no longer put aside.
    fn show(&mut self, at: usize) {
        if let Some(request) = self.requests.get_mut(at) {
            request.aside = false;
            self.shown = Some(request.key());
        }
    }

    /// Shows the first request waiting after `at`; past the last, the panel
    /// closes.
    fn show_after(&mut self, at: usize) {
        let next = self.waiting_indices().find(|index| *index > at);
        match next {
            Some(next) => self.show(next),
            None => self.shown = None,
        }
    }

    /// A request newly waiting opens the panel at itself only when the
    /// panel is closed and nothing waiting was put aside.
    fn surface(&mut self, at: usize) {
        let aside = self
            .requests
            .iter()
            .any(|request| request.waiting() && request.aside);
        if self.shown.is_none() && !aside {
            self.show(at);
        }
    }
}

#[cfg(test)]
#[path = "approvals_tests.rs"]
mod tests;
