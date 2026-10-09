//! The approval queue: approvals and question forms from every session in
//! the order they arrived, the panel that shows one, and the `reply` that
//! answers it (`docs/tui.md`, "Approvals and questions").

use std::collections::HashMap;

use contract::commands::{Remember, RememberScope, Reply, ReplyAnswer};
use contract::events::{
    AskStep, Decision, Escalation, Interaction, InteractionRequested, PermissionRequested,
    RuleOffer, RuleScope, ToolCallRequested,
};
use contract::shapes::True;
use contract::{Envelope, RequestId, SessionId};
use serde_json::Value;

use crate::app::session_command;
use crate::keys::{Edit, Key};

pub(crate) mod form;

/// The envelope kinds the queue folds, from any session.
pub(crate) const KINDS: [&str; 5] = [
    "tool_call_requested",
    "permission_requested",
    "permission_resolved",
    "interaction_requested",
    "interaction_resolved",
];

/// A choice on the approval panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    /// Allow once.
    Once,
    /// Allow for this session.
    Session,
    /// Always allow in this project.
    Project,
    /// Deny, with the typed feedback.
    Deny,
}

/// One request in the queue: a session's `permission_requested`, or its
/// `interaction_requested` form, not yet resolved.
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
}

/// What a request asks: an approval or a question form.
#[derive(Debug, Clone)]
enum Ask {
    Approval(Approval),
    Form(form::Form),
}

/// An approval: the call it asks about and the person's choice so far.
#[derive(Debug, Clone)]
struct Approval {
    /// The asking call's tool and arguments, when its `tool_call_requested`
    /// was seen.
    call: Option<(String, String)>,
    reversible: bool,
    step: AskStep,
    /// What the person typed, kept while the request waits.
    feedback: String,
    cursor: Choice,
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

impl Approval {
    /// The rule an allow can remember, offered only at review.
    fn rule(&self) -> Option<&RuleOffer> {
        match &self.step {
            AskStep::Review { rule, .. } => rule.as_ref(),
            AskStep::StandingAsk { .. } => None,
        }
    }

    /// The choices it shows: the remembering ones only with a rule.
    fn choices(&self) -> Vec<Choice> {
        if self.rule().is_some() {
            vec![Choice::Once, Choice::Session, Choice::Project, Choice::Deny]
        } else {
            vec![Choice::Once, Choice::Deny]
        }
    }

    /// Why it asked.
    fn why(&self) -> String {
        match &self.step {
            AskStep::StandingAsk { standing_rule } => {
                let scope = match standing_rule.scope {
                    RuleScope::Global => "global",
                    RuleScope::Project => "project",
                };
                format!("asked by a {scope} rule: {}", standing_rule.prefix)
            }
            AskStep::Review {
                escalation:
                    Some(
                        Escalation::ConsecutiveBlocks { reason }
                        | Escalation::SessionBlocks { reason },
                    ),
                ..
            } => format!("the reviewer escalated: {reason}"),
            AskStep::Review {
                escalation: Some(Escalation::ReviewerFailed { error }),
                ..
            } => format!("the reviewer failed: {}", error.message),
            AskStep::Review {
                escalation: None, ..
            } => "no rule allows this call".to_owned(),
        }
    }

    /// The answer the cursor's choice sends.
    fn answer(&self) -> ReplyAnswer {
        let remember = |scope| {
            self.rule().map(|rule| Remember {
                scope,
                prefix: rule.prefix.clone(),
            })
        };
        let (decision, feedback, remember) = match self.cursor {
            Choice::Once => (Decision::Allow, None, None),
            Choice::Session => (Decision::Allow, None, remember(RememberScope::Session)),
            Choice::Project => (Decision::Allow, None, remember(RememberScope::Project)),
            Choice::Deny => {
                let typed = !self.feedback.trim().is_empty();
                (Decision::Deny, typed.then(|| self.feedback.clone()), None)
            }
        };
        ReplyAnswer::Approval {
            decision,
            feedback,
            remember,
        }
    }

    /// One choice row's text.
    fn label(&self, choice: Choice) -> String {
        let prefix = self
            .rule()
            .map(|rule| rule.prefix.as_str())
            .unwrap_or_default();
        match choice {
            Choice::Once => "allow once".to_owned(),
            Choice::Session => format!("allow for this session: {prefix}"),
            Choice::Project => format!("always allow in this project: {prefix}"),
            Choice::Deny if self.feedback.is_empty() => "deny · type to add feedback".to_owned(),
            Choice::Deny => format!("deny · {}", self.feedback),
        }
    }

    /// The panel's lines under `header`: why it asked, the call, the
    /// choices.
    fn lines(&self, mut header: String) -> Vec<String> {
        if !self.reversible {
            header.push_str(IRREVERSIBLE);
        }
        let mut lines = vec![header, self.why()];
        if let Some((tool, arguments)) = &self.call {
            lines.push(format!("{tool} {arguments}"));
        }
        for choice in self.choices() {
            let mark = if choice == self.cursor { '›' } else { ' ' };
            lines.push(format!("{mark} {}", self.label(choice)));
        }
        lines
    }

    /// A key on the shown approval. Esc and ⌥A act on the queue
    /// ([`Queue::on_key`]) before this.
    fn on_key(&mut self, key: &Key) -> Option<PanelKey> {
        match key {
            Key::Char(ch) => self.type_char(*ch),
            Key::Backspace => {
                self.feedback.pop();
                self.cursor = Choice::Deny;
            }
            Key::Up | Key::Down => {
                let choices = self.choices();
                let now = choices
                    .iter()
                    .position(|choice| *choice == self.cursor)
                    .unwrap_or_default();
                let next = if *key == Key::Up {
                    now.saturating_sub(1)
                } else {
                    now.saturating_add(1)
                };
                if let Some(choice) = choices.get(next) {
                    self.cursor = *choice;
                }
            }
            Key::Enter => return Some(PanelKey::Answer),
            // Tab, Shift+Tab, Ctrl+G and Ctrl+R do nothing in the panel,
            // which stands in the input box's place; F1 opens the key map
            // over it.
            Key::Esc
            | Key::AltA
            | Key::Tab
            | Key::BackTab
            | Key::CtrlG
            | Key::CtrlR
            | Key::CtrlV => {}
            // The layout's keys reach the screen behind the panel.
            Key::PageUp
            | Key::PageDown
            | Key::End
            | Key::CtrlC
            | Key::CtrlF
            | Key::F1
            | Key::CtrlO
            | Key::AltP
            | Key::AltR
            | Key::AltDigit(_) => {
                return None;
            }
            // The steering queue's keys do nothing while the panel is open.
            Key::AltUp | Key::AltDown | Key::AltX => {}
        }
        Some(PanelKey::Handled)
    }

    /// An editing key: a paste is typed into the feedback, a control
    /// character as a space; every other edit does nothing.
    fn on_edit(&mut self, edit: &Edit) {
        match edit {
            Edit::Paste(text) => {
                for ch in text.chars() {
                    self.type_char(if ch.is_control() { ' ' } else { ch });
                }
            }
            Edit::Left
            | Edit::Right
            | Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete => {}
        }
    }

    /// Typing goes to the feedback and moves the cursor to deny.
    fn type_char(&mut self, ch: char) {
        self.feedback.push(ch);
        self.cursor = Choice::Deny;
    }
}

/// What an irreversible call's header ends with (`docs/tui.md`, "An
/// approval").
pub(crate) const IRREVERSIBLE: &str = " · irreversible";

/// The request panel as it draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Panel {
    /// Its lines, before wrapping: the header, then the approval's or the
    /// form's rows.
    pub(crate) lines: Vec<String>,
    /// Whether it takes the alert tint: the reviewer escalated. Never on a
    /// form.
    pub(crate) alert: bool,
    /// The form's click targets; none on an approval.
    pub(crate) spots: Vec<PanelSpot>,
    /// The line holding the form's cursor; `None` on an approval, which
    /// keeps its top when it does not fit.
    pub(crate) cursor: Option<usize>,
    /// The words row's line and the text cursor's column on it, while the
    /// form's cursor is on that row.
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

/// What a key did on the open panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelKey {
    /// The panel took it; nothing to send.
    Handled,
    /// Answer the shown request.
    Answer,
    /// Decline the shown form: Esc, or "Chat about this".
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
    /// session asking, and its place among the requests waiting. A form fits
    /// its rows to `width` columns.
    pub(crate) fn panel(&self, width: u16) -> Option<Panel> {
        let at = self.shown_index()?;
        let request = self.requests.get(at)?;
        let waiting: Vec<usize> = self.waiting_indices().collect();
        let k = waiting
            .iter()
            .position(|index| *index == at)
            .unwrap_or_default()
            .saturating_add(1);
        let place = format!("{} · {k} of {}", request.session.0, waiting.len());
        Some(match &request.ask {
            Ask::Approval(approval) => Panel {
                lines: approval.lines(format!("approval · {place}")),
                alert: matches!(
                    approval.step,
                    AskStep::Review {
                        escalation: Some(_),
                        ..
                    }
                ),
                spots: Vec::new(),
                cursor: None,
                caret: None,
            },
            Ask::Form(form) => form.panel(format!("question · {place}"), width),
        })
    }

    /// Whether the panel shows a request.
    pub(crate) fn open(&self) -> bool {
        self.shown_index().is_some()
    }

    /// The badge line while the panel is closed and requests wait,
    /// counting `extra` requests held elsewhere with them.
    pub(crate) fn badge(&self, extra: usize) -> Option<String> {
        if self.shown.is_some() {
            return None;
        }
        let waiting = self.waiting_indices().count().saturating_add(extra);
        (waiting > 0).then(|| format!("! {waiting} waiting · /approvals or ⌥A"))
    }

    /// Handles a key while the panel is open; `None` when the panel is
    /// closed or the key is not the panel's. ⌥A moves to the next request
    /// waiting; Esc puts an approval aside and declines a form.
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
            None => {}
        }
    }

    /// A click on the shown form's `spot`; `None` when no form is shown.
    pub(crate) fn click(&mut self, spot: form::Spot) -> Option<PanelKey> {
        let at = self.shown_index()?;
        match &mut self.requests.get_mut(at)?.ask {
            Ask::Form(form) => Some(form.click(spot)),
            Ask::Approval(_) => None,
        }
    }

    /// Answers the shown request with the `reply` command `id`, returning
    /// its line, and moves the panel on. `None` when the panel is closed.
    pub(crate) fn answer(&mut self, id: &str) -> Option<String> {
        let at = self.shown_index()?;
        let answer = match &self.requests.get(at)?.ask {
            Ask::Approval(approval) => approval.answer(),
            Ask::Form(form) => form.answer(),
        };
        self.reply(at, id, answer)
    }

    /// Declines the shown form with the `reply` command `id`, returning its
    /// line, and moves the panel on. `None` when no form is shown.
    pub(crate) fn decline(&mut self, id: &str) -> Option<String> {
        let at = self.shown_index()?;
        let request = self.requests.get(at)?;
        let session = match request.ask {
            Ask::Form(_) => request.session.clone(),
            Ask::Approval(_) => return None,
        };
        let line = self.reply(at, id, ReplyAnswer::Declined { declined: True })?;
        self.declines.insert(id.to_owned(), session);
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
    /// its resolution. Only a `form` interaction is queued.
    pub(crate) fn fold(&mut self, envelope: &Envelope) {
        let session = &envelope.session_id;
        let action = envelope.action_id.as_ref().map(|id| id.0.clone());
        let payload = Value::Object(envelope.payload.clone());
        match envelope.kind.as_str() {
            "tool_call_requested" => {
                if let (Some(action), Ok(call)) =
                    (action, serde_json::from_value::<ToolCallRequested>(payload))
                {
                    // Raw text that was not JSON shows as the model sent it.
                    let arguments = call
                        .arguments
                        .as_str()
                        .map_or_else(|| call.arguments.to_string(), str::to_owned);
                    self.calls
                        .insert((session.0.clone(), action), (call.name, arguments));
                }
            }
            "permission_requested" => {
                let Ok(asked) = serde_json::from_value::<PermissionRequested>(payload) else {
                    return;
                };
                self.queue(session, asked.request_id.0, |calls| {
                    let call = action.and_then(|action| calls.remove(&(session.0.clone(), action)));
                    Ask::Approval(Approval {
                        call,
                        reversible: asked.declared.reversible,
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
                match asked.interaction {
                    Interaction::Form { fields } => {
                        self.queue(session, asked.request_id.0, |_| {
                            Ask::Form(form::Form::new(fields))
                        });
                    }
                    // The other kinds are not drawn yet (#1243).
                    Interaction::Confirm { .. }
                    | Interaction::Select { .. }
                    | Interaction::MultiSelect { .. }
                    | Interaction::TextInput { .. } => {}
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
    /// form raised again on resume, keeps what the person typed.
    fn queue(
        &mut self,
        session: &SessionId,
        request_id: String,
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
