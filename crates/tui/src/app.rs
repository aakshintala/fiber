//! Terminal state: the draft, the attach phase, the folded stream, the
//! approval queue and the repository offer (`docs/tui.md`, "Turns",
//! "Steering", "Quit", "Approvals and questions", "Approving what a
//! repository ships").

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
use jiff::tz::TimeZone;
use serde_json::{Map, Value, json};

use crate::approvals::{self, Panel, PanelKey, Queue};
use crate::home::Level;
use crate::input::Draft;
use crate::keys::Key;
use crate::link::Line;
use crate::shell;
use crate::window::Pages;
use notices::Notices;
use steering::Steering;

#[path = "notices.rs"]
mod notices;
#[path = "steering.rs"]
mod steering;

mod attention;
mod chrome;
#[path = "app_commands.rs"]
mod commands;
#[path = "copy.rs"]
pub(crate) mod copy;
mod find;
#[path = "app_focus.rs"]
mod focus;
mod form;
#[path = "history.rs"]
mod history;
#[path = "app_home.rs"]
mod home;
mod keyboard;
mod links;
#[path = "app_mouse.rs"]
mod mouse;
mod offer;
pub(crate) mod panel;
mod paste;
pub(crate) mod results;
mod screen;
mod select;

use screen::Screen;

pub(crate) use find::{FindBar, Snippet};

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
    /// Send these command lines, then quit.
    Exit(Vec<String>),
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
    /// The search's pause after `generation`'s keystroke passed: start its
    /// scan (`docs/tui.md`, "History and paging").
    FindPause {
        /// The generation whose pause passed.
        generation: u64,
        /// How long the pause waited.
        after: Duration,
    },
    /// Open this URL with the link opener (`docs/tui.md`, "Links").
    OpenLink(String),
    /// Read an image from the clipboard for this ticket, off the loop
    /// thread: the worker posts the base64 or the notice to show.
    ReadImage(u64),
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
    /// The one clipboard image read at a time, and its ticket.
    paste: paste::Paste,
    phase: Phase,
    link: Link,
    /// Lines held until the hub connects: the `start` of an early Enter.
    held: Vec<String>,
    /// Commands waiting for their answer: kind and the draft they carried.
    pending: HashMap<String, (Kind, Draft)>,
    /// The notices floating over the conversation.
    notices: Notices,
    /// The conversation's size, scroll and pages (`docs/tui.md`, "History
    /// and paging").
    screen: Screen,
    /// The first Ctrl+C, waiting for the second. Its hint shows while set.
    armed_at: Option<Instant>,
    /// Whether detection saw kitty's keyboard flags.
    kitty: bool,
    /// Approval requests from every session.
    queue: Queue,
    /// The attached session's offer of its repository's code.
    offer: crate::offer::Offer,
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
    /// What the person chose to show: the panel's hide.
    chrome: chrome::Chrome,
    /// The attached session's folded panel data (`docs/tui.md`, "The panel").
    panel_state: panel::PanelState,
    /// The drag selecting conversation text, and a copy waiting on dropped
    /// pages (`docs/tui.md`, "Selection and copy").
    select: select::Selection,
    /// Conversation search (`docs/tui.md`, "Search").
    find: find::Find,
    /// The effective bindings (`docs/tui.md`, "Bindings").
    keyboard: keyboard::Keyboard,
    /// Whether a link opener is on `PATH` (`docs/tui.md`, "Links").
    opener: bool,
    /// What the hub's `attention` lines queued (`docs/tui.md`, "Getting
    /// the person's attention").
    attention: attention::State,
}

impl App {
    /// An empty app in `workspace`, sized 80x24 until the loop sets it.
    pub(crate) fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            home: None,
            draft: Draft::default(),
            paste: paste::Paste::default(),
            phase: Phase::Starting,
            link: Link::Waiting,
            held: Vec::new(),
            pending: HashMap::new(),
            notices: Notices::default(),
            screen: Screen::new(),
            armed_at: None,
            kitty: false,
            queue: Queue::default(),
            offer: crate::offer::Offer::default(),
            steering: Steering::default(),
            name: None,
            overlays: commands::Overlays::default(),
            history: history::History::default(),
            copied: false,
            pending_turn: None,
            focus: None,
            stops: Vec::new(),
            regions: crate::focus::Regions::default(),
            chrome: chrome::Chrome::default(),
            panel_state: panel::PanelState::default(),
            select: select::Selection::default(),
            find: find::Find::default(),
            keyboard: keyboard::Keyboard::default(),
            opener: false,
            attention: attention::State::default(),
        }
    }

    /// Attaches straight to `session`. The `draw` jig uses it: an events
    /// file is one session's stream.
    pub(crate) fn attach(&mut self, session: SessionId) {
        self.phase = Phase::Attached {
            session,
            busy: false,
        };
        self.panel_state.attached();
    }

    /// Hands one key to what is on top: the key map, the approval panel, a
    /// completion panel, then the input box.
    fn route_key(&mut self, key: Key, now: Instant) -> Effect {
        self.copied = false;
        if let Some(effect) = self.home_key(&key) {
            return effect;
        }
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
            Some(PanelKey::Decline) => return self.decline(),
            None => {}
        }
        if let Some(effect) = self.offer_key(&key) {
            return effect;
        }
        if let Some(effect) = self.history_key(&key) {
            return effect;
        }
        if let Some(effect) = self.results_key(&key) {
            return effect;
        }
        if let Some(effect) = self.find_key(&key) {
            return effect;
        }
        if let Some(effect) = self.select_key(&key) {
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
                self.screen.follow();
                Effect::None
            }
            Key::F1 => self.open_keymap(),
            Key::Char(_) | Key::Backspace | Key::Up | Key::Down | Key::Tab => Effect::None,
            Key::BackTab if self.completions().is_none() => self.navigate(),
            Key::BackTab => Effect::None,
            Key::AltA => self.open_first(),
            Key::AltP => self.toggle_panel(),
            Key::AltR | Key::AltDigit(_) => Effect::None,
            Key::CtrlR => self.open_search(),
            Key::CtrlV => self.paste.press(self.draft.serial()),
            Key::CtrlF => Effect::None,
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
        lines.extend(self.find_outgoing());
        self.reconcile_attention();
        self.settle();
        lines
    }

    /// Shows `text` as a notice.
    pub(crate) fn push_notice(&mut self, text: String) {
        self.notices.push(text);
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
        self.find_lost();
        self.abandon_copy();
        self.settle();
    }

    /// Writing `unsent`, command lines this app made, to the hub failed:
    /// the connection is lost, and their commands fail as if rejected, so
    /// a draft they carried returns to an empty draft. A close from the
    /// quit question that was never written keeps its resume line.
    pub(crate) fn write_failed(&mut self, unsent: &[String]) {
        self.home_unsent(unsent);
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
        self.screen.set_size(width, height);
        self.settle();
    }

    /// Sets the zone the time of day under a prompt bubble shows
    /// (`docs/tui.md`, "Turns").
    pub(crate) fn set_zone(&mut self, zone: TimeZone) {
        self.screen.pages_mut().zone = zone;
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
    pub(crate) fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The input box's rows: the draft's wrapped rows, at most a third of
    /// the screen and at least one.
    pub(crate) fn input_height(&self) -> usize {
        let cap = usize::from(self.screen.height() / 3).max(1);
        self.draft.rows(self.column_width()).len().min(cap)
    }

    /// Whether the quit hint shows: armed by a first Ctrl+C, or asking
    /// while sessions work.
    pub(crate) fn hint(&self) -> bool {
        self.armed_at.is_some() || self.quit_open()
    }

    /// Whether new output arrived while scrolled up.
    pub(crate) fn has_new(&self) -> bool {
        self.screen.has_new()
    }

    /// The top wrapped row while scrolled up; `None` follows.
    pub(crate) fn top(&self) -> Option<usize> {
        self.screen.top()
    }

    /// The conversation's rows: the screen less the header, the input box
    /// or the panel in its place, the steering queue, the badge and the
    /// hint. None on a screen too short for them.
    pub(crate) fn conversation_height(&self) -> usize {
        let input = self.panel().map_or(self.input_height(), |panel| {
            panel
                .lines
                .iter()
                .map(|line| {
                    crate::view::rows(ratatui::text::Line::raw(line.as_str()), self.column_width())
                })
                .sum()
        });
        let below = self.chrome.header_rows()
            + input
            + self.completion_rows()
            + self.steering().len()
            + usize::from(self.badge().is_some())
            + usize::from(self.hint());
        usize::from(self.screen.height()).saturating_sub(below)
    }

    /// The approval panel, while it is open.
    pub(crate) fn panel(&self) -> Option<Panel> {
        self.queue.panel(self.column_width())
    }

    /// The badge line while the panel is closed and requests wait.
    pub(crate) fn badge(&self) -> Option<String> {
        self.queue.badge(usize::from(self.offer.aside()))
    }

    /// The resident conversation's lines, before wrapping.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> Vec<ratatui::text::Line<'static>> {
        self.screen
            .pages()
            .rows()
            .into_iter()
            .map(|(line, _)| line)
            .collect()
    }

    /// Opens or closes what `target` names. The rows move, so a selection
    /// clears.
    pub(crate) fn open(&mut self, target: Target) {
        if self.screen.open(target) {
            self.clear_selection();
            self.settle();
        }
    }

    /// The top row shown and every row: what a scroll bar draws.
    pub(crate) fn scroll(&self) -> (usize, usize) {
        self.screen.scroll_bar(self.conversation_height())
    }

    /// The lines drawing rows `[top, top + height)`.
    pub(crate) fn shown(&self, top: usize, height: usize) -> crate::window::Shown {
        self.screen.pages().shown(top, height)
    }

    /// The seq ranges of pages the next frame needs and does not hold.
    pub(crate) fn needs(&self) -> Vec<RangeInclusive<Seq>> {
        self.screen.needs(self.conversation_height())
    }

    /// Folds a fetched range's durable lines into their pages.
    pub(crate) fn load(&mut self, lines: Vec<Envelope>) {
        self.screen.pages_mut().load(&lines);
        self.settle();
    }

    /// Loading `range` failed: its rows stay blank and the notice says why.
    /// A failed page ends a whole-turn copy waiting on dropped pages.
    pub(crate) fn load_failed(&mut self, range: &RangeInclusive<Seq>, message: &str) {
        self.screen.pages_mut().fail(*range.start());
        self.cancel_pending_turn();
        self.notices
            .push(format!("Could not load history: {message}"));
    }

    /// Scrolls so `row` is the top row, as dragging the scroll bar does.
    pub(crate) fn jump(&mut self, row: usize) {
        self.screen.jump(row);
        self.settle();
    }

    /// The pages.
    pub(crate) fn pages(&self) -> &Pages {
        self.screen.pages()
    }

    /// `toggle_ledgers`: closes every ledger when all are open, else opens
    /// them all. Groups made later start the same way.
    fn toggle_ledgers(&mut self) {
        self.screen.pages_mut().toggle_ledgers();
        self.clear_selection();
        self.settle();
    }

    /// Ctrl+C clears, then quits: a second press before [`QUIT_WINDOW`]
    /// has passed since the first asks while sessions work, and quits
    /// otherwise; a later one re-arms.
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
            return self.quit();
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
                    (Some((Kind::Start, draft)), Some(session)) => self.started(session, draft),
                    _ => Vec::new(),
                }
            }
            "command_rejected" => {
                if let Some(id) = command_id {
                    let message = hub_string(&hub.payload, "message").unwrap_or_default();
                    if !self.history_rejected(&id, &message) {
                        self.rejected(&id, message);
                    }
                    self.panel_refused(&id)
                } else {
                    Vec::new()
                }
            }
            "attention" => {
                self.attention_line(&hub.payload);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// `start` was accepted: attach with a `full` connection, ask for the
    /// session's `/` commands, then send `draft` as its first prompt. A
    /// started session is the attached one.
    fn started(&mut self, session: SessionId, draft: Draft) -> Vec<String> {
        self.attach(session.clone());
        vec![
            self.subscribe(&session, Level::Full),
            self.ask_commands(&session),
            self.first_prompt(&session, draft),
        ]
    }

    /// A command the terminal sent was rejected: a notice, and its draft
    /// back in the box when the box is empty. A rejected `cancel`
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

    /// Drops a pending command, returning its draft to an empty box.
    fn fail(&mut self, id: &str) {
        let Some((kind, draft)) = self.pending.remove(id) else {
            return;
        };
        if self.draft.is_empty() {
            self.draft.put_back(draft);
        }
        if kind == Kind::Start {
            self.phase = Phase::Starting;
        }
        if kind == Kind::Reply {
            self.queue.restore(id);
            self.offer.restore(id);
        }
    }

    /// `/approvals` and Alt+A with the panel closed: a put-aside offer
    /// opens again, else the panel opens at the first request waiting, or a
    /// notice says none waits.
    fn open_first(&mut self) -> Effect {
        // The offer holds its session before its first request, so it
        // reopens first.
        if self.offer.reopen() {
            return Effect::None;
        }
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
        let mut send = self.reply_ack(envelope);
        if self.session() != Some(&envelope.session_id) {
            return send;
        }
        // The search's fetch answers here, never in the pages: its lines
        // fold into the scan and go no further.
        if let Some(lines) = self.find_answered(envelope) {
            send.extend(lines);
            return send;
        }
        send.extend(self.panel_line(envelope));
        if envelope.kind == "turn_started"
            && let Some(started) = read!(envelope, TurnStarted)
        {
            self.history.saw(&envelope.session_id, &started.input);
        }
        let applied = self.screen.pages_mut().apply(envelope);
        let mut changed = applied.changed;
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = read!(envelope, CommandAccepted) {
                    let sent = self.pending.remove(&accepted.command_id.0);
                    self.commands_answered(&accepted);
                    let shell = sent.filter(|(kind, _)| *kind == Kind::Shell);
                    let item = shell
                        .and_then(|(_, draft)| shell::answered(&draft.expand(), accepted.result));
                    changed |= self.screen.pages_mut().add_shell(item);
                }
            }
            "shell_command" => {
                if let Some(ran) = read!(envelope, ShellCommand) {
                    changed |= self.screen.pages_mut().add_shell(Some(shell::ran(&ran)));
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
            "repository_code_offered" | "repository_code_resolved" => {
                self.offer.fold(envelope);
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
            self.screen.changed();
        }
        send
    }

    fn set_busy(&mut self, busy: bool) {
        if let Phase::Attached { busy: flag, .. } = &mut self.phase {
            *flag = busy;
        }
    }

    /// PageUp and PageDown move by the conversation height less one.
    fn page(&mut self, up: bool) {
        let height = self.conversation_height();
        self.screen.page(up, height);
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
