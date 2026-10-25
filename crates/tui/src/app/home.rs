//! Home's state on the app: the launch description, whether a `start`
//! went out, and what home draws (`docs/tui.md`, "Home"). The data it
//! draws lives in [`crate::home`]; this module is `App`'s home.

use serde_json::{Value, json};

use std::cell::Cell;

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::{App, Effect, Kind, mint};
use crate::home::{
    Chip, Header, HomeScreen, Launch, Left, ListLine, Sessions, Spot, State, cascade_line,
    delete_line, from_status, import_head, line, recent_rows,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ListFocus {
    FirstStop,
    FirstExited,
    Row(u64),
}

/// Home's state: the launch description, and whether a `start` went out in
/// this run, which hides the input box's placeholder.
pub(super) struct Home {
    /// What the terminal knows about where it was launched.
    pub(super) launch: Launch,
    /// The hub's build as `fiber --version` writes it, when the latest
    /// `hub_hello` named another version or commit than this terminal's.
    pub(super) hub_other: Option<String>,
    /// A `start` went out in this run.
    prompted: bool,
    /// The first-run import offer is still eligible: resolved once,
    /// cleared by Esc or by its accepted answer, never re-resolved,
    /// so a receipt landing mid-run neither retracts a shown offer
    /// nor re-fires a dismissed one.
    pub(super) import_offer: bool,
    /// The offer's pending state still owes its hub read: set on a
    /// remote home, whose receipts live on the hub machine past a link
    /// the startup resolve cannot use. The first outgoing pass reads
    /// it from the hub and clears this, so the read runs once.
    pub(super) import_owed: bool,
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
    /// The target the next home frame focuses, if a list open asked for it.
    focus_list: Option<ListFocus>,
    /// The session list's scroll offset: the wheel over its rows moves
    /// it, focus unmoved, and the frame clamps it to the rows past its
    /// window.
    list_scroll: Cell<usize>,
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
    /// The session home came from, while its row is live: `→` and `Esc`
    /// go back to it, and the foot names the way back.
    pub(super) came_from: Option<SessionId>,
}

impl Home {
    /// The launch project that sections and row text compare against;
    /// a listed hub has none.
    pub(super) fn launch_project(&self) -> Option<&str> {
        self.launch.location.project()
    }

    /// The project `prompt_history` names: the launch project, else
    /// the project of the chosen workspace's newest row.
    pub(super) fn history_project(&self) -> Option<String> {
        self.launch
            .location
            .project()
            .or_else(|| {
                self.chooser
                    .chosen()
                    .and_then(|workspace| self.sessions.project_of(workspace))
            })
            .map(str::to_owned)
    }
}

/// One bold key with its dim label, joined by dim ` · `.
fn hint_key(spans: &mut Vec<Span<'static>>, key: &str, label: &str) {
    if !spans.is_empty() {
        spans.push(Span::styled(
            " · ",
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    spans.push(Span::styled(
        key.to_owned(),
        Style::new().add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(" {label}"),
        Style::new().add_modifier(Modifier::DIM),
    ));
}

/// The back part of the foot hint: `→ or Esc back to the session`
/// naming the action's first bound key, or `→ back to the session`
/// when the action holds no key. The `→` part is always present while
/// the back key is live: plain `→` is the arrow, like `↑↓`.
fn hint_back(spans: &mut Vec<Span<'static>>, esc: Option<&str>) {
    if !spans.is_empty() {
        spans.push(Span::styled(
            " · ",
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
    spans.push(Span::styled("→", Style::new().add_modifier(Modifier::BOLD)));
    if let Some(key) = esc {
        spans.push(Span::styled(
            " or ",
            Style::new().add_modifier(Modifier::DIM),
        ));
        spans.push(Span::styled(
            key.to_owned(),
            Style::new().add_modifier(Modifier::BOLD),
        ));
    }
    spans.push(Span::styled(
        " back to the session",
        Style::new().add_modifier(Modifier::DIM),
    ));
}

/// The foot hint with its keys bold: each action by its first bound
/// key, leaving an unbound action's part out with its joiner.
fn home_foot(
    open: Option<String>,
    delete: Option<String>,
    key_map: Option<String>,
    back: bool,
    esc: Option<String>,
) -> ratatui::text::Line<'static> {
    let mut spans = Vec::new();
    if let Some(label) = open {
        hint_key(
            &mut spans,
            &label,
            "opens the focused session, or starts one from the box",
        );
    }
    hint_key(&mut spans, "↑↓", "move");
    if back {
        hint_back(&mut spans, esc.as_deref());
    }
    if let Some(label) = delete {
        hint_key(
            &mut spans,
            &label,
            "stops a live one, deletes an exited one",
        );
    }
    if let Some(label) = key_map {
        hint_key(&mut spans, &label, "the key map");
    }
    hint_key(&mut spans, "Ctrl+C", "twice to quit");
    ratatui::text::Line::from(spans)
}

impl App {
    /// Stores the launch description as home, keying prompt history by its
    /// project as the run argument did. The app keeps today's screen, so
    /// the jigs and every existing app test stay byte-identical.
    pub(crate) fn set_home(&mut self, launch: Launch) {
        self.set_reduced_motion(launch.reduced_motion);
        // A launch that saved the rail hidden starts with it hidden;
        // this is the launch's state, not a person-caused hide, so it
        // queues nothing.
        let rail_hidden = launch.rail_hidden;
        // The picker's scope starts from launch, then from what
        // `/scoped-models` saves.
        self.model_picker.scoped = launch.scoped_models.clone();
        // A remote home owes the offer its hub read: the receipts live
        // on the hub machine, past the link the startup resolve cannot
        // use. A local home resolves from its own seam at startup.
        let import_owed = matches!(launch.location, crate::home::LaunchLocation::Remote { .. });
        self.home = Some(Home {
            launch,
            hub_other: None,
            prompted: false,
            import_offer: false,
            import_owed,
            sessions: Sessions::default(),
            fed: false,
            feed_id: None,
            recent_ask: None,
            subs: Subscriptions::default(),
            focus_list: None,
            list_scroll: Cell::new(0),
            last_chip: Spot::Workspace,
            blockers: Vec::new(),
            chooser: Chooser::default(),
            questions: Questions::default(),
            left_after_attach: None,
            came_from: None,
        });
        self.sync_project();
        if self.home.as_ref().is_some_and(|home| home.no_workspace()) {
            self.open_picker();
        }
        if rail_hidden {
            self.chrome.hide_rail();
        }
        // The launch carries `tui.inline_images`: size the images now
        // that it is known.
        self.refresh_image_sizing();
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
        self.home.as_ref().is_some_and(|home| {
            matches!(
                home.questions.prompt(),
                Some(Prompt::Quit) | Some(Prompt::CloseAll { .. }) | Some(Prompt::Stop { .. })
            )
        })
    }

    /// Whether the clear question is open, on home or not: while it is,
    /// every key goes to it first.
    pub(crate) fn clear_open(&self) -> bool {
        self.home
            .as_ref()
            .is_some_and(|home| matches!(home.questions.prompt(), Some(Prompt::Clear { .. })))
    }

    /// The clear question, while open: the old session's name, what it
    /// waits on, and whether Enter staged its `start`.
    pub(crate) fn clear_question(&self) -> Option<(String, String, bool)> {
        match self.home.as_ref().and_then(|home| home.questions.prompt()) {
            Some(Prompt::Clear {
                name, waiting, now, ..
            }) => Some((name.clone(), waiting.clone(), *now)),
            _ => None,
        }
    }

    /// The session list's scroll offset: what the frame's window
    /// starts at, after clamping it to the rows past the window.
    pub(crate) fn home_list_scroll(&self) -> usize {
        self.home
            .as_ref()
            .map(|home| home.list_scroll.get())
            .unwrap_or(0)
    }

    /// Moves the session list's scroll offset, as the wheel over its
    /// rows asks. The next frame clamps it to the rows past its
    /// window, and pulls a focused row outside the window into view.
    pub(crate) fn set_home_list_scroll(&self, start: usize) {
        if let Some(home) = self.home.as_ref() {
            home.list_scroll.set(start);
        }
    }

    /// What home draws, or `None` unless [`App::on_home`] holds.
    pub(crate) fn home_screen(&self) -> Option<HomeScreen> {
        let home = self.home.as_ref().filter(|_| self.on_home())?;
        // The workspace in use: the picked one, else the launch
        // directory. Clicking its chip opens the workspace picker. With
        // none the chip reads `[no workspace]`, so no segment shows.
        let segment = self.home_workspace().map(|workspace| {
            workspace
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| workspace.display().to_string())
        });
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
            // The first-run import offer draws its own frame below;
            // the question carries its head.
            Some(Prompt::Import) => (Some(import_head()), 0),
            // The quit question draws through the hint below, recomputed
            // every frame; the close-all, stop and clear questions draw
            // as overlays from what was fixed when they opened.
            Some(Prompt::Quit)
            | Some(Prompt::CloseAll { .. })
            | Some(Prompt::Stop { .. })
            | Some(Prompt::Clear { .. })
            | None => (None, 0),
        };
        // The foot names each action by its first bound key, leaving
        // an unbound action's part out with its joiner; Ctrl+C cannot
        // be rebound, so its hint stays fixed. The quit hint replaces
        // the foot while Ctrl+C is armed.
        let sections = home.sessions.sections(home.launch_project());
        let (mut waiting_count, mut working, mut retrying, mut crashed, mut ready) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut spend = 0.0;
        let mut live_count = 0usize;
        let mut list = Vec::new();
        let wall = self.rail_state().wall();
        for section in sections {
            let hidden_exited = section.hidden();
            let section_spend = section
                .live
                .iter()
                .fold(0.0, |spend, row| spend + row.spend);
            let waiting = section
                .live
                .iter()
                .filter(|row| row.left.is_none() && row.state == State::Waiting)
                .count();
            list.push(ListLine::Head {
                start: section.start,
                name: section.name,
                live: section.live.len(),
                spend: crate::format::money(section_spend),
                waiting,
            });
            for row in section.live {
                live_count += 1;
                spend += row.spend;
                if row.left == Some(Left::Crashed) {
                    crashed += 1;
                } else {
                    match row.state {
                        State::Waiting => waiting_count += 1,
                        State::Working | State::Jobs => working += 1,
                        State::Retrying => retrying += 1,
                        State::Idle => ready += 1,
                        State::Unreadable => {}
                    }
                }
                let status = row.status.as_ref();
                let stop = row.left.is_none() && row.state != State::Unreadable;
                // A closing row keeps no ⌥N on home, even before the
                // rail sheds its card.
                let number = (!row.status.as_ref().is_some_and(|status| status.closing))
                    .then(|| self.rail_state().number(&row.id))
                    .flatten();
                list.push(ListLine::Live {
                    key: row.key,
                    number,
                    glyph: self.motion.glyph(row).to_owned(),
                    name: crate::home::title(row),
                    word: crate::view::rail::word(row),
                    tone: crate::view::rail::tone(row),
                    since: status
                        .map(|status| crate::view::rail::elapsed(status.since, wall))
                        .unwrap_or_default(),
                    stop,
                    id: row.id.0.clone(),
                    turns: status.and_then(|status| status.turns),
                    branch: status
                        .and_then(|status| status.git.as_ref().and_then(|git| git.branch.clone())),
                    spend: crate::format::money(row.spend),
                    context: status.and_then(|status| {
                        status.context.as_ref().map(|context| {
                            context
                                .tokens
                                .saturating_mul(100)
                                .checked_div(context.window)
                                .unwrap_or(0)
                        })
                    }),
                    wait: if row.state == State::Unreadable {
                        row.note.clone().or_else(|| Some(self.unreadable_reason()))
                    } else {
                        row.note.clone().or_else(|| row.waiting.clone())
                    },
                });
            }
            if !section.exited.is_empty() {
                list.push(ListLine::ExitedHead {
                    count: section.exited.len(),
                });
            }
            for row in section.exited.iter().take(section.shown_exited) {
                list.push(ListLine::Exited {
                    key: row.key,
                    glyph: "○",
                    name: crate::home::title(row),
                    id: row.id.0.clone(),
                    turns: row.status.as_ref().and_then(|status| status.turns),
                    spend: crate::format::money(row.spend),
                    age: row.status.as_ref().map(|status| {
                        format!("{} ago", crate::view::rail::elapsed(status.since, wall))
                    }),
                });
            }
            if let Some(row) = section.exited.get(section.shown_exited) {
                list.push(ListLine::More {
                    key: row.key,
                    count: hidden_exited,
                });
            }
        }
        let header = Header {
            counts: [waiting_count, working, retrying, crashed, ready]
                .into_iter()
                .zip([
                    (crate::theme::Role::Attention, "waiting on you"),
                    (crate::theme::Role::Accent, "working"),
                    (crate::theme::Role::Warning, "retrying"),
                    (crate::theme::Role::Error, "crashed"),
                    (crate::theme::Role::Muted, "ready"),
                ])
                .filter(|(count, _)| *count > 0)
                .map(|(count, (role, label))| (role, format!("{count} {label}")))
                .collect(),
            spend: (live_count > 0).then(|| format!("{} live", crate::format::money(spend))),
        };
        let foot = if self.hint() {
            ratatui::text::Line::from(self.hint_text())
        } else {
            home_foot(
                self.keys().first_label("open_focused"),
                self.keys().first_label("delete_session"),
                self.keys().first_label("key_map"),
                self.came_from_live(),
                self.keys().first_label("close_or_interrupt"),
            )
        };
        Some(HomeScreen {
            header,
            list,
            empty: home.sessions.all().is_empty(),
            version: home.launch.version.clone(),
            glyph: home.launch.logo_glyph.clone(),
            chips: {
                let chip = |spot: Option<Spot>, text: String| Chip {
                    spot,
                    dim: false,
                    text,
                };
                let dim = home.no_workspace();
                let workspace = if dim {
                    "no workspace"
                } else {
                    segment.as_deref().unwrap_or("")
                };
                let mut chips = vec![Chip {
                    dim,
                    ..chip(Some(Spot::Workspace), format!("[{workspace}]"))
                }];
                // The new worktree switch sits beside the workspace
                // chip, shown while the workspace in use is in git.
                if home.in_git() {
                    chips.push(chip(
                        Some(Spot::Worktree),
                        if home.chooser.worktree() {
                            "[x] new worktree".to_owned()
                        } else {
                            "[ ] new worktree".to_owned()
                        },
                    ));
                }
                chips.extend([
                    chip(
                        Some(Spot::Model),
                        format!("[{}]", home.launch.model.as_deref().unwrap_or("no model")),
                    ),
                    chip(
                        Some(Spot::Thinking),
                        format!(
                            "[thinking: {}]",
                            home.launch.thinking.as_deref().unwrap_or("default")
                        ),
                    ),
                    chip(None, "Enter starts a session".to_owned()),
                ]);
                chips
            },
            picker: home.picker_screen(),
            blockers: home.blockers.clone(),
            foot,
            question,
            question_import: matches!(home.questions.prompt(), Some(Prompt::Import)),
            question_scroll,
            // Until the first prompt, the placeholder says "/? for
            // shortcuts"; no session exists before Enter, so opening the
            // terminal, glancing at home and leaving creates nothing.
            placeholder: self.draft.is_empty() && !home.prompted,
        })
    }

    /// The hub lines home asks for, once the link is up: `feed`, then the
    /// first unscoped `recent` page. The session list fills in when the
    /// hub's feed arrives.
    pub(super) fn home_outgoing(&mut self) -> Vec<String> {
        if !self.outbox.is_up() {
            return Vec::new();
        }
        // A remote home's offer waits for the hub: the startup
        // resolve reads the terminal's own home, while the receipts
        // live on the hub machine.
        self.resolve_import_offer();
        let mut asks = self.picker_asks();
        asks.extend(self.start_model_pick());
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
        let recent = recent_line(&recent, None);
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
                            home.left_after_attach = Some(exited.clone());
                        }
                    }
                    if how == Left::Exited {
                        self.rail_state.note_exited(&exited);
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
                    // The offer's `import` folds as the `/import` row's:
                    // Task 3's notices closing the offer on acceptance,
                    // the existing refused path with the offer open on
                    // rejection.
                    if let Some(lines) = self.import_answer(id, accepted, &hub.payload) {
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
                    if !accepted && let Some((Kind::Start, _)) = self.outbox.pending.get(id) {
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
                                home.refill_picker();
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
                        home.refill_picker();
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
                    // The `commands` an opening sends is answered while
                    // its replay is still gated: the answer is no history,
                    // and it fills the `/` list now.
                    if accepted
                        && self.gated(&envelope.session_id)
                        && self.completion.awaits(id)
                        && let Some(answer) =
                            super::read!(envelope, contract::events::CommandAccepted)
                    {
                        self.commands_answered(&answer);
                        return Some(Vec::new());
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
                // An import's answer settles through its pending entry
                // even while its session reopens: falling through to
                // `on_session` opens its report, or refuses it with one
                // notice, instead of dropping it at the gate.
                if matches!(
                    envelope.kind.as_str(),
                    "command_accepted" | "command_rejected"
                ) && let Some(id) = envelope.payload.get("command_id").and_then(Value::as_str)
                    && let Some((Kind::Import(_), _)) = self.outbox.pending.get(id)
                {
                    return None;
                }
                if self.gated(&envelope.session_id) {
                    return Some(Vec::new());
                }
                None
            }
        }
    }

    /// Goes home, focused on the first exited row: with the next frame
    /// focusing that row, or the input box when no exited row shows.
    pub(super) fn resume_list(&mut self) -> Effect {
        let effect = self.leave();
        if let Some(home) = self.home.as_mut() {
            home.focus_list = Some(ListFocus::FirstExited);
        }
        effect
    }

    /// Focuses the requested list stop after the next home frame. `/resume`
    /// asks for the first exited row; `OpenAt::List` asks for the first row.
    /// True when it ran, so the frame draws again with the focus shown.
    pub(super) fn home_drawn(&mut self, _targets: &[Target]) -> bool {
        let Some(focus_list) = self.home.as_mut().and_then(|home| home.focus_list.take()) else {
            return false;
        };
        let stops = self.all_list_stops();
        self.focus = stops.into_iter().find_map(|spot| {
            let Spot::Entry(key) = spot else {
                return None;
            };
            let row = self.home.as_ref()?.sessions.by_key(key)?;
            let should_focus = match focus_list {
                ListFocus::FirstStop => true,
                ListFocus::FirstExited => row.left.is_some(),
                ListFocus::Row(want) => key == want,
            };
            should_focus.then_some(TargetId::Home(Spot::Entry(key)))
        });
        true
    }

    /// Focuses the row `key` names after the next home frame: expands the
    /// row's project when the row is an exited one hidden behind more, so
    /// the row is a list stop. An unknown key focuses nothing.
    pub(super) fn focus_row(&mut self, key: u64) {
        let Some(home) = self.home.as_mut() else {
            return;
        };
        let Some(row) = home.sessions.by_key(key) else {
            return;
        };
        let exited = row.left.is_some();
        let project = row.project.clone();
        if exited {
            home.sessions.expand(&project);
        }
        home.focus_list = Some(ListFocus::Row(key));
    }

    /// Whether home's delete question or first-run import offer is open:
    /// the home screen's question, below the quit-surface questions.
    pub(in crate::app) fn home_question_open(&self) -> bool {
        self.on_home()
            && self.home.as_ref().is_some_and(|home| {
                home.questions.prompt().is_some_and(|prompt| {
                    !matches!(
                        prompt,
                        Prompt::Quit
                            | Prompt::CloseAll { .. }
                            | Prompt::Stop { .. }
                            | Prompt::Clear { .. }
                    )
                })
            })
    }

    /// Whether home's workspace picker is open.
    pub(in crate::app) fn home_picker_open(&self) -> bool {
        self.on_home()
            && self
                .home
                .as_ref()
                .is_some_and(|home| home.chooser.picker().is_some())
    }

    /// A key for home's own screen, once no question or picker holds
    /// the keyboard: the chip row's and the list's arrows, and j at the
    /// last drawn list stop paging below the fold. `None` for anything
    /// else, so focused stops keep moving as they do on the conversation.
    /// ↓ from the entry bar focuses the last chip; ↑ from an empty bar
    /// focuses the last drawn list stop. Typing on a row returns to the box.
    pub(super) fn home_screen_key(&mut self, key: &Key) -> Option<Effect> {
        if !self.on_home() {
            return None;
        }
        // Esc on home goes back to the session home came from, with a
        // chip or a row focused as well as from the entry bar.
        if matches!(key, Key::Esc)
            && let Some(effect) = self.go_back()
        {
            return Some(effect);
        }
        // A focused chip takes its keys ahead of focus: arrows move,
        // Enter clicks, typing returns to the entry bar. `None` passes
        // the key on.
        if self.focused_chip().is_some() {
            return self.chip_key(key);
        }
        // With the box focused, j is still draft text. On a row, j/k
        // remain focus movement keys rather than draft text.
        if matches!(key, Key::Char('j')) && self.focus.is_none() {
            return None;
        }
        if matches!(key, Key::Up | Key::Down) {
            if self.focus.is_some() {
                if let Some(effect) = self.list_key(key) {
                    return Some(effect);
                }
                return None;
            }
            return if matches!(key, Key::Up) {
                self.entry_up()
            } else {
                self.entry_down()
            };
        }
        if !matches!(key, Key::Char('j')) {
            if matches!(key, Key::Backspace) {
                return self.delete_key();
            }
            if matches!(key, Key::Char(ch) if !matches!(ch, 'k' | 'y'))
                && matches!(
                    self.focus,
                    Some(TargetId::Home(
                        Spot::Entry(_) | Spot::Stop(_) | Spot::More(_)
                    ))
                )
            {
                self.focus = None;
            }
            return None;
        }
        // j follows the drawn list order; when it reaches the last drawn
        // stop it advances into the next logical row, or pages recents.
        let Some(TargetId::Home(spot @ (Spot::Entry(_) | Spot::Stop(_) | Spot::More(_)))) =
            self.focus
        else {
            return None;
        };
        let current = spot.list_stop()?;
        if matches!(spot, Spot::Entry(key) if self.stops.iter().any(|target| target.id == TargetId::Home(Spot::Stop(key))))
            || self.list_stops().last().copied() != Some(current)
        {
            return None;
        }
        let stops = self.all_list_stops();
        let at = stops.iter().position(|stop| *stop == current)?;
        if let Some(next) = stops.get(at.saturating_add(1)) {
            self.focus = Some(TargetId::Home(*next));
            return Some(Effect::None);
        }
        if let Spot::Entry(key) = current
            && let Some(effect) = self.page_recent(key)
        {
            return Some(effect);
        }
        self.focus = None;
        Some(Effect::None)
    }

    /// Clicks `spot` on home: a row opens its session, its ✕ stops a
    /// live session or asks to delete an exited one, a `More` row reveals
    /// its project's loaded exited rows, the workspace chip opens the
    /// picker, the worktree switch toggles it, the model chip opens the model picker, the
    /// thinking chip opens it at the model's thinking chips, a picker
    /// row chooses its workspace, and a quit choice does what its key
    /// does while the quit question is open, on home or over a session.
    pub(super) fn home_click(&mut self, spot: Spot) -> Effect {
        if let Spot::Quit(choice) = spot {
            return self.quit_click(choice);
        }
        if let Spot::Clear(choice) = spot {
            return self.clear_click(choice);
        }
        match spot {
            Spot::Entry(key) => self.open_row(key),
            Spot::Stop(key) => self.stop_or_ask(key),
            Spot::Workspace => self.open_picker(),
            Spot::Worktree => self.toggle_worktree(),
            Spot::Model => self.open_model_picker(crate::model_picker::Mode::Choose),
            Spot::Thinking => self.open_thinking(),
            Spot::Pick(at) => self.pick(at),
            Spot::More(key) => {
                let project = self
                    .home
                    .as_ref()
                    .and_then(|home| home.sessions.by_key(key))
                    .map(|row| row.project.clone());
                if let Some(project) = project
                    && let Some(home) = self.home.as_mut()
                {
                    home.sessions.expand(&project);
                    self.focus = Some(TargetId::Home(Spot::Entry(key)));
                }
                Effect::None
            }
            Spot::Keep => {
                self.close_delete_prompt();
                self.close_import_prompt();
                Effect::None
            }
            Spot::Question => Effect::None,
            Spot::Start(key) => {
                let workspace = self
                    .home
                    .as_ref()
                    .and_then(|home| home.sessions.by_key(key))
                    .map(|row| row.workspace.clone())
                    .filter(|workspace| !workspace.is_empty());
                if let Some(workspace) = workspace {
                    self.choose_workspace(workspace);
                    self.focus = None;
                }
                Effect::None
            }
            Spot::Clear(_) | Spot::Quit(_) => Effect::None,
        }
    }

    /// An edit on home: Delete asks to stop a live row or delete an
    /// exited one; otherwise the
    /// picker and chip row get first refusal, and unhandled edits reach the
    /// draft. The workspace picker owns every edit while open, so
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
        // `→` with no chip focused goes back to the session home came
        // from; with a chip focused the chip row keeps it.
        if matches!(edit, Edit::Right)
            && self.focused_chip().is_none()
            && let Some(effect) = self.go_back()
        {
            return Some(effect);
        }
        match edit {
            Edit::Delete => self.delete_or_stop_key(),
            Edit::Left
            | Edit::Right
            | Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::DeleteToLineStart
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Paste(_) => None,
        }
    }

    /// Asks the first unscoped `recent` page again: an accepted delete
    /// fills the gap it left.
    fn ask_recent_first(&mut self) -> Vec<String> {
        if !self.outbox.is_up() {
            return Vec::new();
        }
        let Some(home) = self.home.as_mut() else {
            return Vec::new();
        };
        let id = mint();
        home.recent_ask = Some((id.clone(), true));
        vec![recent_line(&id, None)]
    }

    /// Asks the next unscoped `recent` page past the focused last row,
    /// using its id as `before`. `None` unless the focused row ends the
    /// list and is its last recent row, the last
    /// answer was not empty, and no `recent` is in flight.
    pub(super) fn page_recent(&mut self, focused: u64) -> Option<Effect> {
        let home = self.home.as_mut()?;
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
        Some(Effect::Send(vec![recent_line(&id, Some(before.as_str()))]))
    }

    /// The text y copies and Ctrl+G opens for `spot`: the row's line;
    /// controls copy nothing.
    pub(super) fn home_text(&self, spot: Spot) -> Option<String> {
        match spot {
            Spot::Entry(key) => {
                let home = self.home.as_ref()?;
                let row = home.sessions.by_key(key)?;
                Some(line(row, home.launch_project()))
            }
            Spot::More(key) => {
                let home = self.home.as_ref()?;
                let row = home.sessions.by_key(key)?;
                let section = home
                    .sessions
                    .sections(home.launch_project())
                    .into_iter()
                    .find(|section| section.project == row.project)?;
                let hidden = section.hidden();
                (hidden > 0).then(|| format!("… {hidden} more"))
            }
            Spot::Stop(_)
            | Spot::Start(_)
            | Spot::Workspace
            | Spot::Worktree
            | Spot::Model
            | Spot::Thinking
            | Spot::Pick(_)
            | Spot::Keep
            | Spot::Question
            | Spot::Quit(_)
            | Spot::Clear(_) => None,
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
                // An empty pick or launch directory is none: no `workspace`
                // key names a remote home, as the hub treats a missing key.
                let workspace = home
                    .chooser
                    .chosen()
                    .filter(|chosen| !chosen.is_empty())
                    .map(str::to_owned)
                    .or_else(|| {
                        home.launch
                            .location
                            .workspace()
                            .filter(|workspace| !workspace.as_os_str().is_empty())
                            .map(|workspace| workspace.display().to_string())
                    });
                let mut args = json!({});
                if let (Some(workspace), Some(object)) = (workspace, args.as_object_mut()) {
                    object.insert("workspace".to_owned(), Value::String(workspace));
                }
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
            None => {
                let mut args = json!({});
                if let (Some(workspace), Some(object)) =
                    (self.workspace.clone(), args.as_object_mut())
                {
                    object.insert(
                        "workspace".to_owned(),
                        Value::String(workspace.display().to_string()),
                    );
                }
                args
            }
        }
    }
}

/// A `recent` line: the first page, or past `before`.
fn recent_line(id: &str, before: Option<&str>) -> String {
    match before {
        Some(before) => {
            json!({"id": id, "command": "recent", "args": {"before": before}}).to_string()
        }
        None => json!({"id": id, "command": "recent"}).to_string(),
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
