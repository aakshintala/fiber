//! Terminal state: the draft, the attach phase, the folded stream and the
//! approval queue (`docs/tui.md`, "Turns", "Steering", "Quit", "Approvals
//! and questions").

use std::collections::HashMap;
use std::hash::{BuildHasher, RandomState};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use contract::events::{
    CommandAccepted, CommandRejected, Notice, SessionNamed, ShellCommand, SteeringQueue,
    TurnStarted,
};
use contract::shapes::ContentPart;
use contract::{Envelope, HubLine, Seq, SessionId};
use serde_json::{Map, Value, json};

use crate::approvals::{self, Panel, PanelKey, Queue};
use crate::home::Level;
use crate::input::Draft;
use crate::keys::Key;
use crate::link::Line;
use crate::shell;
#[cfg(test)]
use crate::turn::Row;
use crate::view::Scroll;
use crate::window::Pages;
use notices::Notices;
use steering::Steering;

#[path = "notices.rs"]
mod notices;
#[path = "steering.rs"]
mod steering;

#[path = "app_commands.rs"]
mod commands;
#[path = "copy.rs"]
pub(crate) mod copy;
#[path = "app_focus.rs"]
mod focus;
#[path = "history.rs"]
mod history;
#[path = "app_home.rs"]
mod home;
#[path = "app_mouse.rs"]
mod mouse;

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
    /// Start the `@` panel's search worker on a listing of the workspace's
    /// files, searching for an empty query at the current generation.
    ListFiles,
    /// Search the listed files for `query`, the text after the `@`.
    Search {
        /// The generation the result is tagged with.
        generation: u64,
        /// The query.
        query: String,
    },
    /// Open `text` in the editor; its text goes back to `target`.
    Editor {
        /// Where the edited text goes.
        target: crate::editor::Target,
        /// What the editor opens.
        text: String,
    },
    /// Copy this text to the clipboard.
    Copy(String),
}

/// Which command the terminal sent and waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Start,
    Prompt,
    Steer,
    Cancel,
    Reply,
    SteerDrop,
    Shell,
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

/// What clicking a line, or Enter on it, opens, keyed by an id that stays
/// with the item when a page is folded again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Target {
    /// A tool group's ledger.
    Group(usize),
    /// A call's diff, output or error.
    Call(usize),
    /// A thinking block's text.
    Thought(usize),
    /// The login a failed turn offers.
    Login,
    /// A handoff's note.
    Note(usize),
    /// The jobs a resumed process marked orphaned.
    Orphans(usize),
    /// A reply's `block`th code block's `copy` cells: they copy its code.
    Copy { reply: usize, block: usize },
}

/// The terminal's state.
pub(crate) struct App {
    /// The launch directory `start` names.
    workspace: PathBuf,
    /// Home's state, once `run` sets it; `None` keeps today's screen.
    home: Option<home::Home>,
    draft: Draft,
    phase: Phase,
    link: Link,
    /// Lines held until the hub connects: the `start` of an early Enter.
    held: Vec<String>,
    /// Commands waiting for their answer: kind and the text they carried.
    pending: HashMap<String, (Kind, String)>,
    /// The notices floating over the conversation.
    notices: Notices,
    /// The conversation, paged (`docs/tui.md`, "History and paging").
    pages: Pages,
    scroll: Scroll,
    width: u16,
    height: u16,
    /// The first Ctrl+C, waiting for the second. Its hint shows while set.
    armed_at: Option<Instant>,
    /// Whether detection saw kitty's keyboard flags.
    kitty: bool,
    /// Approval requests from every session.
    queue: Queue,
    /// The attached session's steering queue.
    steering: Steering,
    /// The session's name, from the latest `session_named`.
    name: Option<String>,
    /// The `/` and `@` panels and the key map overlay.
    overlays: commands::Overlays,
    /// Prompt recall and the Ctrl+R panel.
    history: history::History,
    /// "Copied" shows, from a click on `copy` to the next key or click.
    copied: bool,
    /// A whole-turn copy waiting on dropped pages (`docs/tui.md`,
    /// "History and paging").
    pending_turn: Option<crate::turn_text::PendingTurn>,
    /// The focused click target in navigate mode; None while the input
    /// box has focus.
    focus: Option<crate::mouse::TargetId>,
    /// The last frame's click targets: what focus steps through.
    stops: Vec<crate::mouse::Target>,
    /// Where the panel and the rail are drawn.
    regions: crate::focus::Regions,
}

impl App {
    /// An empty app in `workspace`, sized 80x24 until the loop sets it.
    pub(crate) fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            home: None,
            draft: Draft::default(),
            phase: Phase::Starting,
            link: Link::Waiting,
            held: Vec::new(),
            pending: HashMap::new(),
            notices: Notices::default(),
            pages: Pages::new(80),
            scroll: Scroll::default(),
            width: 80,
            height: 24,
            armed_at: None,
            kitty: false,
            queue: Queue::default(),
            steering: Steering::default(),
            name: None,
            overlays: commands::Overlays::default(),
            history: history::History::default(),
            copied: false,
            pending_turn: None,
            focus: None,
            stops: Vec::new(),
            regions: crate::focus::Regions::default(),
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

    /// Hands one key to what is on top: the key map, the approval panel, a
    /// completion panel, then the input box.
    fn route_key(&mut self, key: Key, now: Instant) -> Effect {
        self.copied = false;
        if key == Key::CtrlC {
            return self.on_ctrl_c(now);
        }
        self.armed_at = None;
        if let Some(effect) = self.keymap_key(&key) {
            return effect;
        }
        match self.queue.on_key(&key) {
            Some(PanelKey::Handled) => return Effect::None,
            Some(PanelKey::Answer) => return self.answer(),
            None => {}
        }
        if let Some(effect) = self.history_key(&key) {
            return effect;
        }
        if let Some(effect) = self.focus_key(&key) {
            return effect;
        }
        if let Some(effect) = self.completion_key(&key) {
            return effect;
        }
        if let Some(effect) = self.draft_key(&key) {
            return effect;
        }
        match key {
            Key::Enter => self.on_enter(),
            Key::Esc if self.notices.close() => Effect::None,
            // Esc with a queued row selected puts the draft back, and
            // interrupts nothing.
            Key::Esc if self.steering.is_selected() => {
                self.steering.clear(&mut self.draft);
                Effect::None
            }
            Key::Esc => self.on_esc(),
            Key::PageUp | Key::PageDown => {
                self.page(key == Key::PageUp);
                Effect::None
            }
            Key::CtrlO => {
                self.toggle_ledgers();
                Effect::None
            }
            Key::End | Key::CtrlC => {
                self.scroll.follow();
                Effect::None
            }
            Key::F1 => self.open_keymap(),
            Key::Char(_) | Key::Backspace | Key::Up | Key::Down | Key::Tab => Effect::None,
            Key::BackTab if self.completions().is_none() => self.navigate(),
            Key::BackTab => Effect::None,
            Key::AltA => self.open_first(),
            Key::CtrlR => self.open_search(),
            Key::CtrlG => self.open_in_editor(),
            Key::AltUp | Key::AltDown | Key::AltX => self.steering_key(&key),
        }
    }

    /// Folds one line from the hub, returning command lines to send.
    pub(crate) fn on_line(&mut self, line: Line) -> Vec<String> {
        let mut lines = match self.home_line(&line) {
            Some(consumed) => consumed,
            None => match line {
                Line::Hub(hub) => self.on_hub(&hub),
                Line::Session(envelope) => self.on_session(&envelope),
            },
        };
        lines.extend(self.home_outgoing());
        self.settle();
        lines
    }

    /// The hub could not be reached, or runs a schema this terminal cannot
    /// read: the notice, and a held `start` fails as if rejected.
    pub(crate) fn connect_failed(&mut self, notice: String) {
        self.link = Link::Down;
        self.notices.push(notice);
        self.held.clear();
        if let Phase::Pending { command_id } = &self.phase {
            let id = command_id.clone();
            self.fail(&id);
        }
        self.settle();
    }

    /// The hub connection ended. Reconnecting is a later ticket. A
    /// connection never connected, refused for its schema version, keeps
    /// the notice that says why.
    pub(crate) fn disconnected(&mut self) {
        if self.link == Link::Up {
            self.link = Link::Down;
            self.notices.push("Connection lost.".to_owned());
        }
        self.settle();
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
        self.settle();
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

    /// Sets the screen size for wrapping and paging; a new width re-counts
    /// every page.
    pub(crate) fn set_size(&mut self, width: u16, height: u16) {
        self.width = width.max(1);
        self.height = height.max(1);
        self.pages.set_width(self.width);
        self.settle();
    }

    /// Records kitty's keyboard flags reply.
    pub(crate) fn set_kitty(&mut self) {
        self.kitty = true;
    }

    /// Whether detection saw kitty's keyboard flags.
    pub(crate) fn kitty(&self) -> bool {
        self.kitty
    }

    /// The draft in the input box.
    pub(crate) fn input(&self) -> &Draft {
        &self.draft
    }

    /// The session's name, if it has one.
    #[cfg_attr(not(test), expect(dead_code, reason = "#669 draws it"))]
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The input box's rows: the draft's wrapped rows, at most a third of
    /// the screen and at least one.
    pub(crate) fn input_height(&self) -> usize {
        let cap = usize::from(self.height / 3).max(1);
        self.draft.rows(self.width).len().min(cap)
    }

    /// Whether the quit hint shows: armed by a first Ctrl+C.
    pub(crate) fn hint(&self) -> bool {
        self.armed_at.is_some()
    }

    /// Whether new output arrived while scrolled up.
    pub(crate) fn has_new(&self) -> bool {
        self.scroll.has_new
    }

    /// The top wrapped row while scrolled up; `None` follows.
    pub(crate) fn top(&self) -> Option<usize> {
        self.scroll.top
    }

    /// The conversation's rows: the screen less the input box or the
    /// panel in its place, the steering queue, the badge and the hint. None
    /// on a screen too short for them.
    pub(crate) fn conversation_height(&self) -> usize {
        let input = self.panel().map_or(self.input_height(), |panel| {
            panel
                .lines
                .iter()
                .map(|line| crate::view::rows(ratatui::text::Line::raw(line.as_str()), self.width))
                .sum()
        });
        let below = input
            + self.completion_rows()
            + self.steering().len()
            + usize::from(self.badge().is_some())
            + usize::from(self.hint());
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

    /// The resident conversation's lines, before wrapping.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> Vec<ratatui::text::Line<'static>> {
        self.rows().into_iter().map(|(line, _)| line).collect()
    }

    /// Opens or closes what `target` names.
    pub(crate) fn open(&mut self, target: Target) {
        if self.pages.open(&target) {
            self.scroll.changed();
            self.settle();
        }
    }

    /// The resident pages' lines.
    #[cfg(test)]
    fn rows(&self) -> Vec<Row> {
        self.pages.rows()
    }

    /// The top row shown and every row: what a scroll bar draws.
    pub(crate) fn scroll(&self) -> (usize, usize) {
        (self.view_top(), self.pages.index().total())
    }

    /// The lines drawing rows `[top, top + height)`.
    pub(crate) fn shown(&self, top: usize, height: usize) -> crate::window::Shown {
        self.pages.shown(top, height)
    }

    /// The seq ranges of pages the next frame needs and does not hold.
    pub(crate) fn needs(&self) -> Vec<RangeInclusive<Seq>> {
        self.pages
            .needs(self.view_top(), self.conversation_height())
    }

    /// Folds a fetched range's durable lines into their pages.
    pub(crate) fn load(&mut self, lines: Vec<Envelope>) {
        self.pages.load(&lines);
        self.settle();
    }

    /// Loading `range` failed: its rows stay blank and the notice says why.
    /// A failed page ends a whole-turn copy waiting on dropped pages.
    pub(crate) fn load_failed(&mut self, range: &RangeInclusive<Seq>, message: &str) {
        self.pages.fail(*range.start());
        self.cancel_pending_turn();
        self.notices
            .push(format!("Could not load history: {message}"));
    }

    /// Scrolls so `row` is the top row, as dragging the scroll bar does.
    pub(crate) fn jump(&mut self, row: usize) {
        self.scroll.top = Some(row);
        self.settle();
    }

    /// The pages.
    pub(crate) fn pages(&self) -> &Pages {
        &self.pages
    }

    /// `toggle_ledgers`: closes every ledger when all are open, else opens
    /// them all. Groups made later start the same way.
    fn toggle_ledgers(&mut self) {
        self.pages.toggle_ledgers();
        self.settle();
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
                if let Some(lines) = self.history_answered(&id, hub.payload.get("result")) {
                    return lines;
                }
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
                    if !self.history_rejected(&id, &message) {
                        self.rejected(&id, message);
                    }
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// `start` was accepted: attach with a `full` connection, and ask for
    /// the session's `/` commands. A started session is the attached one.
    fn started(&mut self, session: SessionId) -> Vec<String> {
        self.attach(session.clone());
        vec![
            self.subscribe(&session, Level::Full),
            self.ask_commands(&session),
        ]
    }

    /// A command the terminal sent was rejected: a notice, and its
    /// text back in the draft when the draft is empty. A rejected `cancel`
    /// shows nothing; after a rejected `start` the next Enter tries again.
    fn rejected(&mut self, id: &str, message: String) {
        match self.pending.get(id) {
            None => {}
            // Another client may have dropped or amended the row first.
            Some((Kind::Cancel | Kind::SteerDrop, _)) => {
                self.pending.remove(id);
            }
            Some((
                Kind::Start
                | Kind::Prompt
                | Kind::Steer
                | Kind::Reply
                | Kind::Shell
                | Kind::Command,
                _,
            )) => {
                self.notices.push(message);
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
            self.draft.paste(&text);
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
            self.notices.push("No requests waiting.".to_owned());
        }
        Effect::None
    }

    /// Folds one session line, returning command lines to send.
    fn on_session(&mut self, envelope: &Envelope) -> Vec<String> {
        // The queue takes every session's requests; everything else is the
        // attached session's alone.
        if approvals::KINDS.contains(&envelope.kind.as_str()) {
            self.queue.fold(envelope);
        }
        if self.session() != Some(&envelope.session_id) {
            return Vec::new();
        }
        let mut send = Vec::new();
        if envelope.kind == "turn_started"
            && let Some(started) = read!(envelope, TurnStarted)
        {
            self.history.saw(&envelope.session_id, &started.input);
        }
        let applied = self.pages.apply(envelope);
        let mut changed = applied.changed;
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = read!(envelope, CommandAccepted) {
                    let sent = self.pending.remove(&accepted.command_id.0);
                    self.commands_answered(&accepted);
                    let shell = sent.filter(|(kind, _)| *kind == Kind::Shell);
                    let item = shell.and_then(|(_, text)| shell::answered(&text, accepted.result));
                    changed |= self.pages.add_shell(item);
                }
            }
            "shell_command" => {
                if let Some(ran) = read!(envelope, ShellCommand) {
                    changed |= self.pages.add_shell(Some(shell::ran(&ran)));
                }
            }
            "command_rejected" => {
                if let Some(rejected) = read!(envelope, CommandRejected)
                    && let Some(id) = rejected.command_id
                {
                    self.rejected(&id.0, rejected.message);
                }
            }
            "reloaded" => {
                if self.link == Link::Up {
                    send.push(self.ask_commands(&envelope.session_id));
                }
            }
            "session_named" => {
                if let Some(named) = read!(envelope, SessionNamed) {
                    self.name = named.name;
                }
            }
            "notice" => {
                if let Some(notice) = read!(envelope, Notice) {
                    self.notices.push(notice.message);
                }
            }
            "steering_queue" => {
                if let Some(queue) = read!(envelope, SteeringQueue) {
                    self.steering.fold(&queue, &mut self.draft);
                }
            }
            _ => {}
        }
        if let Some(busy) = applied.busy {
            self.set_busy(busy);
        }
        if changed {
            self.scroll.changed();
        }
        send
    }

    fn set_busy(&mut self, busy: bool) {
        if let Phase::Attached { busy: flag, .. } = &mut self.phase {
            *flag = busy;
        }
    }

    /// The top row when following: the last screenful.
    fn bottom_top(&self) -> usize {
        let total = self.pages.index().total();
        total.saturating_sub(self.conversation_height())
    }

    /// The top row shown: the bottom while following, and never past it.
    fn view_top(&self) -> usize {
        let bottom = self.bottom_top();
        self.scroll.top.map_or(bottom, |top| top.min(bottom))
    }

    /// Clamps the top to the bottom and drops the pages outside the window.
    fn settle_pages(&mut self) {
        if self.scroll.top.is_some() {
            self.scroll.top = Some(self.view_top());
        }
        self.pages.trim(self.view_top(), self.conversation_height());
    }

    /// PageUp and PageDown move by the conversation height less one.
    fn page(&mut self, up: bool) {
        let step = self.conversation_height().saturating_sub(1).max(1);
        let bottom = self.bottom_top();
        if up {
            self.scroll.up(step, bottom);
        } else {
            self.scroll.down(step, bottom);
        }
        self.settle();
    }
}

/// A new command id from random bytes, as `doors::mint` makes one: `c_`
/// and 16 hex digits. `tui` keeps its own copy because it may not depend on
/// `doors`. `RandomState` seeds its keys from the operating system's
/// randomness.
pub(crate) fn mint() -> String {
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
pub(crate) fn text_of(parts: &[ContentPart]) -> String {
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
