//! Terminal state: the draft, the attach phase, the folded stream and the
//! approval queue (`docs/tui.md`, "Turns", "Steering", "Quit", "Approvals
//! and questions").

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use contract::events::{
    CommandAccepted, CommandRejected, InputItem, OpeningMessage, SteeringApplied, TurnCompleted,
    TurnStarted, UsageRecorded,
};
use contract::shapes::ContentPart;
use contract::{Envelope, HubLine, SessionId};
use serde_json::{Map, Value, json};

use crate::approvals::{self, Panel, PanelKey, Queue};
use crate::keys::Key;
use crate::link::Line;
use crate::slash;
use crate::turn::{Fold, Row, Turn};

#[path = "app_commands.rs"]
mod commands;

/// A line's payload as `$kind`; `None` when it does not parse, and the
/// line is skipped.
macro_rules! read {
    ($envelope:expr, $kind:ty) => {
        serde_json::from_value::<$kind>(serde_json::Value::Object($envelope.payload.clone())).ok()
    };
}
pub(crate) use read;

/// How long the second Ctrl+C waits for the first.
pub(crate) const QUIT_WINDOW: Duration = Duration::from_secs(1);

/// What the quit hint says.
pub(crate) const QUIT_HINT: &str = "Press Ctrl+C again to quit";

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
    /// A built-in command such as `handoff`, `reload` or `close`.
    Command,
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

/// What clicking a line, or Enter on it, opens. Ids are unique within the
/// app, assigned in fold order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// A tool group's ledger.
    Group(usize),
    /// A call's diff, output or error.
    Call(usize),
    /// A thinking block's text.
    Thought(usize),
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
    turns: Vec<Turn>,
    fold: Fold,
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
    /// Approval requests from every session.
    queue: Queue,
    /// The `/` list: built-in commands, then the attached session's skills.
    slash_rows: Vec<slash::Row>,
    /// Esc closed the `/` panel for this draft.
    slash_closed: bool,
    /// The selected row of the open completion panel.
    selected: usize,
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
            turns: Vec::new(),
            fold: Fold::default(),
            top: None,
            has_new: false,
            width: 80,
            height: 24,
            armed_at: None,
            kitty: false,
            queue: Queue::default(),
            slash_rows: slash::rows(&[]),
            slash_closed: false,
            selected: 0,
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
        let effect = self.route_key(key, now);
        self.edited();
        effect
    }

    /// Hands one key to what is on top: the approval panel, a
    /// completion panel, then the input box.
    fn route_key(&mut self, key: Key, now: Instant) -> Effect {
        if key == Key::CtrlC {
            return self.on_ctrl_c(now);
        }
        self.armed_at = None;
        match self.queue.on_key(&key) {
            Some(PanelKey::Handled) => return Effect::None,
            Some(PanelKey::Answer) => return self.answer(),
            None => {}
        }
        if let Some(effect) = self.completion_key(&key) {
            return effect;
        }
        match key {
            Key::Char(ch) => self.type_char(ch),
            Key::Backspace => self.backspace(),
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
            Key::CtrlO => {
                self.toggle_ledgers();
                Effect::None
            }
            Key::End | Key::CtrlC => {
                self.follow();
                Effect::None
            }
            Key::Up | Key::Down | Key::Tab | Key::BackTab | Key::F1 => Effect::None,
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
        let input = self.panel().map_or_else(
            || {
                1usize.saturating_add(
                    self.completions()
                        .map_or(0, |completions| completions.lines.len()),
                )
            },
            |panel| {
                panel
                    .lines
                    .iter()
                    .map(|line| {
                        crate::view::rows(ratatui::text::Line::raw(line.as_str()), self.width)
                    })
                    .sum()
            },
        );
        let below = input
            + usize::from(self.badge().is_some())
            + usize::from(self.hint())
            + usize::from(self.notice.is_some());
        usize::from(self.height).saturating_sub(below)
    }

    /// The approval panel, while it is open.
    pub(crate) fn panel(&self) -> Option<Panel> {
        self.queue.panel()
    }

    /// The badge line while the panel is closed and requests wait.
    pub(crate) fn badge(&self) -> Option<String> {
        self.queue.badge()
    }

    /// The conversation's lines, before wrapping.
    pub(crate) fn lines(&self) -> Vec<ratatui::text::Line<'static>> {
        self.rows().into_iter().map(|(line, _)| line).collect()
    }

    /// For each line of [`Self::lines`] that opens something, its index and
    /// what it opens.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "#682 clicks and #683 focus call it")
    )]
    pub(crate) fn targets(&self) -> Vec<(usize, Target)> {
        self.rows()
            .into_iter()
            .enumerate()
            .filter_map(|(at, (_, target))| target.map(|target| (at, target)))
            .collect()
    }

    /// Opens or closes what `target` names.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "#682 clicks and #683 focus call it")
    )]
    pub(crate) fn open(&mut self, target: Target) {
        if self.turns.iter_mut().any(|turn| turn.toggle(target)) {
            self.changed();
        }
    }

    fn rows(&self) -> Vec<Row> {
        let mut out = Vec::new();
        for turn in &self.turns {
            turn.rows(self.width, &mut out);
        }
        out
    }

    /// `toggle_ledgers`: closes every ledger when all are open, else opens
    /// them all. Groups made later start the same way.
    fn toggle_ledgers(&mut self) {
        let mut ledgers: Vec<_> = self
            .turns
            .iter_mut()
            .flat_map(Turn::groups_mut)
            .filter(|group| group.has_calls())
            .collect();
        let open = if ledgers.is_empty() {
            !self.fold.ledgers
        } else {
            !ledgers.iter().all(|group| group.open)
        };
        for group in &mut ledgers {
            group.open = open;
        }
        self.fold.ledgers = open;
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
        if let Some(effect) = self.built_in() {
            return effect;
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
            Some((Kind::Start | Kind::Prompt | Kind::Steer | Kind::Reply | Kind::Command, _)) => {
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
        if kind == Kind::Reply {
            self.queue.restore(id);
        }
    }

    /// Sends the shown request's answer. With the link down nothing goes
    /// out and the request stays.
    fn answer(&mut self) -> Effect {
        if self.link != Link::Up {
            return Effect::None;
        }
        let id = mint();
        let Some(line) = self.queue.answer(&id) else {
            return Effect::None;
        };
        self.pending.insert(id, (Kind::Reply, String::new()));
        Effect::Send(vec![line])
    }

    /// `/approvals` and Alt+A with the panel closed: the panel opens at the
    /// first request waiting, or a notice says none waits.
    fn open_first(&mut self) -> Effect {
        if !self.queue.open_first() {
            self.notice = Some("No requests waiting.".to_owned());
        }
        Effect::None
    }

    fn on_session(&mut self, envelope: &Envelope) {
        // The queue takes every session's requests; everything else is the
        // attached session's alone.
        if approvals::KINDS.contains(&envelope.kind.as_str()) {
            self.queue.fold(envelope);
        }
        if self.session() != Some(&envelope.session_id) {
            return;
        }
        let action = envelope.action_id.as_ref().map(|id| id.0.as_str());
        let ts = envelope.ts;
        // Only a line that changed a card shows the overlay.
        let changed = match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = read!(envelope, CommandAccepted) {
                    self.pending.remove(&accepted.command_id.0);
                }
                false
            }
            "command_rejected" => {
                if let Some(rejected) = read!(envelope, CommandRejected)
                    && let Some(id) = rejected.command_id
                {
                    self.rejected(&id.0, rejected.message);
                }
                false
            }
            "opening_message" => {
                if let Some(opening) = read!(envelope, OpeningMessage) {
                    self.slash_rows = slash::rows(&opening.skills);
                }
                false
            }
            "turn_started" => read!(envelope, TurnStarted).is_some_and(|started| {
                self.set_busy(true);
                let prompts = started
                    .input
                    .iter()
                    .filter_map(|input| {
                        if let InputItem::Message { content, .. } = input {
                            Some(text_of(content))
                        } else {
                            None
                        }
                    })
                    .collect();
                self.turns.push(Turn::new(prompts, ts));
                true
            }),
            "turn_completed" => read!(envelope, TurnCompleted).is_some_and(|done| {
                self.set_busy(false);
                self.open_turn().is_some_and(|turn| {
                    turn.complete(done, ts);
                    true
                })
            }),
            "usage_recorded" => read!(envelope, UsageRecorded).is_some_and(|line| {
                // A line folds where its generation already is, so a late
                // correction updates a closed card; else into the open turn.
                let known = self
                    .turns
                    .iter()
                    .rposition(|turn| turn.spend.holds(&line.generation_id));
                let turn = match known {
                    Some(at) => self.turns.get_mut(at),
                    None => self.open_turn(),
                };
                turn.is_some_and(|turn| {
                    turn.spend.record(&line);
                    true
                })
            }),
            "steering_applied" => read!(envelope, SteeringApplied).is_some_and(|applied| {
                self.open_turn().is_some_and(|turn| {
                    turn.steer(text_of(&applied.content));
                    true
                })
            }),
            "step_started" => {
                if let Some(turn) = self.open_turn() {
                    turn.step_started();
                }
                false
            }
            _ => action.is_some_and(|action| {
                crate::turn::fold_action(&mut self.turns, &mut self.fold, envelope, action)
            }),
        };
        if changed {
            self.changed();
        }
    }

    /// The turn still running, if any.
    fn open_turn(&mut self) -> Option<&mut Turn> {
        self.turns.last_mut().filter(|turn| turn.is_open())
    }

    fn set_busy(&mut self, busy: bool) {
        if let Phase::Attached { busy: flag, .. } = &mut self.phase {
            *flag = busy;
        }
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
            .into_iter()
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
pub(crate) fn session_command(
    id: &str,
    command: &str,
    session: &SessionId,
    args: Option<Value>,
) -> Value {
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
