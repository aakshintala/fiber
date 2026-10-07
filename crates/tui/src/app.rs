//! Terminal state: the draft, the attach phase, the folded stream and the
//! approval queue (`docs/tui.md`, "Turns", "Steering", "Quit", "Approvals
//! and questions").

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use contract::commands::{Remember, RememberScope, Reply, ReplyAnswer};
use contract::events::{
    AskStep, CommandAccepted, CommandRejected, Decision, Escalation, InputItem,
    PermissionRequested, RuleOffer, RuleScope, SteeringApplied, TextCompleted, TextDelta,
    ToolCallRequested, TurnCompleted, TurnOutcome, TurnStarted,
};
use contract::shapes::ContentPart;
use contract::{Envelope, HubLine, RequestId, SessionId};
use serde_json::{Map, Value, json};

use crate::keys::Key;
use crate::link::Line;

/// How long the second Ctrl+C waits for the first.
pub(crate) const QUIT_WINDOW: Duration = Duration::from_secs(1);

/// What the quit hint says.
pub(crate) const QUIT_HINT: &str = "Press Ctrl+C again to quit";

/// The draft that reopens the waiting queue.
const APPROVALS: &str = "/approvals";

/// What the terminal is attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No session yet.
    Starting,
    /// `start` sent, or held until the hub connects; waiting for its answer.
    Pending {
        /// The `start` command's id.
        command_id: String,
    },
    /// Attached to a session.
    Attached {
        /// The session.
        session: SessionId,
        /// Whether a turn is running.
        busy: bool,
    },
}

/// What a key does.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Effect {
    /// Nothing to send.
    None,
    /// Send these command lines to the hub.
    Send(Vec<String>),
    /// Quit the terminal.
    Quit,
}

/// Which command the terminal sent and waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Start,
    Prompt,
    Steer,
    Cancel,
    Reply,
}

/// The hub connection, as the terminal sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Link {
    /// Not connected yet: an Enter is held until `hub_hello`.
    Waiting,
    /// The hub spoke a `hub_hello` this terminal reads.
    Up,
    /// The hub could not be reached, was refused, or hung up. Nothing goes
    /// out again; reconnecting is a later ticket.
    Down,
}

/// One conversation item.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Item {
    /// A turn's input message.
    Prompt(String),
    /// A steering message.
    Steer(String),
    /// One action's reply text, updated as deltas arrive.
    Reply { action: String, text: String },
    /// A closed turn.
    Closed(String),
}

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

/// The terminal's state.
pub(crate) struct App {
    /// The launch directory `start` names.
    workspace: PathBuf,
    draft: String,
    phase: Phase,
    link: Link,
    /// Lines held until the hub connects: the `start` of an early Enter.
    held: Vec<String>,
    /// Commands waiting for their answer: kind and the text they carried.
    pending: HashMap<String, (Kind, String)>,
    /// The one notice line, the latest.
    notice: Option<String>,
    items: Vec<Item>,
    /// The top wrapped row while scrolled up; `None` follows new output.
    top: Option<usize>,
    /// New output arrived while scrolled up.
    has_new: bool,
    width: u16,
    height: u16,
    /// The first Ctrl+C, waiting for the second. Its hint shows while set.
    armed_at: Option<Instant>,
    /// Whether detection saw kitty's keyboard flags.
    kitty: bool,
    /// Approval requests in the order they arrived, from any session.
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

impl App {
    /// An empty app in `workspace`, sized 80x24 until the loop sets it.
    pub(crate) fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            draft: String::new(),
            phase: Phase::Starting,
            link: Link::Waiting,
            held: Vec::new(),
            pending: HashMap::new(),
            notice: None,
            items: Vec::new(),
            top: None,
            has_new: false,
            width: 80,
            height: 24,
            armed_at: None,
            kitty: false,
            requests: Vec::new(),
            shown: None,
            calls: HashMap::new(),
        }
    }

    /// Attaches straight to `session`. The `draw` jig uses it: an events
    /// file is one session's stream.
    pub(crate) fn attach(&mut self, session: SessionId) {
        self.phase = Phase::Attached {
            session,
            busy: false,
        };
    }

    /// Handles one key at `now`, read from the injected clock.
    pub(crate) fn on_key(&mut self, key: Key, now: Instant) -> Effect {
        if key == Key::CtrlC {
            return self.on_ctrl_c(now);
        }
        self.armed_at = None;
        if let Some(effect) = self.on_panel_key(&key) {
            return effect;
        }
        match key {
            Key::Char(ch) => {
                self.draft.push(ch);
                Effect::None
            }
            Key::Backspace => {
                self.draft.pop();
                Effect::None
            }
            Key::Enter => self.on_enter(),
            Key::Esc => self.on_esc(),
            Key::PageUp => {
                self.page_up();
                Effect::None
            }
            Key::PageDown => {
                self.page_down();
                Effect::None
            }
            Key::End | Key::CtrlC => {
                self.follow();
                Effect::None
            }
            Key::Up | Key::Down => Effect::None,
            Key::AltA => self.open_first(),
        }
    }

    /// Folds one line from the hub, returning command lines to send.
    pub(crate) fn on_line(&mut self, line: Line) -> Vec<String> {
        match line {
            Line::Hub(hub) => self.on_hub(&hub),
            Line::Session(envelope) => {
                self.on_session(&envelope);
                Vec::new()
            }
        }
    }

    /// The hub could not be reached, or runs a schema this terminal cannot
    /// read: the notice, and a held `start` fails as if rejected.
    pub(crate) fn connect_failed(&mut self, notice: String) {
        self.link = Link::Down;
        self.notice = Some(notice);
        self.held.clear();
        if let Phase::Pending { command_id } = &self.phase {
            let id = command_id.clone();
            self.fail(&id);
        }
    }

    /// The hub connection ended. Reconnecting is a later ticket. A
    /// connection never connected, refused for its schema version, keeps
    /// the notice that says why.
    pub(crate) fn disconnected(&mut self) {
        if self.link == Link::Up {
            self.link = Link::Down;
            self.notice = Some("Connection lost.".to_owned());
        }
    }

    /// Writing `unsent`, command lines this app made, to the hub failed:
    /// the connection is lost, and their commands fail as if rejected, so
    /// a draft they carried returns to an empty draft.
    pub(crate) fn write_failed(&mut self, unsent: &[String]) {
        self.disconnected();
        for line in unsent {
            if let Some(id) = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
            {
                self.fail(&id);
            }
        }
    }

    /// Whether the hub spoke a `hub_hello` this terminal reads, and the
    /// connection has not ended since.
    pub(crate) fn connected(&self) -> bool {
        self.link == Link::Up
    }

    /// The attached session, if any.
    pub(crate) fn session(&self) -> Option<&SessionId> {
        match &self.phase {
            Phase::Attached { session, .. } => Some(session),
            Phase::Starting | Phase::Pending { .. } => None,
        }
    }

    /// Sets the screen size for wrapping and paging.
    pub(crate) fn set_size(&mut self, width: u16, height: u16) {
        self.width = width.max(1);
        self.height = height.max(1);
    }

    /// Records kitty's keyboard flags reply. No binding needs it yet.
    pub(crate) fn set_kitty(&mut self) {
        self.kitty = true;
    }

    /// Whether detection saw kitty's keyboard flags.
    #[cfg(test)]
    pub(crate) fn kitty(&self) -> bool {
        self.kitty
    }

    /// The draft in the input box.
    pub(crate) fn draft(&self) -> &str {
        &self.draft
    }

    /// The notice line, if any.
    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Whether the quit hint shows: armed by a first Ctrl+C.
    pub(crate) fn hint(&self) -> bool {
        self.armed_at.is_some()
    }

    /// Whether new output arrived while scrolled up.
    pub(crate) fn has_new(&self) -> bool {
        self.has_new
    }

    /// The top wrapped row while scrolled up; `None` follows.
    pub(crate) fn top(&self) -> Option<usize> {
        self.top
    }

    /// The conversation's rows: the screen less the input line or the
    /// panel in its place, the badge, the hint and the notice. None on a
    /// screen too short for them.
    pub(crate) fn conversation_height(&self) -> usize {
        let input = self.panel().map_or(1, |panel| {
            panel
                .lines
                .iter()
                .map(|line| crate::view::rows(line, self.width))
                .sum()
        });
        let below = input
            + usize::from(self.badge().is_some())
            + usize::from(self.hint())
            + usize::from(self.notice.is_some());
        usize::from(self.height).saturating_sub(below)
    }

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

    /// The conversation as plain lines, before wrapping.
    pub(crate) fn lines(&self) -> Vec<String> {
        self.items
            .iter()
            .map(|item| match item {
                Item::Prompt(text) => format!("› {text}"),
                Item::Steer(text) => format!("steer · {text}"),
                Item::Reply { text, .. } | Item::Closed(text) => text.clone(),
            })
            .collect()
    }

    /// Ctrl+C clears, then quits: a second press before [`QUIT_WINDOW`]
    /// has passed since the first quits; a later one re-arms.
    fn on_ctrl_c(&mut self, now: Instant) -> Effect {
        if !self.draft.is_empty() {
            self.draft.clear();
            self.armed_at = None;
            return Effect::None;
        }
        let quits = self
            .armed_at
            .and_then(|armed| armed.checked_add(QUIT_WINDOW))
            .is_some_and(|end| now < end);
        if quits {
            return Effect::Quit;
        }
        self.armed_at = Some(now);
        Effect::None
    }

    /// Enter sends the draft: `start` with no session, `prompt` when idle,
    /// `steer` during a turn.
    fn on_enter(&mut self) -> Effect {
        if self.draft.trim() == APPROVALS {
            self.draft.clear();
            return self.open_first();
        }
        // A command sent after the connection is lost goes nowhere, so the
        // draft stays.
        if self.draft.trim().is_empty() || self.link == Link::Down {
            return Effect::None;
        }
        let (kind, session) = match &self.phase {
            Phase::Starting => (Kind::Start, None),
            Phase::Pending { .. } => return Effect::None,
            Phase::Attached { session, busy } => {
                let kind = if *busy { Kind::Steer } else { Kind::Prompt };
                (kind, Some(session.clone()))
            }
        };
        let id = mint();
        let text = std::mem::take(&mut self.draft);
        let content = json!([{"type": "text", "text": text}]);
        let line = match session {
            None => {
                self.phase = Phase::Pending {
                    command_id: id.clone(),
                };
                json!({
                    "id": id,
                    "command": "start",
                    "args": {
                        "workspace": self.workspace.display().to_string(),
                        "content": content,
                    },
                })
            }
            Some(session) => {
                let command = if kind == Kind::Steer {
                    "steer"
                } else {
                    "prompt"
                };
                session_command(&id, command, &session, Some(json!({ "content": content })))
            }
        };
        self.pending.insert(id, (kind, text));
        let line = line.to_string();
        if self.link == Link::Up {
            Effect::Send(vec![line])
        } else {
            // An Enter before the hub connects is held, not lost: `start`
            // goes out once the hub speaks `hub_hello`.
            self.held.push(line);
            Effect::None
        }
    }

    /// Esc with nothing open interrupts the turn: `cancel`, only when busy.
    fn on_esc(&mut self) -> Effect {
        let session = match &self.phase {
            Phase::Attached {
                session,
                busy: true,
            } if self.connected() => session.clone(),
            Phase::Starting | Phase::Pending { .. } | Phase::Attached { .. } => {
                return Effect::None;
            }
        };
        let id = mint();
        let line = session_command(&id, "cancel", &session, None).to_string();
        self.pending.insert(id, (Kind::Cancel, String::new()));
        Effect::Send(vec![line])
    }

    fn on_hub(&mut self, hub: &HubLine) -> Vec<String> {
        let command_id = hub_string(&hub.payload, "command_id");
        match hub.kind.as_str() {
            "hub_hello" if hub.schema_version == contract::SCHEMA_VERSION => {
                self.link = Link::Up;
                std::mem::take(&mut self.held)
            }
            "hub_hello" => {
                self.connect_failed(format!(
                    "The hub runs schema version {}; this terminal reads {}.",
                    hub.schema_version,
                    contract::SCHEMA_VERSION
                ));
                Vec::new()
            }
            "command_accepted" => {
                let Some(id) = command_id else {
                    return Vec::new();
                };
                let session = hub
                    .payload
                    .get("result")
                    .and_then(|result| result.get("session_id"))
                    .and_then(Value::as_str)
                    .map(|id| SessionId(id.to_owned()));
                match (self.pending.remove(&id), session) {
                    (Some((Kind::Start, _)), Some(session)) => self.started(session),
                    _ => Vec::new(),
                }
            }
            "command_rejected" => {
                if let Some(id) = command_id {
                    let message = hub_string(&hub.payload, "message").unwrap_or_default();
                    self.rejected(&id, message);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// `start` was accepted: attach with a `full` connection.
    fn started(&mut self, session: SessionId) -> Vec<String> {
        self.attach(session.clone());
        let args = json!({"level": "full"});
        vec![session_command(&mint(), "subscribe", &session, Some(args)).to_string()]
    }

    /// A command the terminal sent was rejected: one notice line, and its
    /// text back in the draft when the draft is empty. A rejected `cancel`
    /// shows nothing; after a rejected `start` the next Enter tries again.
    fn rejected(&mut self, id: &str, message: String) {
        match self.pending.get(id) {
            None => {}
            Some((Kind::Cancel, _)) => {
                self.pending.remove(id);
            }
            Some((Kind::Start | Kind::Prompt | Kind::Steer | Kind::Reply, _)) => {
                self.notice = Some(message);
                self.fail(id);
            }
        }
    }

    /// Drops a pending command, returning its text to an empty draft.
    fn fail(&mut self, id: &str) {
        let Some((kind, text)) = self.pending.remove(id) else {
            return;
        };
        if self.draft.is_empty() {
            self.draft = text;
        }
        if kind == Kind::Start {
            self.phase = Phase::Starting;
        }
        if kind == Kind::Reply
            && let Some(at) = self
                .requests
                .iter()
                .position(|request| request.answered_by.as_deref() == Some(id))
        {
            // Back where it was, unanswered.
            if let Some(request) = self.requests.get_mut(at) {
                request.answered_by = None;
            }
            self.surface(at);
        }
    }

    /// Handles a key while the panel is open; `None` when the panel is
    /// closed or the key is not the panel's.
    fn on_panel_key(&mut self, key: &Key) -> Option<Effect> {
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
            Key::Enter => return Some(self.answer(at)),
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
            Key::PageUp | Key::PageDown | Key::End | Key::CtrlC => return None,
        }
        Some(Effect::None)
    }

    /// Sends the shown request's answer and moves the panel on. With the
    /// link down nothing goes out and the request stays.
    fn answer(&mut self, at: usize) -> Effect {
        if self.link != Link::Up {
            return Effect::None;
        }
        let Some(request) = self.requests.get_mut(at) else {
            return Effect::None;
        };
        let id = mint();
        let reply = Reply {
            request_id: RequestId(request.request_id.clone()),
            answer: request.answer(),
        };
        let args = serde_json::to_value(reply).ok();
        let line = session_command(&id, "reply", &request.session, args).to_string();
        request.answered_by = Some(id.clone());
        self.pending.insert(id, (Kind::Reply, String::new()));
        self.show_after(at);
        Effect::Send(vec![line])
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

    /// `/approvals` and Alt+A with the panel closed: the panel opens at the
    /// first request waiting.
    fn open_first(&mut self) -> Effect {
        let first = self.waiting_indices().next();
        match first {
            Some(first) => self.show(first),
            None => self.notice = Some("No requests waiting.".to_owned()),
        }
        Effect::None
    }

    /// Folds the lines the queue reads, from any session: the call, the
    /// request and its resolution.
    fn on_permission(&mut self, envelope: &Envelope) {
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
                let Some(request_id) = hub_string(&envelope.payload, "request_id") else {
                    return;
                };
                let key = (session.0.clone(), request_id);
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

    fn on_session(&mut self, envelope: &Envelope) {
        // The queue takes every session's requests; everything else is the
        // attached session's alone.
        if matches!(
            envelope.kind.as_str(),
            "tool_call_requested" | "permission_requested" | "permission_resolved"
        ) {
            self.on_permission(envelope);
            return;
        }
        if self.session() != Some(&envelope.session_id) {
            return;
        }
        let payload = Value::Object(envelope.payload.clone());
        let action = envelope.action_id.as_ref().map(|id| id.0.as_str());
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Ok(accepted) = serde_json::from_value::<CommandAccepted>(payload) {
                    self.pending.remove(&accepted.command_id.0);
                }
            }
            "command_rejected" => {
                if let Ok(rejected) = serde_json::from_value::<CommandRejected>(payload)
                    && let Some(id) = rejected.command_id
                {
                    self.rejected(&id.0, rejected.message);
                }
            }
            "turn_started" => {
                if let Ok(started) = serde_json::from_value::<TurnStarted>(payload) {
                    self.set_busy(true);
                    for input in &started.input {
                        if let InputItem::Message { content, .. } = input {
                            self.push(Item::Prompt(text_of(content)));
                        }
                    }
                }
            }
            "steering_applied" => {
                if let Ok(applied) = serde_json::from_value::<SteeringApplied>(payload) {
                    self.push(Item::Steer(text_of(&applied.content)));
                }
            }
            "assistant_message_delta" => {
                if let (Some(action), Ok(delta)) =
                    (action, serde_json::from_value::<TextDelta>(payload))
                {
                    self.reply(action, |text| text.push_str(&delta.text));
                }
            }
            "text_completed" => {
                if let (Some(action), Ok(done)) =
                    (action, serde_json::from_value::<TextCompleted>(payload))
                {
                    self.reply(action, |text| *text = done.text);
                }
            }
            "turn_completed" => {
                if let Ok(done) = serde_json::from_value::<TurnCompleted>(payload) {
                    self.set_busy(false);
                    let line = match (done.outcome, done.error) {
                        (TurnOutcome::Completed, _) => "▣ completed".to_owned(),
                        (TurnOutcome::Interrupted, _) => "▣ interrupted".to_owned(),
                        (TurnOutcome::Failed, Some(error)) => {
                            format!("▣ failed · {}", error.message)
                        }
                        (TurnOutcome::Failed, None) => "▣ failed".to_owned(),
                    };
                    self.push(Item::Closed(line));
                }
            }
            _ => {}
        }
    }

    fn set_busy(&mut self, busy: bool) {
        if let Phase::Attached { busy: flag, .. } = &mut self.phase {
            *flag = busy;
        }
    }

    fn push(&mut self, item: Item) {
        self.items.push(item);
        self.changed();
    }

    /// Updates one action's reply text, starting its item on first text.
    /// The reply being streamed is near the end, so the search runs back.
    fn reply(&mut self, action: &str, update: impl FnOnce(&mut String)) {
        let found = self.items.iter_mut().rev().find_map(|item| match item {
            Item::Reply { action: has, text } if has == action => Some(text),
            Item::Prompt(_) | Item::Steer(_) | Item::Reply { .. } | Item::Closed(_) => None,
        });
        match found {
            Some(text) => update(text),
            None => {
                let mut text = String::new();
                update(&mut text);
                self.items.push(Item::Reply {
                    action: action.to_owned(),
                    text,
                });
            }
        }
        self.changed();
    }

    /// New output while scrolled up shows the overlay; the view stays put.
    fn changed(&mut self) {
        if self.top.is_some() {
            self.has_new = true;
        }
    }

    /// The top row when following: the last screenful.
    fn bottom_top(&self) -> usize {
        let total: usize = self
            .lines()
            .iter()
            .map(|line| crate::view::rows(line, self.width))
            .sum();
        total.saturating_sub(self.conversation_height())
    }

    /// PageUp moves up by the conversation height less one.
    fn page_up(&mut self) {
        let step = self.conversation_height().saturating_sub(1).max(1);
        let top = self.top.unwrap_or_else(|| self.bottom_top());
        self.top = Some(top.saturating_sub(step));
    }

    /// PageDown moves down by the conversation height less one, and follows
    /// again on reaching the bottom.
    fn page_down(&mut self) {
        let Some(top) = self.top else {
            return;
        };
        let step = self.conversation_height().saturating_sub(1).max(1);
        let next = top.saturating_add(step);
        if next >= self.bottom_top() {
            self.follow();
        } else {
            self.top = Some(next);
        }
    }

    /// End jumps to the bottom and resumes following.
    fn follow(&mut self) {
        self.top = None;
        self.has_new = false;
    }
}

/// A new command id from random bytes, as `doors::mint` makes one: `c_`
/// and 16 hex digits. `tui` keeps its own copy because it may not depend on
/// `doors`. `RandomState` seeds its keys from the operating system's
/// randomness.
fn mint() -> String {
    format!("c_{:016x}", RandomState::new().hash_one(()))
}

/// A command for a session: `session_id` beside `id`, `command` and `args`.
fn session_command(id: &str, command: &str, session: &SessionId, args: Option<Value>) -> Value {
    let mut line = json!({"id": id, "command": command, "session_id": session.0});
    if let (Some(args), Some(object)) = (args, line.as_object_mut()) {
        object.insert("args".to_owned(), args);
    }
    line
}

/// The text parts of a message, joined.
fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Unknown => None,
        })
        .collect()
}

fn hub_string(payload: &Map<String, Value>, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
