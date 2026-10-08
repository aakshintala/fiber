//! Home's state on the app: the launch description, whether a `start`
//! went out, and what home draws (`docs/tui.md`, "Home"). The data it
//! draws lives in [`crate::home`]; this module is `App`'s home.

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::layout::Rect;
use serde_json::{Value, json};

use super::{App, Effect, Kind, Link, Phase, mint, session_command};
use crate::focus::{Area, order};
use crate::home::{
    HomeScreen, Launch, Left, Level, Sessions, Spot, State, Subs, cascade_line, delete_line,
    dependents, from_status, line, opening, recent_rows, toggle_line,
};
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use crate::view::max_question_scroll;
use contract::SessionId;

#[path = "app_exit.rs"]
mod exit;

/// Home's state: the launch description, and whether a `start` went out in
/// this run, which hides the input box's placeholder.
pub(super) struct Home {
    /// What the terminal knows about where it was launched.
    pub(super) launch: Launch,
    /// A `start` went out in this run.
    prompted: bool,
    /// The session list: live rows from the feed, exited rows from `recent`.
    pub(super) sessions: Sessions,
    /// `feed` and the first `recent` page went out.
    fed: bool,
    /// The `feed` command waiting for its answer.
    feed_id: Option<String>,
    /// The latest `recent` command waiting for its answer, and whether it
    /// asked the first page.
    recent_ask: Option<(String, bool)>,
    /// This connection's subscription level per session: the accepted
    /// one, and every subscribe waiting for its answer.
    subs: Subs,
    /// The session being opened: lines for it fold into its row and are
    /// dropped until the last subscribe's acknowledgement, which is the
    /// first line at the new level.
    opening: Option<Opening>,
    /// Lowering lines reconciliation queued, sent from `on_line`'s tail.
    outbox: Vec<String>,
    /// `/resume` asks the next frame to focus the list.
    focus_list: bool,
    /// A rejected `start`'s message, above the box until the next `start`
    /// goes out.
    blockers: Vec<String>,
    /// The workspace picked for the next `start`, else the launch one.
    chosen: Option<String>,
    /// The workspace picker above the box: its list, fixed at open, and
    /// the selected index.
    picker: Option<(Vec<String>, usize)>,
    /// The foot's question, while one is open: deleting a session.
    prompt: Option<Prompt>,
    /// The stops and deletes waiting for their answers, by command id.
    asks: HashMap<String, Ask>,
    /// The sessions `close` went out for from the quit question, with
    /// their close ids: a close never written keeps its resume line.
    closing: Vec<(SessionId, String)>,
}

/// A stop or delete waiting for its answer: a refusal becomes the row's
/// note and a notice, an accepted delete drops the row.
enum Ask {
    /// A `close` with `now` for a live row: stopping one session.
    Stop(SessionId),
    /// A `delete` for an exited row, cascading when the question named
    /// the sessions it would remove.
    Delete(SessionId),
}

/// The foot's question: deleting an exited session through the hub.
/// Delete is permanent, so the terminal asks first, naming the session
/// and everything `--cascade` would add.
enum Prompt {
    /// Asking to delete `id`: without `expect` the plain question, with
    /// it the cascade question naming the sessions the delete removes.
    /// `scroll` shows later wrapped rows past the screen.
    Delete {
        id: SessionId,
        expect: Option<Vec<SessionId>>,
        scroll: usize,
    },
    /// Asking to quit while sessions work: Enter leaves them running,
    /// `c` closes them all now, Esc stays. It stores nothing: the
    /// question recomputes from the current rows on every frame.
    Quit,
}

impl Prompt {
    /// The session the question asks about; none for quitting.
    fn id(&self) -> Option<&SessionId> {
        match self {
            Prompt::Delete { id, .. } => Some(id),
            Prompt::Quit => None,
        }
    }
}

/// A session opening from home and its last subscribe, whose
/// acknowledgement ends the gate.
struct Opening {
    /// The session opening.
    session: SessionId,
    /// The last subscribe's id: only its rejection fails the open.
    ack: String,
}

impl App {
    /// Stores the launch description as home, keying prompt history by its
    /// project as the run argument did. The app keeps today's screen, so
    /// the jigs and every existing app test stay byte-identical.
    pub(crate) fn set_home(&mut self, launch: Launch) {
        self.set_project(launch.project.clone());
        self.home = Some(Home {
            launch,
            prompted: false,
            sessions: Sessions::default(),
            fed: false,
            feed_id: None,
            recent_ask: None,
            subs: Subs::default(),
            opening: None,
            outbox: Vec::new(),
            focus_list: false,
            blockers: Vec::new(),
            chosen: None,
            picker: None,
            prompt: None,
            asks: HashMap::new(),
            closing: Vec::new(),
        });
    }

    /// Whether home draws: home is set, no session is attached, and neither
    /// the key map nor the approval panel covers the screen.
    pub(crate) fn on_home(&self) -> bool {
        self.home.is_some()
            && self.session().is_none()
            && self.keymap_top().is_none()
            && self.panel().is_none()
    }

    /// Whether the quit question is open, on home or not: while it is,
    /// every key goes to it first.
    pub(crate) fn quit_open(&self) -> bool {
        self.home
            .as_ref()
            .is_some_and(|home| matches!(home.prompt, Some(Prompt::Quit)))
    }

    /// Whether a home modal holds the keyboard: the delete question or
    /// the workspace picker, the two `home_key` branches after the quit
    /// question.
    pub(super) fn home_modal(&self) -> bool {
        self.on_home()
            && self
                .home
                .as_ref()
                .is_some_and(|home| home.prompt.is_some() || home.picker.is_some())
    }

    /// What home draws, or `None` unless [`App::on_home`] holds.
    pub(crate) fn home_screen(&self) -> Option<HomeScreen> {
        let home = self.home.as_ref().filter(|_| self.on_home())?;
        // Scoped is inside git with the toggle off: only the launch
        // project's rows show.
        let scoped = home.launch.git && !home.sessions.show_all();
        // The workspace in use: the picked one, else the launch
        // directory. Clicking its chip opens the workspace picker.
        let workspace = home
            .chosen
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| home.launch.workspace.clone());
        let segment = workspace
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| workspace.display().to_string());
        // The delete question wraps over as many rows as needed in
        // the list's place, naming the session and everything
        // `--cascade` would add.
        let (question, question_scroll) = match &home.prompt {
            Some(Prompt::Delete { id, expect, scroll }) => (
                home.sessions.row(id).map(|row| match expect {
                    None => delete_line(row),
                    Some(expect) => cascade_line(
                        row,
                        &expect
                            .iter()
                            .filter(|session| **session != row.id)
                            .cloned()
                            .collect::<Vec<_>>(),
                    ),
                }),
                *scroll,
            ),
            // The quit question draws through the hint below, recomputed
            // every frame.
            Some(Prompt::Quit) | None => (None, 0),
        };
        let foot = if self.hint() {
            self.hint_text()
        } else {
            "↓ the session list · F1 the key map · Ctrl+C twice to quit".to_owned()
        };
        Some(HomeScreen {
            version: home.launch.version.clone(),
            // A remote client has no launch directory; the picker it would
            // always show is a later ticket's.
            glyph: home.launch.logo_glyph.clone(),
            chips: vec![
                (Some(Spot::Workspace), format!("[{segment}]")),
                (
                    None,
                    format!("[{}]", home.launch.model.as_deref().unwrap_or("no model")),
                ),
                (
                    None,
                    format!(
                        "[thinking: {}]",
                        home.launch.thinking.as_deref().unwrap_or("default")
                    ),
                ),
                (None, "enter starts a session".to_owned()),
            ],
            rows: home
                .sessions
                .shown(&home.launch.project, scoped)
                .iter()
                .map(|row| {
                    // Every readable row ends in a ✕: stopping a live
                    // session, deleting an exited one. An unreadable row
                    // has none.
                    (
                        row.key,
                        line(row, &home.launch.project),
                        row.state != State::Unreadable,
                    )
                })
                .collect(),
            picker: home.picker.clone(),
            blockers: home.blockers.clone(),
            // The toggle shows inside git whenever a row hides, or
            // while everything shows.
            toggle: {
                let (hidden, waiting) = if scoped {
                    home.sessions.hidden(&home.launch.project)
                } else {
                    (0, 0)
                };
                (home.launch.git && (hidden > 0 || home.sessions.show_all()))
                    .then(|| toggle_line(waiting, home.sessions.show_all()))
            },
            foot,
            question,
            question_scroll,
            // Until the first prompt, the placeholder says "/? for
            // shortcuts"; no session exists before Enter, so opening the
            // terminal, glancing at home and leaving creates nothing.
            placeholder: self.draft.is_empty() && !home.prompted,
        })
    }

    /// The hub lines home asks for, once the link is up: `feed`, then the
    /// first `recent` page, naming the launch project while scoped. The
    /// session list fills in when the hub's feed arrives.
    pub(super) fn home_outgoing(&mut self) -> Vec<String> {
        if self.link != Link::Up {
            return Vec::new();
        }
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        if home.fed {
            return std::mem::take(&mut home.outbox);
        }
        home.fed = true;
        let feed = mint();
        let recent = mint();
        home.feed_id = Some(feed.clone());
        home.recent_ask = Some((recent.clone(), true));
        let project = home.launch.project.clone();
        let scoped = home.launch.git && !home.sessions.show_all();
        let recent = recent_line(&recent, None, scoped.then_some(project.as_str()));
        vec![json!({"id": feed, "command": "feed"}).to_string(), recent]
    }

    /// Folds one hub line into the session list, with the lines to send;
    /// `None` means `on_line` goes on as today. A hub `session_left` and
    /// every `session_status` fold into the rows; the answers to `feed`
    /// and the latest `recent` fill the list, and their refusals are one
    /// notice. Every subscribe acknowledgement updates the levels, and
    /// while a session opens its lines fold into its row and are dropped
    /// until the last subscribe's acknowledgement. `attention` and other
    /// hub lines pass through untouched, and a status still reaches the
    /// conversation below.
    pub(super) fn home_line(&mut self, line: &Line) -> Option<Vec<String>> {
        self.home.as_ref()?;
        match line {
            Line::Hub(hub) => match hub.kind.as_str() {
                "session_left" => {
                    let id = hub.payload.get("session_id").and_then(Value::as_str)?;
                    let how = match hub.payload.get("how").and_then(Value::as_str) {
                        Some("exited") => Left::Exited,
                        Some("crashed") => Left::Crashed,
                        _ => return Some(Vec::new()),
                    };
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.left(&SessionId(id.to_owned()), how);
                    }
                    Some(Vec::new())
                }
                "command_accepted" | "command_rejected" => {
                    let id = hub.payload.get("command_id").and_then(Value::as_str)?;
                    let accepted = hub.kind.as_str() == "command_accepted";
                    if self.answered(id, accepted).is_some() {
                        if !accepted && self.is_ack(id) {
                            return Some(self.fail_open(refusal(&hub.payload)));
                        }
                        return Some(Vec::new());
                    }
                    if let Some(ask) = self.take_ask(id) {
                        return Some(self.answer_ask(ask, accepted, &hub.payload));
                    }
                    // A rejected `start` is home's blocker text, above the
                    // box until the next `start` goes out. Reading
                    // `pending` first, it still reaches the rejection
                    // below, staying a notice as today.
                    if !accepted && let Some((Kind::Start, _)) = self.pending.get(id) {
                        if let Some(message) = hub.payload.get("message").and_then(Value::as_str)
                            && let Some(home) = self.home.as_mut()
                        {
                            home.blockers = message.lines().map(str::to_owned).collect();
                        }
                        return None;
                    }
                    let is_feed =
                        self.home.as_ref().and_then(|home| home.feed_id.as_deref()) == Some(id);
                    if is_feed {
                        if let Some(home) = self.home.as_mut() {
                            home.feed_id = None;
                        }
                        if !accepted {
                            self.notices.push(refusal(&hub.payload));
                        }
                        return Some(Vec::new());
                    }
                    let recent = self
                        .home
                        .as_ref()
                        .and_then(|home| home.recent_ask.as_ref())
                        .filter(|(asked, _)| asked == id)
                        .map(|(_, first)| *first);
                    if let Some(first) = recent {
                        if let Some(home) = self.home.as_mut() {
                            home.recent_ask = None;
                        }
                        if accepted {
                            let rows = hub
                                .payload
                                .get("result")
                                .map(recent_rows)
                                .unwrap_or_default();
                            if let Some(home) = self.home.as_mut() {
                                home.sessions.recent(rows, first);
                            }
                        } else {
                            self.notices.push(refusal(&hub.payload));
                        }
                        return Some(Vec::new());
                    }
                    None
                }
                _ => None,
            },
            Line::Session(envelope) => {
                if envelope.kind == "session_status" {
                    let session = envelope.session_id.clone();
                    let was_left = self
                        .home
                        .as_ref()
                        .and_then(|home| home.sessions.row(&session))
                        .is_some_and(|row| row.left.is_some());
                    let row = from_status(envelope);
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.status(row);
                    }
                    if was_left {
                        self.reconcile(&session);
                    }
                    let gated = self
                        .home
                        .as_ref()
                        .and_then(|home| home.opening.as_ref())
                        .is_some_and(|opening| opening.session == session);
                    if gated {
                        return Some(Vec::new());
                    }
                    return None;
                }
                if matches!(
                    envelope.kind.as_str(),
                    "command_accepted" | "command_rejected"
                ) {
                    let accepted = envelope.kind == "command_accepted";
                    let id = envelope.payload.get("command_id").and_then(Value::as_str)?;
                    if self.answered(id, accepted).is_some() {
                        if !accepted && self.is_ack(id) {
                            return Some(self.fail_open(refusal(&envelope.payload)));
                        }
                        let session = envelope.session_id.clone();
                        if accepted
                            && self
                                .home
                                .as_ref()
                                .is_some_and(|home| home.subs.full(&session))
                        {
                            self.reconcile(&session);
                        }
                        if accepted && self.is_ack(id) {
                            if let Some(home) = self.home.as_mut() {
                                home.opening = None;
                            }
                            return None;
                        }
                        return Some(Vec::new());
                    }
                    if let Some(ask) = self.take_ask(id) {
                        return Some(self.answer_ask(ask, accepted, &envelope.payload));
                    }
                }
                let gated = self
                    .home
                    .as_ref()
                    .and_then(|home| home.opening.as_ref())
                    .is_some_and(|opening| opening.session == envelope.session_id);
                if gated {
                    return Some(Vec::new());
                }
                None
            }
        }
    }

    /// A `subscribe` line for `session` at `level`, recorded as sent:
    /// the expected level is the last in-flight one, so no path sends a
    /// subscribe at the level already held or asked for.
    pub(super) fn subscribe(&mut self, session: &SessionId, level: Level) -> String {
        let (id, line) = subscribe_line(session, level);
        if let Some(home) = self.home.as_mut() {
            home.subs.sent(id, session.clone(), level);
        }
        line
    }

    /// Leaves the session on screen for home: the conversation cleared
    /// with the session left running, then lowered to `summary` when this
    /// connection holds it at `full` and its row is live. `/close` keeps
    /// `go_home` and lowers nothing: a command for an exited session
    /// would resume it.
    pub(super) fn leave(&mut self) -> Effect {
        let attached = self.session().cloned();
        self.go_home();
        let Some(session) = attached else {
            return Effect::None;
        };
        let lowers = self.link == Link::Up
            && self.home.as_ref().is_some_and(|home| {
                home.subs.expected(&session) == Some(Level::Full)
                    && home
                        .sessions
                        .row(&session)
                        .is_some_and(|row| row.left.is_none())
            });
        if lowers {
            Effect::Send(vec![self.subscribe(&session, Level::Summary)])
        } else {
            Effect::None
        }
    }

    /// Opens home at the session list: home, with the next frame
    /// focusing the list, or the input box when the list is empty.
    pub(super) fn resume_list(&mut self) -> Effect {
        let effect = self.leave();
        if let Some(home) = self.home.as_mut() {
            home.focus_list = true;
        }
        effect
    }

    /// Focuses the list after `/resume`: the first list stop among the
    /// new targets, leaving focus in the box when the list is empty.
    /// True when it ran, so the frame draws again with the focus shown.
    pub(super) fn home_drawn(&mut self, targets: &[Target]) -> bool {
        let Some(home) = self.home.as_mut() else {
            return false;
        };
        if !home.focus_list {
            return false;
        }
        home.focus_list = false;
        self.focus = order(targets, &self.regions, Area::Conversation)
            .into_iter()
            .find(|id| matches!(id, TargetId::Home(Spot::Entry(_) | Spot::Toggle)));
        true
    }

    /// A key for home, ahead of the key map and focus: the picker's keys
    /// while it is open, then down in an empty box focusing the first
    /// row, and down or j on the last drawn row focusing the next one
    /// below the fold. `None` for anything else, so the focused stops
    /// keep moving as they do on the conversation. Only ↓ enters the
    /// list: j and k move only while it already has focus, and the draft
    /// keeps every printable key typed into it.
    pub(super) fn home_key(&mut self, key: &Key) -> Option<Effect> {
        // The quit question takes every key first, on home or not:
        // Enter leaves working sessions running, `c` closes them all
        // now, Esc stays, and anything else, Ctrl+C included, does
        // nothing.
        if self.quit_open() {
            match key {
                Key::Enter => return Some(Effect::Quit),
                Key::Char('c') => return Some(self.close_all()),
                Key::Esc => {
                    self.close_prompt();
                    return Some(Effect::None);
                }
                Key::Char(_)
                | Key::Backspace
                | Key::Up
                | Key::Down
                | Key::PageUp
                | Key::PageDown
                | Key::End
                | Key::AltA
                | Key::Tab
                | Key::BackTab
                | Key::F1
                | Key::CtrlO
                | Key::CtrlG
                | Key::CtrlR
                | Key::CtrlF
                | Key::CtrlC
                | Key::AltUp
                | Key::AltDown
                | Key::AltX
                | Key::AltP
                | Key::AltR
                | Key::AltDigit(_) => return Some(Effect::None),
            }
        }
        if !self.on_home() {
            return None;
        }
        // The delete question takes every key first: Enter deletes, Esc
        // keeps the session, Up and Down scroll its wrapped rows past
        // the screen, Ctrl+C passes through to quit, and anything else
        // is swallowed.
        if self.home.as_ref().is_some_and(|home| home.prompt.is_some()) {
            match key {
                Key::Enter => return Some(self.send_delete()),
                Key::Esc => {
                    self.close_prompt();
                    return Some(Effect::None);
                }
                Key::Up => {
                    self.scroll_question(false);
                    return Some(Effect::None);
                }
                Key::Down => {
                    self.scroll_question(true);
                    return Some(Effect::None);
                }
                Key::CtrlC => return None,
                Key::Char(_)
                | Key::Backspace
                | Key::PageUp
                | Key::PageDown
                | Key::End
                | Key::AltA
                | Key::Tab
                | Key::BackTab
                | Key::F1
                | Key::CtrlO
                | Key::CtrlG
                | Key::CtrlR
                | Key::CtrlF
                | Key::AltUp
                | Key::AltDown
                | Key::AltX
                | Key::AltP
                | Key::AltR
                | Key::AltDigit(_) => return Some(Effect::None),
            }
        }
        // The picker takes every key first: moving, choosing and closing
        // it, while Ctrl+C passes through and anything else is swallowed
        // so the draft keeps nothing typed into the picker.
        if self.home.as_ref().is_some_and(|home| home.picker.is_some()) {
            match key {
                Key::CtrlC => return None,
                Key::Up => self.move_picker(false),
                Key::Down => self.move_picker(true),
                Key::Enter => self.choose_picker(),
                Key::Esc => self.close_picker(),
                Key::Char(_)
                | Key::Backspace
                | Key::CtrlO
                | Key::PageUp
                | Key::PageDown
                | Key::End
                | Key::AltA
                | Key::Tab
                | Key::BackTab
                | Key::F1
                | Key::CtrlG
                | Key::CtrlR
                | Key::CtrlF
                | Key::AltUp
                | Key::AltDown
                | Key::AltX
                | Key::AltP
                | Key::AltR
                | Key::AltDigit(_) => {}
            }
            return Some(Effect::None);
        }
        // Only ↓ enters the list: with the box focused the draft keeps
        // j, and focus moves it once the list has focus.
        if matches!(key, Key::Char('j')) && self.focus.is_none() {
            return None;
        }
        if !matches!(key, Key::Down | Key::Char('j')) {
            // Backspace on a focused row asks to delete it when it
            // exited; anything else on a focused row is swallowed.
            if matches!(key, Key::Backspace) {
                return self.delete_key();
            }
            return None;
        }
        if self.focus.is_none() {
            if !self.draft.is_empty() || self.completions().is_some() {
                return None;
            }
            // The toggle heads the list while it shows.
            let first = order(&self.stops, &self.regions, Area::Conversation)
                .into_iter()
                .find(|id| matches!(id, TargetId::Home(Spot::Entry(_) | Spot::Toggle)));
            if let Some(id) = first {
                self.focus = Some(id);
                return Some(Effect::None);
            }
            return None;
        }
        let Some(TargetId::Home(Spot::Entry(focused) | Spot::Stop(focused))) = self.focus else {
            return None;
        };
        let ordered = order(&self.stops, &self.regions, Area::Conversation);
        // The last drawn list stop, rows or their crosses: further steps
        // stay while focus is short of it.
        let last = ordered
            .iter()
            .rev()
            .find(|id| matches!(id, TargetId::Home(Spot::Entry(_) | Spot::Stop(_))));
        if last != self.focus.as_ref() {
            return None;
        }
        let next = self.home.as_ref().and_then(|home| {
            let scoped = home.launch.git && !home.sessions.show_all();
            let shown = home.sessions.shown(&home.launch.project, scoped);
            let at = shown.iter().position(|row| row.key == focused)?;
            shown.get(at + 1).map(|row| row.key)
        });
        if let Some(key) = next {
            self.focus = Some(TargetId::Home(Spot::Entry(key)));
            return Some(Effect::None);
        }
        // At the end of the list, ↓ asks the next `recent` page when
        // the focused row is the last recent row, the last answer was
        // not empty, and no `recent` is in flight. Focus stays.
        self.page_recent(focused)
    }

    /// Clicks `spot` on home: a row opens its session, its ✕ stops a
    /// live session or asks to delete an exited one, the toggle flips
    /// the scope, the workspace chip opens the picker, and a picker row
    /// chooses its workspace.
    pub(super) fn home_click(&mut self, spot: Spot) -> Effect {
        match spot {
            Spot::Entry(key) => self.open_row(key),
            Spot::Stop(key) => self.stop_or_ask(key),
            Spot::Toggle => self.toggle_scope(),
            Spot::Workspace => self.open_picker(),
            Spot::Pick(at) => self.pick(at),
        }
    }

    /// A row's ✕: `close` with `now` for a live session, stopping it,
    /// with a `summary` subscribe first when this connection holds
    /// nothing for it; the delete question for an exited row. With the
    /// link down nothing goes out. A refusal of the close becomes the
    /// row's note and a notice.
    fn stop_or_ask(&mut self, key: u64) -> Effect {
        if !self.on_home() || self.link != Link::Up {
            return Effect::None;
        }
        let row = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .cloned();
        let Some(row) = row else {
            return Effect::None;
        };
        if row.state == State::Unreadable {
            self.notices.push(
                "Cannot attach: this session's schema is newer than this terminal reads."
                    .to_owned(),
            );
            return Effect::None;
        }
        if row.left.is_some() {
            self.ask_delete(key);
            return Effect::None;
        }
        let session = row.id.clone();
        let mut lines = Vec::new();
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.subs.expected(&session).is_none())
        {
            lines.push(self.subscribe(&session, Level::Summary));
        }
        let id = mint();
        lines.push(session_command(&id, "close", &session, Some(json!({"now": true}))).to_string());
        if let Some(home) = self.home.as_mut() {
            home.asks.insert(id, Ask::Stop(session));
        }
        Effect::Send(lines)
    }

    /// Opens the delete question for the row with `key`. Delete through
    /// the hub is permanent, so the terminal asks first, naming the
    /// session.
    fn ask_delete(&mut self, key: u64) {
        let id = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .map(|row| row.id.clone());
        if let (Some(home), Some(id)) = (self.home.as_mut(), id) {
            home.prompt = Some(Prompt::Delete {
                id,
                expect: None,
                scroll: 0,
            });
        }
    }

    /// Closes the foot's question, keeping the session.
    fn close_prompt(&mut self) {
        if let Some(home) = self.home.as_mut() {
            home.prompt = None;
        }
    }

    /// Scrolls the delete question one row: Up toward its first rows,
    /// Down toward its later ones, clamped to the wrapped rows past the
    /// screen. Scrolling a short question changes nothing drawn, and
    /// Down past the end holds, so one Up steps back one row.
    fn scroll_question(&mut self, down: bool) {
        let max = self
            .home_screen()
            .map(|screen| {
                max_question_scroll(
                    self,
                    &screen,
                    Rect::new(0, 0, self.screen.width(), self.screen.height()),
                )
            })
            .unwrap_or(0);
        if let Some(home) = self.home.as_mut()
            && let Some(Prompt::Delete { scroll, .. }) = home.prompt.as_mut()
        {
            let clamped = (*scroll).min(max);
            *scroll = if down {
                clamped.saturating_add(1).min(max)
            } else {
                clamped.saturating_sub(1)
            };
        }
    }

    /// Backspace, or Delete through `home_edit`, on a focused row: the
    /// delete question when it exited. On a focused live or unreadable
    /// row it does nothing, and on any other focused stop it is
    /// swallowed; with the box focused the draft keeps the key.
    fn delete_key(&mut self) -> Option<Effect> {
        let key = match self.focus {
            None => return None,
            Some(TargetId::Home(Spot::Entry(key) | Spot::Stop(key))) => key,
            Some(_) => return Some(Effect::None),
        };
        if self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
            .is_some_and(|row| row.left.is_some())
        {
            self.ask_delete(key);
        }
        Some(Effect::None)
    }

    /// Delete on a focused row, ahead of the focus early return in
    /// `on_edit`: the delete question when it exited, swallowed
    /// otherwise. With the box focused the draft keeps the key.
    pub(super) fn home_edit(&mut self, edit: &Edit) -> Option<Effect> {
        if !self.on_home() {
            return None;
        }
        if self.home.as_ref().is_some_and(|home| home.prompt.is_some()) {
            return Some(Effect::None);
        }
        match edit {
            Edit::Delete => self.delete_key(),
            Edit::Left
            | Edit::Right
            | Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Paste(_) => None,
        }
    }

    /// Enter in the delete question: `delete` for its session, cascading
    /// with the confirmed set from the cascade question. With the link
    /// down nothing goes out and the question stays.
    fn send_delete(&mut self) -> Effect {
        let ask = self.home.as_ref().and_then(|home| match &home.prompt {
            Some(Prompt::Delete { id, expect, .. }) => Some((id.clone(), expect.clone())),
            Some(Prompt::Quit) | None => None,
        });
        let Some((id, expect)) = ask else {
            return Effect::None;
        };
        if self.link != Link::Up {
            return Effect::None;
        }
        let command = mint();
        let line = match &expect {
            None => json!({"id": command, "command": "delete", "args": {"session": id.0}}),
            Some(expect) => json!({
                "id": command,
                "command": "delete",
                "args": {
                    "session": id.0,
                    "cascade": true,
                    "expect": expect.iter().map(|session| &session.0).collect::<Vec<_>>(),
                },
            }),
        }
        .to_string();
        if let Some(home) = self.home.as_mut() {
            home.asks.insert(command, Ask::Delete(id));
        }
        Effect::Send(vec![line])
    }

    /// Takes the stop or delete waiting on command `id`, if any.
    fn take_ask(&mut self, id: &str) -> Option<Ask> {
        self.home.as_mut().and_then(|home| home.asks.remove(id))
    }

    /// A refusal shows on the row, in place of its detail.
    fn note(&mut self, id: &SessionId, note: String) {
        if let Some(home) = self.home.as_mut() {
            home.sessions.note(id, note);
        }
    }

    /// Answers a stop or delete. An accepted delete drops the row and
    /// asks the first `recent` page again. A dependents refusal, or a
    /// stale cascade refusal, reopens the cascade question naming the
    /// set the delete would remove now; every other refusal, and one
    /// naming no other session, becomes the row's note and a notice.
    fn answer_ask(
        &mut self,
        ask: Ask,
        accepted: bool,
        payload: &serde_json::Map<String, Value>,
    ) -> Vec<String> {
        match ask {
            Ask::Stop(session) => {
                if !accepted {
                    let message = refusal(payload);
                    self.note(&session, message.clone());
                    self.notices.push(message);
                }
                Vec::new()
            }
            Ask::Delete(id) => {
                if accepted {
                    self.clear_prompt(&id);
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.remove(&id);
                    }
                    return self.ask_recent_first();
                }
                let (code, message) = refusal_parts(payload);
                if code == "session_has_dependents" || code == "stale_request" {
                    // The hub names the root too, so the row goes first
                    // and the set deduplicates keeping first occurrence.
                    let mut expect = vec![id.clone()];
                    for session in dependents(&message) {
                        if !expect.contains(&session) {
                            expect.push(session);
                        }
                    }
                    if expect.len() > 1 {
                        if let Some(home) = self.home.as_mut() {
                            home.prompt = Some(Prompt::Delete {
                                id,
                                expect: Some(expect),
                                scroll: 0,
                            });
                        }
                        return Vec::new();
                    }
                }
                self.note(&id, message.clone());
                self.notices.push(message);
                self.clear_prompt(&id);
                Vec::new()
            }
        }
    }

    /// Closes the foot's question when it asks about `id`: an answer for
    /// an older question leaves a newer one open.
    fn clear_prompt(&mut self, id: &SessionId) {
        if let Some(home) = self.home.as_mut()
            && home
                .prompt
                .as_ref()
                .is_some_and(|prompt| prompt.id() == Some(id))
        {
            home.prompt = None;
        }
    }

    /// Asks the first `recent` page again, naming the launch project
    /// while scoped: an accepted delete fills the gap it left.
    fn ask_recent_first(&mut self) -> Vec<String> {
        if self.link != Link::Up {
            return Vec::new();
        }
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        let project = home.launch.project.clone();
        let scoped = home.launch.git && !home.sessions.show_all();
        let id = mint();
        home.recent_ask = Some((id.clone(), true));
        vec![recent_line(&id, None, scoped.then_some(project.as_str()))]
    }

    /// Flips the scope toggle and asks the first `recent` page again,
    /// naming the launch project while scoped.
    fn toggle_scope(&mut self) -> Effect {
        let Some(home) = self.home.as_mut() else {
            return Effect::None;
        };
        home.sessions.toggle();
        if self.link != Link::Up {
            return Effect::None;
        }
        let project = home.launch.project.clone();
        let scoped = home.launch.git && !home.sessions.show_all();
        let id = mint();
        home.recent_ask = Some((id.clone(), true));
        Effect::Send(vec![recent_line(
            &id,
            None,
            scoped.then_some(project.as_str()),
        )])
    }

    /// Asks the next `recent` page past the focused last row: `before`
    /// its id, naming the launch project while scoped. `None` unless the
    /// focused row ends the list and is its last recent row, the last
    /// answer was not empty, and no `recent` is in flight.
    fn page_recent(&mut self, focused: u64) -> Option<Effect> {
        let home = self.home.as_mut()?;
        let project = home.launch.project.clone();
        let scoped = home.launch.git && !home.sessions.show_all();
        if !home.sessions.more() || home.recent_ask.is_some() {
            return None;
        }
        let before = home
            .sessions
            .last_recent()
            .filter(|last| last.key == focused)?
            .id
            .0
            .clone();
        let id = mint();
        home.recent_ask = Some((id.clone(), false));
        Some(Effect::Send(vec![recent_line(
            &id,
            Some(before.as_str()),
            scoped.then_some(project.as_str()),
        )]))
    }

    /// Opens the workspace picker above the box: the launch directory
    /// first, then each distinct workspace from the feed rows and then
    /// the recent rows, at most ten. Its list is fixed at open.
    fn open_picker(&mut self) -> Effect {
        let Some(home) = self.home.as_mut() else {
            return Effect::None;
        };
        let project = home.launch.project.clone();
        let mut list = vec![home.launch.workspace.display().to_string()];
        for row in home.sessions.shown(&project, false) {
            if list.len() == PICKER_CAP {
                break;
            }
            if !list.contains(&row.workspace) {
                list.push(row.workspace.clone());
            }
        }
        home.picker = Some((list, 0));
        Effect::None
    }

    /// Moves the picker's selection one row, clamped to its list.
    fn move_picker(&mut self, down: bool) {
        if let Some(home) = self.home.as_mut()
            && let Some((list, selected)) = home.picker.as_mut()
        {
            *selected = if down {
                (*selected)
                    .saturating_add(1)
                    .min(list.len().saturating_sub(1))
            } else {
                (*selected).saturating_sub(1)
            };
        }
    }

    /// Chooses the picker's selected workspace for the next `start`.
    fn choose_picker(&mut self) {
        let choice = self
            .home
            .as_ref()
            .and_then(|home| home.picker.as_ref())
            .and_then(|(list, selected)| list.get(*selected).cloned());
        if let (Some(home), Some(workspace)) = (self.home.as_mut(), choice) {
            home.chosen = Some(workspace);
            home.picker = None;
        }
    }

    /// Closes the picker, keeping the workspace as it was.
    fn close_picker(&mut self) {
        if let Some(home) = self.home.as_mut() {
            home.picker = None;
        }
    }

    /// Clicks the picker row `at`: past the list fixed at open it does
    /// nothing.
    fn pick(&mut self, at: usize) -> Effect {
        let choice = self
            .home
            .as_ref()
            .and_then(|home| home.picker.as_ref())
            .and_then(|(list, _)| list.get(at).cloned());
        if let (Some(home), Some(workspace)) = (self.home.as_mut(), choice) {
            home.chosen = Some(workspace);
            home.picker = None;
        }
        Effect::None
    }

    /// The text y copies and Ctrl+G opens for `spot`: the row's line;
    /// controls copy nothing.
    pub(super) fn home_text(&self, spot: Spot) -> Option<String> {
        match spot {
            Spot::Entry(key) => {
                let home = self.home.as_ref()?;
                let row = home.sessions.by_key(key)?;
                Some(line(row, &home.launch.project))
            }
            Spot::Stop(_) | Spot::Toggle | Spot::Workspace | Spot::Pick(_) => None,
        }
    }

    /// The workspace in use on home: the attached session's row
    /// workspace, else the picked one, else the launch workspace.
    pub(super) fn home_workspace(&self) -> PathBuf {
        if let Some(home) = &self.home {
            if let Some(session) = self.session()
                && let Some(row) = home.sessions.row(session)
            {
                return PathBuf::from(&row.workspace);
            }
            if let Some(chosen) = &home.chosen {
                return PathBuf::from(chosen);
            }
            return home.launch.workspace.clone();
        }
        self.workspace.clone()
    }

    /// Opens the row with `key`: the subscribes its level needs, then the
    /// session's commands. The conversation clears as going home does,
    /// and the gate holds until the last subscribe is answered.
    fn open_row(&mut self, key: u64) -> Effect {
        if !self.on_home() || self.link != Link::Up || matches!(self.phase, Phase::Pending { .. }) {
            return Effect::None;
        }
        let Some(row) = self
            .home
            .as_ref()
            .and_then(|home| home.sessions.by_key(key))
        else {
            return Effect::None;
        };
        if row.state == crate::home::State::Unreadable {
            self.notices.push(
                "Cannot attach: this session's schema is newer than this terminal reads."
                    .to_owned(),
            );
            return Effect::None;
        }
        let session = row.id.clone();
        let expected = self
            .home
            .as_ref()
            .and_then(|home| home.subs.expected(&session));
        self.go_home();
        self.attach(session.clone());
        let levels = opening(expected);
        let mut ack = String::new();
        let mut lines = Vec::new();
        for level in levels {
            let (id, line) = subscribe_line(&session, *level);
            if let Some(home) = self.home.as_mut() {
                home.subs.sent(id.clone(), session.clone(), *level);
            }
            ack = id;
            lines.push(line);
        }
        lines.push(self.ask_commands(&session));
        if let Some(home) = self.home.as_mut() {
            home.opening = Some(Opening { session, ack });
        }
        Effect::Send(lines)
    }

    /// Records the acknowledgement of the in-flight subscribe `id`:
    /// the accepted level changes only on acceptance. Some with its
    /// session when the id was in flight, either way.
    fn answered(&mut self, id: &str, accepted: bool) -> Option<SessionId> {
        self.home
            .as_mut()
            .and_then(|home| home.subs.answered(id, accepted))
    }

    /// Whether `id` is the open's last subscribe: only its rejection
    /// fails the open, and only its acceptance ends the gate.
    fn is_ack(&self, id: &str) -> bool {
        self.home
            .as_ref()
            .and_then(|home| home.opening.as_ref())
            .is_some_and(|opening| opening.ack == id)
    }

    /// Lowers `session` to `summary` when this connection holds it at
    /// `full` without showing it: not the attached one, nothing in
    /// flight, and its row live. Untouched live sessions get no
    /// subscribe: their rows come from the hub's feed, which the hub
    /// reads over its own connections, and subscribing each one would
    /// send a command to every live session.
    fn reconcile(&mut self, session: &SessionId) {
        let lowers = self.link == Link::Up
            && self.session() != Some(session)
            && self.home.as_ref().is_some_and(|home| {
                home.subs.full(session)
                    && !home.subs.pending(session)
                    && home
                        .sessions
                        .row(session)
                        .is_some_and(|row| row.left.is_none())
            });
        if lowers {
            let line = self.subscribe(session, Level::Summary);
            if let Some(home) = self.home.as_mut() {
                home.outbox.push(line);
            }
        }
    }

    /// A rejection of the open's last subscribe fails the open: home again
    /// when still attached, with the message as the row's note and a
    /// notice. A rejection of an earlier step changes nothing by itself.
    fn fail_open(&mut self, message: String) -> Vec<String> {
        let session = self
            .home
            .as_mut()
            .and_then(|home| home.opening.take())
            .map(|opening| opening.session);
        let Some(session) = session else {
            return Vec::new();
        };
        if let Some(home) = self.home.as_mut() {
            home.sessions.note(&session, message.clone());
        }
        let attached = self.session() == Some(&session);
        self.notices.push(message);
        if attached {
            self.go_home();
        }
        Vec::new()
    }

    /// The `start` args: the picked workspace on home, else the launch
    /// workspace, and the launch directory as today. Sending one hides the
    /// placeholder and clears the blocker lines until the run ends.
    pub(super) fn start_args(&mut self) -> Value {
        match &mut self.home {
            Some(home) => {
                home.prompted = true;
                home.blockers = Vec::new();
                let workspace = home
                    .chosen
                    .clone()
                    .unwrap_or_else(|| home.launch.workspace.display().to_string());
                json!({ "workspace": workspace })
            }
            None => json!({ "workspace": self.workspace.display().to_string() }),
        }
    }
}

/// The picker's rows: the launch directory and nine row workspaces.
const PICKER_CAP: usize = 10;

/// A `recent` line: the first page, or past `before`, naming `project`
/// while scoped.
fn recent_line(id: &str, before: Option<&str>, project: Option<&str>) -> String {
    let mut args = serde_json::Map::new();
    if let Some(before) = before {
        args.insert("before".to_owned(), Value::String(before.to_owned()));
    }
    if let Some(project) = project {
        args.insert("project".to_owned(), Value::String(project.to_owned()));
    }
    if args.is_empty() {
        json!({"id": id, "command": "recent"}).to_string()
    } else {
        json!({"id": id, "command": "recent", "args": Value::Object(args)}).to_string()
    }
}

/// A `subscribe` line for `session` at `level`, with its id.
fn subscribe_line(session: &SessionId, level: Level) -> (String, String) {
    let id = mint();
    let name = match level {
        Level::Summary => "summary",
        Level::Full => "full",
    };
    let line = session_command(&id, "subscribe", session, Some(json!({"level": name}))).to_string();
    (id, line)
}

/// A refusal's code and message.
fn refusal_parts(payload: &serde_json::Map<String, Value>) -> (&str, String) {
    let code = payload
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or_default();
    (code, refusal(payload))
}
/// A hub refusal's message, for a notice.
fn refusal(payload: &serde_json::Map<String, Value>) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("rejected")
        .to_owned()
}

#[cfg(test)]
#[path = "app_home_tests.rs"]
mod tests;
