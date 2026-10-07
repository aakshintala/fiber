//! The approval queue: requests from every session in the order they
//! arrived, the panel that shows one, and the `reply` that answers it
//! (`docs/tui.md`, "Approvals and questions").

use std::collections::HashMap;

use contract::commands::{Remember, RememberScope, Reply, ReplyAnswer};
use contract::events::{
    AskStep, Decision, Escalation, PermissionRequested, RuleOffer, RuleScope, ToolCallRequested,
};
use contract::{Envelope, RequestId, SessionId};
use serde_json::Value;

use crate::app::session_command;
use crate::keys::Key;

/// The envelope kinds the queue folds, from any session.
pub(crate) const KINDS: [&str; 3] = [
    "tool_call_requested",
    "permission_requested",
    "permission_resolved",
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

/// One approval request in the queue: a session's `permission_requested`
/// not yet resolved.
#[derive(Debug, Clone)]
struct Request {
    session: SessionId,
    request_id: String,
    /// The asking call's tool and arguments, when its `tool_call_requested`
    /// was seen.
    call: Option<(String, String)>,
    reversible: bool,
    step: AskStep,
    /// What the person typed, kept while the request waits.
    feedback: String,
    cursor: Choice,
    /// Put aside with Esc and not shown since.
    aside: bool,
    /// The `reply` sent for it; it leaves the visible queue until a
    /// rejection puts it back.
    answered_by: Option<String>,
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
}

/// The approval panel as it draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Panel {
    /// Its lines, before wrapping: header, why, the call, the choices.
    pub(crate) lines: Vec<String>,
    /// Whether it takes the alert tint: the reviewer escalated.
    pub(crate) alert: bool,
}

/// What a key did on the open panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelKey {
    /// The panel took it; nothing to send.
    Handled,
    /// Enter: answer the shown request.
    Answer,
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
}

impl Queue {
    /// The approval panel, while it is open.
    pub(crate) fn panel(&self) -> Option<Panel> {
        let at = self.shown_index()?;
        let request = self.requests.get(at)?;
        let waiting: Vec<usize> = self.waiting_indices().collect();
        let k = waiting
            .iter()
            .position(|index| *index == at)
            .unwrap_or_default()
            .saturating_add(1);
        let mut header = format!(
            "approval · {} · {k} of {}",
            request.session.0,
            waiting.len()
        );
        if !request.reversible {
            header.push_str(" · irreversible");
        }
        let mut lines = vec![header, request.why()];
        if let Some((tool, arguments)) = &request.call {
            lines.push(format!("{tool} {arguments}"));
        }
        for choice in request.choices() {
            let mark = if choice == request.cursor { '›' } else { ' ' };
            lines.push(format!("{mark} {}", request.label(choice)));
        }
        Some(Panel {
            lines,
            alert: matches!(
                request.step,
                AskStep::Review {
                    escalation: Some(_),
                    ..
                }
            ),
        })
    }

    /// The badge line while the panel is closed and requests wait.
    pub(crate) fn badge(&self) -> Option<String> {
        if self.shown.is_some() {
            return None;
        }
        let waiting = self.waiting_indices().count();
        (waiting > 0).then(|| format!("! {waiting} waiting · /approvals or ⌥A"))
    }

    /// Handles a key while the panel is open; `None` when the panel is
    /// closed or the key is not the panel's.
    pub(crate) fn on_key(&mut self, key: &Key) -> Option<PanelKey> {
        let at = self.shown_index()?;
        let request = self.requests.get_mut(at)?;
        match key {
            Key::Char(ch) => {
                request.feedback.push(*ch);
                request.cursor = Choice::Deny;
            }
            Key::Backspace => {
                request.feedback.pop();
                request.cursor = Choice::Deny;
            }
            Key::Up | Key::Down => {
                let choices = request.choices();
                let now = choices
                    .iter()
                    .position(|choice| *choice == request.cursor)
                    .unwrap_or_default();
                let next = if *key == Key::Up {
                    now.saturating_sub(1)
                } else {
                    now.saturating_add(1)
                };
                if let Some(choice) = choices.get(next) {
                    request.cursor = *choice;
                }
            }
            Key::Enter => return Some(PanelKey::Answer),
            Key::Esc => {
                request.aside = true;
                self.show_after(at);
            }
            Key::AltA => {
                let next = (at.saturating_add(1)..self.requests.len())
                    .chain(0..at)
                    .find(|index| self.requests.get(*index).is_some_and(Request::waiting));
                if let Some(next) = next {
                    self.show(next);
                }
            }
            // Tab and Shift+Tab move nothing in the panel; F1 opens the
            // key map over it.
            Key::Tab | Key::BackTab => {}
            Key::PageUp | Key::PageDown | Key::End | Key::CtrlC | Key::F1 | Key::CtrlO => {
                return None;
            }
        }
        Some(PanelKey::Handled)
    }

    /// Answers the shown request with the `reply` command `id`, returning
    /// its line, and moves the panel on. `None` when the panel is closed.
    pub(crate) fn answer(&mut self, id: &str) -> Option<String> {
        let at = self.shown_index()?;
        let request = self.requests.get_mut(at)?;
        let reply = Reply {
            request_id: RequestId(request.request_id.clone()),
            answer: request.answer(),
        };
        let args = serde_json::to_value(reply).ok();
        let line = session_command(id, "reply", &request.session, args).to_string();
        request.answered_by = Some(id.to_owned());
        self.show_after(at);
        Some(line)
    }

    /// The `reply` command `id` failed: its request, if it is still
    /// pending, is back where it was, unanswered.
    pub(crate) fn restore(&mut self, id: &str) {
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
                let key = (session.0.clone(), asked.request_id.0.clone());
                if self.requests.iter().any(|request| request.key() == key) {
                    return;
                }
                let call =
                    action.and_then(|action| self.calls.remove(&(session.0.clone(), action)));
                self.requests.push(Request {
                    session: session.clone(),
                    request_id: asked.request_id.0,
                    call,
                    reversible: asked.declared.reversible,
                    step: asked.step,
                    feedback: String::new(),
                    cursor: Choice::Once,
                    aside: false,
                    answered_by: None,
                });
                self.surface(self.requests.len().saturating_sub(1));
            }
            _ => {
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
        }
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
