//! Home's state on the app: the launch description, whether a `start`
//! went out, and what home draws (`docs/tui.md`, "Home"). The data it
//! draws lives in [`crate::home`]; this module is `App`'s home.

use std::path::PathBuf;

use serde_json::{Value, json};

use super::{App, Effect, Kind, Link, mint};
use crate::focus::{Area, order};
use crate::home::{
    HomeScreen, Launch, Left, Sessions, Spot, State, cascade_line, delete_line, from_status, line,
    line_with, recent_rows, toggle_line,
};
use crate::keys::{Edit, Key};
use crate::link::Line;
use crate::mouse::{Target, TargetId};
use contract::SessionId;

mod chips;
mod open;
mod open_at;
mod prompt;
mod reset;
mod typed;
mod workspace;

use open::Subscriptions;
use prompt::{Prompt, Questions};
use workspace::Chooser;

/// Home's state: the launch description, and whether a `start` went out in
/// this run, which hides the input box's placeholder.
pub(super) struct Home {
    /// What the terminal knows about where it was launched.
    pub(super) launch: Launch,
    /// A `start` went out in this run.
    prompted: bool,
    /// The chip focused last: ↓ from the entry bar and ↑ from the
    /// first row land here while drawn, else on the workspace chip.
    /// Always a chip; starts `Spot::Workspace`.
    pub(super) last_chip: Spot,
    /// The session list: live rows from the feed, exited rows from `recent`.
    pub(super) sessions: Sessions,
    /// `feed` and the first `recent` page went out.
    fed: bool,
    /// The `feed` command waiting for its answer.
    feed_id: Option<String>,
    /// The latest `recent` command waiting for its answer, and whether it
    /// asked the first page.
    recent_ask: Option<(String, bool)>,
    /// This connection's subscriptions, the session opening and the
    /// lowering lines queued.
    subs: Subscriptions,
    /// `/resume` asks the next frame to focus the list.
    focus_list: bool,
    /// A rejected `start`'s message, above the box until the next `start`
    /// goes out.
    blockers: Vec<String>,
    /// The workspace chooser: the picked workspace, the new worktree
    /// switch and the picker.
    chooser: Chooser,
    /// The foot's question, the stops and deletes waiting for their
    /// answers, and the quit question's closes.
    questions: Questions,
    /// The attached session left after it was attached: a `session_left`
    /// for it arrived while attached, and no live status or fresh attach
    /// has superseded it. While it names the attached session, quitting
    /// prints no resume line for it: the session is dead. An exited row
    /// from before the attach sets no marker: the session is live and
    /// resuming, and still gets its line.
    pub(super) left_after_attach: Option<SessionId>,
}

impl App {
    /// Stores the launch description as home, keying prompt history by its
    /// project as the run argument did. The app keeps today's screen, so
    /// the jigs and every existing app test stay byte-identical.
    pub(crate) fn set_home(&mut self, launch: Launch) {
        self.set_reduced_motion(launch.reduced_motion);
        self.set_project(launch.project.clone());
        // A launch that saved the rail hidden starts with it hidden;
        // this is the launch's state, not a person-caused hide, so it
        // queues nothing.
        let rail_hidden = launch.rail_hidden;
        // The picker's scope starts from launch, then from what
        // `/scoped-models` saves.
        self.model_picker.scoped = launch.scoped_models.clone();
        self.home = Some(Home {
            launch,
            prompted: false,
            sessions: Sessions::default(),
            fed: false,
            feed_id: None,
            recent_ask: None,
            subs: Subscriptions::default(),
            focus_list: false,
            last_chip: Spot::Workspace,
            blockers: Vec::new(),
            chooser: Chooser::default(),
            questions: Questions::default(),
            left_after_attach: None,
        });
        if rail_hidden {
            self.chrome.hide_rail();
        }
    }

    /// Stores bare `fiber`'s `--model` and `-c`, normalized into the
    /// per-run layer: each `start` carries them in `overrides`
    /// (`docs/invocation.md`, "Sessions"). The loop calls it once with
    /// the launch description's layer, before the first frame.
    pub(crate) fn set_launch_overrides(&mut self, overrides: Vec<String>) {
        self.launch_overrides = overrides;
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
            .is_some_and(|home| matches!(home.questions.prompt(), Some(Prompt::Quit)))
    }

    /// Whether a home modal holds the keyboard: the delete question or
    /// the workspace picker, the two `home_key` branches after the quit
    /// question.
    pub(super) fn home_modal(&self) -> bool {
        self.on_home()
            && self.home.as_ref().is_some_and(|home| {
                home.questions.prompt().is_some() || home.chooser.picker().is_some()
            })
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
            .chooser
            .chosen()
            .map(PathBuf::from)
            .unwrap_or_else(|| home.launch.workspace.clone());
        let segment = workspace
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| workspace.display().to_string());
        // The delete question names the session and everything
        // `--cascade` would add; the frame wraps and scrolls it.
        let (question, question_scroll) = match home.questions.prompt() {
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
        // The foot names the key map's bound key, leaving it out
        // when the action is unbound; Ctrl+C cannot be rebound, so its
        // hint stays fixed.
        let foot = if self.hint() {
            self.hint_text()
        } else {
            match self.keys().first_label("key_map") {
                Some(label) => {
                    format!("↓ chips and sessions · {label} the key map · Ctrl+C twice to quit")
                }
                None => "↓ chips and sessions · Ctrl+C twice to quit".to_owned(),
            }
        };
        Some(HomeScreen {
            version: home.launch.version.clone(),
            // A remote client has no launch directory; the picker it would
            // always show is a later ticket's.
            glyph: home.launch.logo_glyph.clone(),
            chips: {
                let mut chips = vec![(Some(Spot::Workspace), format!("[{segment}]"))];
                // The new worktree switch sits beside the workspace
                // chip, shown while the workspace in use is in git.
                if home.in_git() {
                    chips.push((
                        Some(Spot::Worktree),
                        if home.chooser.worktree() {
                            "[x] new worktree".to_owned()
                        } else {
                            "[ ] new worktree".to_owned()
                        },
                    ));
                }
                chips.extend([
                    (
                        Some(Spot::Model),
                        format!("[{}]", home.launch.model.as_deref().unwrap_or("no model")),
                    ),
                    (
                        Some(Spot::Thinking),
                        format!(
                            "[thinking: {}]",
                            home.launch.thinking.as_deref().unwrap_or("default")
                        ),
                    ),
                    (None, "Enter starts a session".to_owned()),
                ]);
                chips
            },
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
                        line_with(row, &home.launch.project, self.motion.glyph(row)),
                        row.state != State::Unreadable,
                    )
                })
                .collect(),
            picker: home.picker_screen(),
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
        let mut asks = self.picker_asks();
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        if home.fed {
            let mut out = home.subs.take_outbox();
            out.append(&mut asks);
            return out;
        }
        home.fed = true;
        let feed = mint();
        let recent = mint();
        home.feed_id = Some(feed.clone());
        home.recent_ask = Some((recent.clone(), true));
        let project = home.launch.project.clone();
        let scoped = home.launch.git && !home.sessions.show_all();
        let recent = recent_line(&recent, None, scoped.then_some(project.as_str()));
        let mut out = vec![json!({"id": feed, "command": "feed"}).to_string(), recent];
        out.extend(self.launch_lines());
        out.append(&mut asks);
        out
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
                    let exited = SessionId(id.to_owned());
                    // An exit for the attached session arrived after the
                    // attach: the session is dead, so quitting owes it no
                    // resume line. An exited row from before the attach
                    // sets no marker: the session is live and resuming.
                    let after_attach = self.session() == Some(&exited);
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.left(&exited, how);
                        if after_attach {
                            home.left_after_attach = Some(exited);
                        }
                    }
                    Some(Vec::new())
                }
                "command_accepted" | "command_rejected" => {
                    let id = hub.payload.get("command_id").and_then(Value::as_str)?;
                    let accepted = hub.kind.as_str() == "command_accepted";
                    // The picker's `directories` answers fold in first,
                    // while home exists: an id the open picker sent is
                    // consumed here, any other id falls through untouched.
                    if let Some(lines) = self.picker_answer(id, accepted, &hub.payload) {
                        return Some(lines);
                    }
                    if let Some(lines) = self.subscribe_answer(id, accepted, &hub.payload) {
                        return Some(lines);
                    }
                    if let Some(ask) = self.take_ask(id) {
                        return Some(self.answer_ask(ask, accepted, &hub.payload));
                    }
                    if let Some(lines) = self.rail_answered(id, accepted, &hub.payload) {
                        return Some(lines);
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
                            let code = refusal_code(&hub.payload);
                            self.notices.push(&code, refusal(&hub.payload));
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
                            self.list_answered(first, true);
                        } else {
                            let code = refusal_code(&hub.payload);
                            self.notices.push(&code, refusal(&hub.payload));
                            self.list_answered(first, false);
                        }
                        return Some(Vec::new());
                    }
                    if accepted {
                        let result = hub.payload.get("result");
                        let result = result.unwrap_or(&serde_json::Value::Null);
                        if self.sessions_accepted(id, result) {
                            return Some(Vec::new());
                        }
                    } else if self.sessions_rejected(id) {
                        return Some(Vec::new());
                    }
                    None
                }
                _ => None,
            },
            Line::Session(envelope) => {
                if envelope.kind == "session_status" {
                    // A delegate's own summary is for its parent's panel,
                    // never the session list (`docs/tui.md`, "The panel").
                    if self.delegate_status(envelope) {
                        return Some(Vec::new());
                    }
                    let session = envelope.session_id.clone();
                    let was_left = self
                        .home
                        .as_ref()
                        .and_then(|home| home.sessions.row(&session))
                        .is_some_and(|row| row.left.is_some());
                    let row = from_status(envelope);
                    if let Some(home) = self.home.as_mut() {
                        home.sessions.status(row);
                        // A live status for the session supersedes an exit
                        // that arrived after attach: it is live again.
                        if home
                            .sessions
                            .row(&session)
                            .is_some_and(|row| row.left.is_none())
                            && home.left_after_attach.as_ref() == Some(&session)
                        {
                            home.left_after_attach = None;
                        }
                    }
                    self.note_feed(&session);
                    if was_left {
                        self.reconcile(&session);
                    }
                    if self.gated(&session) {
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
                    if let Some(lines) = self.subscribe_answer(id, accepted, &envelope.payload) {
                        return if accepted {
                            self.subscribe_accepted(&envelope.session_id, id)
                        } else {
                            Some(lines)
                        };
                    }
                    if let Some(ask) = self.take_ask(id) {
                        return Some(self.answer_ask(ask, accepted, &envelope.payload));
                    }
                }
                if envelope.kind == "notice" && self.gated(&envelope.session_id) {
                    // An opening session's notice reaches the notices
                    // while its replay stays gated: a refused credential
                    // switch reports without opening the gate or
                    // returning home.
                    self.notices
                        .push(&refusal_code(&envelope.payload), refusal(&envelope.payload));
                    return Some(Vec::new());
                }
                if self.gated(&envelope.session_id) {
                    return Some(Vec::new());
                }
                None
            }
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
    /// while it is open, then the chip row's and the list's arrows, and
    /// j on the last drawn row paging below the fold. `None` for
    /// anything else, so the focused stops keep moving as they do on the
    /// conversation. Only ↓ enters the list from the entry bar, landing
    /// on the chip row first: j moves only while the list has focus, and
    /// the draft keeps every printable key typed into it.
    pub(super) fn home_key(&mut self, key: &Key) -> Option<Effect> {
        // The quit question takes every key first, on home or not.
        if let Some(effect) = self.quit_key(key) {
            return Some(effect);
        }
        if !self.on_home() {
            return None;
        }
        // The delete question takes every key first.
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.questions.prompt().is_some())
        {
            return self.question_key(key);
        }
        // The picker takes every key first: typing, completing,
        // moving, choosing and closing it, while Ctrl+C passes through
        // and anything else is swallowed so the draft keeps nothing
        // typed into the picker.
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.chooser.picker().is_some())
        {
            return self.picker_key(key);
        }
        // A focused chip takes its keys ahead of focus: arrows move,
        // Enter clicks, typing returns to the entry bar. `None` passes
        // the key on.
        if self.focused_chip().is_some() {
            return self.chip_key(key);
        }
        // Only ↓ enters the list: with the box focused the draft keeps
        // j, and focus moves it once the list has focus.
        if matches!(key, Key::Char('j')) && self.focus.is_none() {
            return None;
        }
        if matches!(key, Key::Up | Key::Down) {
            // ↑ ↓ on a row or the toggle walk the rows, row to row.
            if self.focus.is_some() {
                if let Some(effect) = self.list_key(key) {
                    return Some(effect);
                }
                return None;
            }
            // ↑ in the box stays with recall; ↓ leaves for the chip
            // row, while the completion panel and a recalled prompt
            // keep it and a multi-line draft moves its cursor first.
            if matches!(key, Key::Up) {
                return None;
            }
            return self.entry_down();
        }
        if !matches!(key, Key::Char('j')) {
            // Backspace on a focused row asks to delete it when it
            // exited; anything else on a focused row is swallowed.
            if matches!(key, Key::Backspace) {
                return self.delete_key();
            }
            return None;
        }
        // j on the last drawn row pages below the fold; shorter of it
        // steps through the stops as on the conversation.
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
    /// the scope, the workspace chip opens the picker, the worktree
    /// switch toggles it, the model chip opens the model picker, the
    /// thinking chip opens it at the model's thinking chips, a picker
    /// row chooses its workspace, and a quit choice does what its key
    /// does while the quit question is open, on home or over a session.
    pub(super) fn home_click(&mut self, spot: Spot) -> Effect {
        if let Spot::Quit(choice) = spot {
            return self.quit_click(choice);
        }
        match spot {
            Spot::Entry(key) => self.open_row(key),
            Spot::Stop(key) => self.stop_or_ask(key),
            Spot::Toggle => self.toggle_scope(),
            Spot::Workspace => self.open_picker(),
            Spot::Worktree => self.toggle_worktree(),
            Spot::Model => self.open_model_picker(crate::model_picker::Mode::Choose),
            Spot::Thinking => self.open_thinking(),
            Spot::Pick(at) => self.pick(at),
            Spot::Keep => {
                self.close_delete_prompt();
                Effect::None
            }
            Spot::Question => Effect::None,
            Spot::Start(_) | Spot::More(_) => Effect::None,
            Spot::Quit(_) => Effect::None,
        }
    }

    /// Asks the first `recent` page again, naming the launch project
    /// `on_edit`: the delete question when it exited, swallowed
    /// otherwise. The workspace picker owns every edit while open, so
    /// nothing reaches the chip row or the draft behind it. A focused
    /// chip takes its edits first: ← → move
    /// between chips, every other edit returns to the entry bar. With
    /// the box focused the draft keeps the key.
    pub(super) fn home_edit(&mut self, edit: &Edit) -> Option<Effect> {
        if !self.on_home() {
            return None;
        }
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.questions.prompt().is_some())
        {
            return Some(Effect::None);
        }
        if self
            .home
            .as_ref()
            .is_some_and(|home| home.chooser.picker().is_some())
        {
            return Some(Effect::None);
        }
        // A focused chip takes its edits ahead of the row's Delete:
        // ← → move between chips, every other edit returns to the
        // entry bar and applies there.
        if self.focused_chip().is_some()
            && let Some(effect) = self.chip_edit(edit)
        {
            return Some(effect);
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
    pub(super) fn page_recent(&mut self, focused: u64) -> Option<Effect> {
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

    /// The text y copies and Ctrl+G opens for `spot`: the row's line;
    /// controls copy nothing.
    pub(super) fn home_text(&self, spot: Spot) -> Option<String> {
        match spot {
            Spot::Entry(key) => {
                let home = self.home.as_ref()?;
                let row = home.sessions.by_key(key)?;
                Some(line(row, &home.launch.project))
            }
            Spot::Stop(_)
            | Spot::Toggle
            | Spot::Start(_)
            | Spot::More(_)
            | Spot::Workspace
            | Spot::Worktree
            | Spot::Model
            | Spot::Thinking
            | Spot::Pick(_)
            | Spot::Keep
            | Spot::Question
            | Spot::Quit(_) => None,
        }
    }

    /// The `start` args: the picked workspace on home, else the launch
    /// workspace, and the launch directory as today. Bare `fiber`'s
    /// `--model` and `-c` ride in `overrides`, normalized into the
    /// per-run layer (`docs/invocation.md`, "Sessions"). The new
    /// worktree switch adds `"worktree": true` only while it shows and
    /// is on. Sending one hides the placeholder and clears the blocker
    /// lines until the run ends.
    pub(super) fn start_args(&mut self) -> Value {
        match &mut self.home {
            Some(home) => {
                home.prompted = true;
                home.blockers = Vec::new();
                let workspace = home
                    .chooser
                    .chosen()
                    .map(str::to_owned)
                    .unwrap_or_else(|| home.launch.workspace.display().to_string());
                let mut args = json!({"workspace": workspace});
                if let Some(object) = args.as_object_mut() {
                    if !self.launch_overrides.is_empty() {
                        object.insert(
                            "overrides".to_owned(),
                            Value::Array(
                                self.launch_overrides
                                    .iter()
                                    .map(|override_text| Value::String(override_text.clone()))
                                    .collect(),
                            ),
                        );
                    }
                    if home.chooser.worktree() && home.in_git() {
                        object.insert("worktree".to_owned(), json!(true));
                    }
                }
                args
            }
            None => json!({ "workspace": self.workspace.display().to_string() }),
        }
    }
}

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

/// A hub refusal's message, for a notice.
pub(super) fn refusal(payload: &serde_json::Map<String, Value>) -> String {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("rejected")
        .to_owned()
}

/// A refusal's code: `malformed` when its answer carries none the
/// terminal can read as a command answer.
pub(super) fn refusal_code(payload: &serde_json::Map<String, Value>) -> String {
    payload
        .get("code")
        .and_then(Value::as_str)
        .filter(|code| !code.is_empty())
        .unwrap_or("malformed")
        .to_owned()
}

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
