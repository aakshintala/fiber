//! Home's data: the session rows, the subscriptions and what home draws (`docs/tui.md`, "Home").

use std::collections::HashMap;
use std::path::PathBuf;

use contract::events::{SessionState, SessionStatus, WaitingKind};
use contract::{Envelope, SessionId};
use ratatui::text::Line;
use serde_json::Value;

/// A click target on home: one row's line, by the key [`Sessions`]
/// gave it when it first appeared, its ✕ in the last column, the scope
/// toggle heading the list, the workspace chip opening the workspace
/// picker, the new worktree switch, the model chip opening the model
/// picker, the thinking chip opening it at the model's thinking chips,
/// one picker row by its index in the current picker list, one
/// quit choice while the quit question is open, the delete question's
/// ✕, and the delete frame itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Spot {
    /// A session row: clicking it, or Enter on it while focused, opens
    /// the session.
    Entry(u64),
    /// A row's ✕ in its last column: stopping a live session, or the
    /// delete question for an exited one. Unreadable rows have none.
    Stop(u64),
    /// Start a session in the section's workspace.
    Start(u64),
    /// Reveal the remaining exited sessions in this project.
    More(u64),
    /// The workspace chip: clicking it opens the workspace picker.
    Workspace,
    /// The new worktree switch: clicking it, or Enter on it while
    /// focused, toggles it.
    Worktree,
    /// The model chip: clicking it, or Enter on it while focused,
    /// opens the model picker in `Mode::Choose`.
    Model,
    /// The thinking chip: clicking it, or Enter on it while focused,
    /// opens the model picker at the model's thinking chips.
    Thinking,
    /// A picker row, by its index in the current picker list.
    Pick(usize),
    /// A quit choice while the quit question is open: clicking it does
    /// what its key does.
    Quit(QuitChoice),
    /// A clear choice while the clear question is open: clicking it
    /// does what its key does.
    Clear(ClearChoice),
    /// The delete question's ✕: clicking it does what Esc does, keeping
    /// the session.
    Keep,
    /// The delete frame itself: a click on it does nothing, so no click
    /// passes through to the rows under it.
    Question,
}

impl Spot {
    /// Whether the spot is a list stop: an entry and its cross normalize
    /// to the entry, and `More` stays its own stop.
    pub(crate) fn list_stop(self) -> Option<Spot> {
        match self {
            Spot::Entry(key) | Spot::Stop(key) => Some(Spot::Entry(key)),
            Spot::More(key) => Some(Spot::More(key)),
            Spot::Start(_)
            | Spot::Workspace
            | Spot::Worktree
            | Spot::Model
            | Spot::Thinking
            | Spot::Pick(_)
            | Spot::Quit(_)
            | Spot::Clear(_)
            | Spot::Keep
            | Spot::Question => None,
        }
    }

    /// Whether the spot is a chip of the chip row: the workspace chip,
    /// the new worktree switch, the model chip or the thinking chip.
    pub(crate) fn is_chip(self) -> bool {
        match self {
            Spot::Workspace | Spot::Worktree | Spot::Model | Spot::Thinking => true,
            Spot::Entry(_)
            | Spot::Stop(_)
            | Spot::Start(_)
            | Spot::More(_)
            | Spot::Pick(_)
            | Spot::Keep
            | Spot::Question
            | Spot::Quit(_)
            | Spot::Clear(_) => false,
        }
    }
}

/// A clear choice: clearing anyway, or staying. Enter clears with the
/// staged `now`, Esc stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClearChoice {
    /// Enter: clear anyway.
    Clear,
    /// Esc, or the title's ✕: stay.
    Stay,
}

/// A quit choice: leaving working sessions running, closing them all
/// now, or staying. `CloseAll` and `Stay` double as the close-all
/// question's confirm and cancel rows; `Leave` draws no row there and
/// does nothing in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuitChoice {
    /// Enter: leave them running.
    Leave,
    /// `c`: close all now.
    CloseAll,
    /// Esc, or the title's ✕: stay.
    Stay,
}

/// The terminal's local launch directory or its listed hub.
#[derive(Debug, PartialEq, Eq)]
pub enum LaunchLocation {
    /// A local terminal has a launch workspace and project.
    Local {
        /// The launch directory; `start`'s default workspace.
        workspace: PathBuf,
        /// Its project key (`docs/state.md`, "Projects").
        project: String,
        /// The launch directory is inside a git repository.
        git: bool,
    },
    /// A listed hub has no client launch workspace or project.
    Remote {
        /// The listed hub's name.
        hub: String,
    },
}

impl Default for LaunchLocation {
    fn default() -> Self {
        Self::Local {
            workspace: PathBuf::new(),
            project: String::new(),
            git: false,
        }
    }
}

impl LaunchLocation {
    /// The launch directory: a listed hub has none.
    pub(crate) fn workspace(&self) -> Option<&std::path::Path> {
        match self {
            Self::Local { workspace, .. } => Some(workspace),
            Self::Remote { .. } => None,
        }
    }

    /// The launch directory's project key: a listed hub has none.
    pub(crate) fn project(&self) -> Option<&str> {
        match self {
            Self::Local { project, .. } => Some(project),
            Self::Remote { .. } => None,
        }
    }

    pub(crate) fn hub(&self) -> Option<&str> {
        match self {
            Self::Local { .. } => None,
            Self::Remote { hub } => Some(hub),
        }
    }
}

/// What the terminal knows about where it was launched, with settings
/// shared by local and remote terminals.
#[derive(Default)]
pub struct Launch {
    /// The launch workspace or listed hub.
    pub location: LaunchLocation,
    /// Where the terminal opens (`docs/invocation.md`, "Commands and flags").
    pub open_at: crate::OpenAt,
    /// `tui.hover`: with it off, mouse mode 1003 is never sent and nothing
    /// is tinted under the pointer.
    pub hover: bool,
    /// `tui.reduced_motion`: spinners, the glimmer and the pulse stay
    /// still, while the time and the countdown keep a one-second wake
    /// (`docs/tui.md`, "The working line").
    pub reduced_motion: bool,
    /// Fiber's version, for the logo.
    pub version: String,
    /// The commit Fiber was built from, when the build recorded one:
    /// compared with the hub's (`docs/invocation.md`, "What the hub speaks").
    pub commit: Option<String>,
    /// `model`: the chip's model, unset on a fresh install with no key.
    pub model: Option<String>,
    /// Bare `fiber`'s `--model` and `-c`, normalized into the per-run
    /// layer: each `start` carries them in `overrides`
    /// (`docs/invocation.md`, "Sessions"). Empty in the jigs, which
    /// launch with no flags.
    pub overrides: Vec<String>,
    /// `thinking`: the chip's level, unset for the default.
    pub thinking: Option<String>,
    /// `tui.logo_glyph`: the one-row logo's mark, "⌇" or "≈". The
    /// four-row logo's wave is drawn pixels, and never changes.
    pub logo_glyph: String,
    /// Where the viewer writes the copies it opens: `cache/images` in
    /// Fiber home (`docs/tui.md`, "Images").
    pub images: PathBuf,
    /// `tui.inline_images`: images draw inline where the terminal
    /// speaks kitty's graphics protocol (`docs/tui.md`, "Images").
    /// False in the default, so tests opt in per case.
    pub inline_images: bool,
    /// `tui.rail.width`: the rail's share of the screen's width, in
    /// percent (`docs/tui.md`, "Layout").
    pub rail_share: f64,
    /// `tui.rail.shown`: false hides the rail until the person shows it
    /// (`docs/tui.md`, "The rail").
    pub rail_hidden: bool,
    /// `tui.panel.width`: the panel's share of the screen's width, in
    /// percent.
    pub panel_share: f64,
    /// `tui.panel.cards`: the panel's cards, in order (`docs/tui.md`,
    /// "The panel").
    pub panel_cards: Vec<String>,
    /// The person's `keys` (`docs/configuration.md`, "Keys"): the
    /// effective bindings overlay it onto the defaults at startup.
    pub keys: crate::KeysSetup,
    /// `tui.theme`: the theme, or following the terminal's appearance.
    pub theme: crate::ThemeSetting,
    /// `tui.attention.*`: the notification, the bell and the title
    /// (`docs/tui.md`, "Getting the person's attention").
    pub attention: crate::Attention,
    /// How the terminal reads the installed models: the cached lists at
    /// once, refreshed in the background (`docs/model-routing.md`,
    /// "Model discovery"). `None` in the jigs, which read nothing.
    pub models: Option<crate::ReadModels>,
    /// Writes a feedback report for a description: the injected writer
    /// `main` supplies (`docs/user/reporting.md`). `None` in the jigs,
    /// which write nothing.
    pub feedback: Option<crate::WriteFeedback>,
    /// `scoped_models`: the references the model picker shows; empty
    /// means every installed model (`docs/configuration.md`, "Keys").
    pub scoped_models: Vec<String>,
    /// The seam the configuration views read and write through
    /// (`docs/tui.md`, "Swapped views"); with none, each says it is not
    /// available.
    pub configure: Option<std::sync::Arc<dyn crate::Configure>>,
    /// Loads terminal-home renderers on the terminal thread.
    pub extensions: Option<std::sync::Arc<dyn crate::ext::Load>>,
}

/// One chip on home's chip row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Chip {
    /// What a click on it opens, if anything.
    pub(crate) spot: Option<Spot>,
    /// It draws dim: the workspace chip while no workspace is chosen.
    pub(crate) dim: bool,
    /// Its text.
    pub(crate) text: String,
}

/// What home draws, built by [`crate::app::App::home_screen`].
pub(crate) struct HomeScreen {
    /// Fiber's version, for the logo.
    pub(crate) version: String,
    /// The glyph before the name in the one-row logo.
    pub(crate) glyph: String,
    /// The chip row, left to right.
    pub(crate) chips: Vec<Chip>,
    /// Counts across all live sessions and the summed spend.
    pub(crate) header: Header,
    /// Project sections in display order.
    pub(crate) list: Vec<ListLine>,
    /// No live, crashed or exited session exists.
    pub(crate) empty: bool,
    /// The rejected `start`'s message, above the box until the next
    /// `start` goes out.
    pub(crate) blockers: Vec<String>,
    /// The workspace picker above the box: its current recents,
    /// the focused row, and the typed path once a key opens its row.
    pub(crate) picker: Option<PickerScreen>,
    /// The foot hint, or the quit hint while Ctrl+C is armed: each
    /// key bold, its label and every joiner dim.
    pub(crate) foot: Line<'static>,
    /// The delete question, unwrapped: the frame wraps its head naming
    /// the session and everything `--cascade` adds, and its body
    /// scrolls past the frame.
    pub(crate) question: Option<String>,
    /// The question is the first-run import offer: the import frame
    /// draws it, never the delete frame.
    pub(crate) question_import: bool,
    /// The delete question's scroll offset: Up shows earlier rows,
    /// Down later ones.
    pub(crate) question_scroll: usize,
    /// The input box shows its placeholder.
    pub(crate) placeholder: bool,
}

/// The header's nonzero state counts in display order.
pub(crate) struct Header {
    pub(crate) counts: Vec<(crate::theme::Role, String)>,
    pub(crate) spend: Option<String>,
}

/// A logical row in a project section.
pub(crate) enum ListLine {
    Head {
        start: Option<u64>,
        name: String,
        live: usize,
        spend: String,
        waiting: usize,
    },
    Live {
        key: u64,
        number: Option<usize>,
        glyph: String,
        name: String,
        word: &'static str,
        tone: crate::theme::Role,
        since: String,
        stop: bool,
        id: String,
        turns: Option<u64>,
        branch: Option<String>,
        spend: String,
        context: Option<u64>,
        wait: Option<String>,
    },
    ExitedHead {
        count: usize,
    },
    Exited {
        key: u64,
        glyph: &'static str,
        name: String,
        id: String,
        turns: Option<u64>,
        spend: String,
        age: Option<String>,
    },
    More {
        key: u64,
        count: usize,
    },
}

/// What the open workspace picker draws (`docs/tui.md`, "Home").
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PickerScreen {
    /// Esc may close the picker because a workspace is already in use.
    pub(crate) escapes: bool,
    /// The recent workspaces, refreshed until a key touches the picker.
    pub(crate) recents: Vec<String>,
    /// The focused row's index over the completions then the recents
    /// while typing, over the recents alone otherwise; `None` with
    /// nothing focused.
    pub(crate) focus: Option<usize>,
    /// The typed path's row, once a key opens it.
    pub(crate) typed: Option<TypedScreen>,
}

/// The picker's typed row: its text, the current directory's matching
/// names, and whether Enter found no directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TypedScreen {
    /// The typed text.
    pub(crate) text: String,
    /// The current directory's names matching the text.
    pub(crate) completions: Vec<String>,
    /// Enter found no directory: the list shows "no such directory".
    pub(crate) missing: bool,
}

/// A subscription level on this connection: `summary` carries only the
/// latest `session_status`, `full` folds the whole stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Level {
    /// Only the latest status.
    Summary,
    /// The whole stream.
    Full,
}

/// What this connection holds, or will hold once its in-flight
/// subscribes are answered, per session. The accepted level changes
/// only on acceptance: answering removes the id from the in-flight
/// list either way, and a rejection leaves the accepted level.
#[derive(Default)]
pub(crate) struct Subs {
    accepted: HashMap<SessionId, Level>,
    inflight: Vec<(String, SessionId, Level)>,
}

impl Subs {
    /// Records a subscribe sent with `id` for `session` at `level`.
    pub(crate) fn sent(&mut self, id: String, session: SessionId, level: Level) {
        self.inflight.push((id, session, level));
    }

    /// The level a new subscribe for `session` must differ from: the
    /// last in-flight subscribe's level, else the accepted one.
    pub(crate) fn expected(&self, session: &SessionId) -> Option<Level> {
        self.inflight
            .iter()
            .rev()
            .find(|(_, id, _)| id == session)
            .map(|(_, _, level)| *level)
            .or_else(|| self.accepted.get(session).copied())
    }

    /// The last in-flight subscribe's id when it asks for `summary`.
    pub(crate) fn pending_summary(&self, session: &SessionId) -> Option<&str> {
        self.inflight
            .iter()
            .rev()
            .find(|(_, id, _)| id == session)
            .filter(|(_, _, level)| *level == Level::Summary)
            .map(|(id, _, _)| id.as_str())
    }

    /// Answers the in-flight subscribe `id`: on acceptance the level
    /// becomes the accepted one. Some when the id was in flight, with
    /// its session, either way.
    pub(crate) fn answered(&mut self, id: &str, accepted: bool) -> Option<SessionId> {
        let at = self.inflight.iter().position(|(sent, _, _)| sent == id)?;
        let (_, session, level) = self.inflight.remove(at);
        if accepted {
            self.accepted.insert(session.clone(), level);
        }
        Some(session)
    }

    /// Whether the accepted level for `session` is `full`.
    pub(crate) fn full(&self, session: &SessionId) -> bool {
        self.accepted.get(session) == Some(&Level::Full)
    }

    /// Whether a subscribe for `session` waits for its answer.
    pub(crate) fn pending(&self, session: &SessionId) -> bool {
        self.inflight.iter().any(|(_, id, _)| id == session)
    }
}

/// The subscribes opening a session sends from `expected`: the first
/// stage is one `summary`, unless the connection already holds
/// `summary`, when no subscribe goes out and the raise follows the
/// first frame at once. The `full` raise follows the first frame in
/// every case, sent by `take_raise` after `draw`.
const SUMMARY_ONLY: [Level; 1] = [Level::Summary];

pub(crate) fn opening(expected: Option<Level>) -> &'static [Level] {
    match expected {
        Some(Level::Summary) => &[],
        _ => &SUMMARY_ONLY,
    }
}

/// What a session is doing, as its row's glyph names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// A turn is running.
    Working,
    /// A model call waits to retry.
    Retrying,
    /// Waiting on the person.
    Waiting,
    /// No turn is running, and jobs are.
    Jobs,
    /// Nothing is in flight.
    Idle,
    /// The status cannot be read: its schema is newer, or it does not
    /// parse. The row says CANNOT ATTACH and why ("The session list").
    Unreadable,
}

/// How a session ended, as its row's glyph names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Left {
    /// Its log ends in `fiber_exited` or `rewound`.
    Exited,
    /// The process died.
    Crashed,
}

/// One session on home: a feed row while live, a recent row once exited.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Row {
    /// The click target's key, set by [`Sessions`]: unique, never reused,
    /// kept through status updates, `left`, and a move from `recent` to
    /// the feed.
    pub(crate) key: u64,
    /// The session.
    pub(crate) id: SessionId,
    /// Its name, or its first prompt when it has none; the id when empty.
    pub(crate) name: String,
    /// The workspace path.
    pub(crate) workspace: String,
    /// The session runs in a git repository: its last status carried
    /// `git`. An unreadable row never does.
    pub(crate) git: bool,
    /// The project's key.
    pub(crate) project: String,
    /// What the session is doing.
    pub(crate) state: State,
    /// How it ended; `None` while live.
    pub(crate) left: Option<Left>,
    /// What it waits on: "approval: \<summary\>" or "question:
    /// \<summary\>". A left row whose last status waited keeps it.
    pub(crate) waiting: Option<String>,
    /// Its spend so far, in US dollars; left out of the line at zero.
    pub(crate) spend: f64,
    /// Jobs running, delegates excluded: an idle row with jobs counts as
    /// working for the quit question.
    pub(crate) jobs: u32,
    /// Delegates running: an idle row with delegates counts as working.
    pub(crate) delegates: u32,
    /// The `full` connections attached, as the latest `clients` line
    /// counts them: less this connection's own, how many open elsewhere.
    pub(crate) clients: u32,
    /// What a refusal says about the row, in place of its detail.
    pub(crate) note: Option<String>,
    /// Its latest status; none for an unreadable row.
    pub(crate) status: Option<SessionStatus>,
}

/// The session list: live rows in feed order, then exited rows newest
/// first. A session id is in at most one of the two.
#[derive(Default)]
pub(crate) struct Sessions {
    feed: Vec<Row>,
    recent: Vec<Row>,
    next_key: u64,
    /// Projects whose loaded exited rows are expanded.
    expanded: std::collections::BTreeSet<String>,
    /// The last unscoped `recent` answer carried rows: an older unscoped
    /// page may exist. Scoped pages never touch it.
    more: bool,
    /// The hub's scoped `recent` total per project: the only source of a
    /// project's exited count. The unscoped answer's total is never
    /// stored here.
    totals: HashMap<String, usize>,
    /// Projects with a trigger while their scoped ask is in flight: the
    /// answer sends exactly one follow-up and clears the flag.
    dirty: std::collections::BTreeSet<String>,
    /// The last row of the unscoped `recent` pages: `page_recent` sends
    /// its id as `before`. Scoped folds and pages never move it.
    unscoped_last: Option<SessionId>,
}

/// One project's live sessions and loaded exited sessions, in display order.
pub(crate) struct Section<'a> {
    pub(crate) project: &'a str,
    pub(crate) name: String,
    pub(crate) start: Option<u64>,
    pub(crate) live: Vec<&'a Row>,
    pub(crate) exited: Vec<&'a Row>,
    pub(crate) shown_exited: usize,
    /// The hub's scoped total for the project: `None` means count the
    /// loaded rows.
    pub(crate) total: Option<usize>,
}

impl Section<'_> {
    /// Loaded exited rows that the collapsed section hides.
    pub(crate) fn hidden(&self) -> usize {
        self.exited.len() - self.shown_exited
    }
}

impl Sessions {
    /// All rows, feed first and then recent, without a project filter.
    pub(crate) fn all(&self) -> Vec<&Row> {
        self.feed.iter().chain(self.recent.iter()).collect()
    }

    /// Reveal every loaded exited row of this project for this terminal run.
    pub(crate) fn expand(&mut self, project: &str) {
        self.expanded.insert(project.to_owned());
    }

    /// Project sections: launch first, live projects in feed order, exited-only
    /// projects by their earliest loaded start, with loaded order breaking ties.
    pub(crate) fn sections(&self, launch_project: Option<&str>) -> Vec<Section<'_>> {
        let mut sections: Vec<Section<'_>> = Vec::new();
        for row in self.all() {
            if sections
                .iter()
                .any(|section| section.project == row.project)
            {
                continue;
            }
            let live = self
                .feed
                .iter()
                .filter(|r| r.project == row.project && r.left != Some(Left::Exited))
                .collect();
            let exited: Vec<&Row> = self
                .feed
                .iter()
                .rev()
                .filter(|r| r.project == row.project && r.left == Some(Left::Exited))
                .chain(self.recent.iter().filter(|r| r.project == row.project))
                .collect();
            let shown_exited = if self.expanded.contains(&row.project) {
                exited.len()
            } else {
                exited.len().min(3)
            };
            let start = self
                .all()
                .into_iter()
                .find(|r| r.project == row.project && !r.workspace.is_empty())
                .map(|r| r.key);
            let name = std::path::Path::new(&row.workspace)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| !n.is_empty())
                .unwrap_or(&row.project)
                .to_owned();
            let total = self.totals.get(&row.project).copied();
            sections.push(Section {
                project: &row.project,
                name,
                start,
                live,
                exited,
                shown_exited,
                total,
            });
        }
        sections.sort_by_key(|section| {
            let category = if Some(section.project) == launch_project {
                0
            } else if !section.live.is_empty() {
                1
            } else {
                2
            };
            let started = if category == 2 {
                section
                    .exited
                    .iter()
                    .filter_map(|r| r.status.as_ref().map(|s| s.started))
                    .min()
            } else {
                None
            };
            (category, started.is_none(), started)
        });
        sections
    }

    /// Folds a `session_status` into the rows: an id already listed keeps
    /// its position and its key, a recent row moves into the feed, and a
    /// new id goes after the rows already there. A status means the
    /// session is live, so it clears `left`: a resumed session. A status
    /// is a late copy, and changes nothing, when it began before the one the
    /// row holds, or in the same millisecond while the row waits and it does
    /// not (the feed and the `full` subscription each deliver every status,
    /// by separate writers, and a tool call and its approval can share a
    /// millisecond).
    pub(crate) fn status(&mut self, row: Row) {
        if let Some(existing) = self.feed.iter_mut().find(|feed| feed.id == row.id) {
            if let (Some(held), Some(new)) = (existing.status.as_ref(), row.status.as_ref())
                && (new.since < held.since
                    || (new.since == held.since
                        && matches!(held.state, SessionState::Waiting { .. })
                        && !matches!(new.state, SessionState::Waiting { .. })))
            {
                return;
            }
            let key = existing.key;
            *existing = row;
            existing.key = key;
            existing.left = None;
            return;
        }
        if let Some(at) = self.recent.iter().position(|recent| recent.id == row.id) {
            let key = self.recent.get(at).map_or(0, |recent| recent.key);
            self.recent.remove(at);
            let mut row = row;
            row.key = key;
            row.left = None;
            self.feed.push(row);
            return;
        }
        let mut row = row;
        row.key = self.fresh();
        row.left = None;
        self.feed.push(row);
    }

    /// Marks `id` left, keeping its position; an unknown id changes
    /// nothing.
    pub(crate) fn left(&mut self, id: &SessionId, how: Left) {
        for row in self.feed.iter_mut().chain(self.recent.iter_mut()) {
            if row.id == *id {
                row.left = Some(how);
                return;
            }
        }
    }

    /// Folds an unscoped `recent` page into the rows: the first page
    /// replaces them, an older page appends. Neither adds an id already
    /// in the feed or, for an older page, already listed; a first page
    /// listing an id again keeps its key. An empty answer ends paging.
    /// The only writer of the unscoped cursor and `more()`: scoped folds
    /// never touch either. The answer's `total` is never stored here.
    pub(crate) fn recent(&mut self, rows: Vec<Row>, first: bool) {
        self.more = !rows.is_empty();
        if first {
            let old = std::mem::take(&mut self.recent);
            let mut kept = Vec::with_capacity(rows.len());
            for mut row in rows {
                if self.feed.iter().any(|feed| feed.id == row.id)
                    || kept.iter().any(|keep: &Row| keep.id == row.id)
                {
                    continue;
                }
                row.key = old
                    .iter()
                    .find(|recent| recent.id == row.id)
                    .map_or_else(|| self.fresh(), |recent| recent.key);
                kept.push(row);
            }
            self.recent = kept;
            // A first page replaces every row, scoped ones included, so
            // the cursor is the kept page's last row.
            self.unscoped_last = self.recent.last().map(|row| row.id.clone());
        } else {
            // An older page appends past the scoped rows, so the cursor
            // is the appended page's last row; an empty page keeps it.
            let mut last = None;
            for mut row in rows {
                if self.feed.iter().any(|feed| feed.id == row.id)
                    || self.recent.iter().any(|recent| recent.id == row.id)
                {
                    continue;
                }
                row.key = self.fresh();
                last = Some(row.id.clone());
                self.recent.push(row);
            }
            if last.is_some() {
                self.unscoped_last = last;
            }
        }
    }

    /// Appends a project-scoped `recent` page: rows of other projects
    /// stay untouched. A feed duplicate is skipped, as is an id an
    /// earlier page already listed, so scoped pages accumulate newest
    /// first. Never touches the unscoped cursor or `more()`.
    pub(crate) fn scoped(&mut self, rows: Vec<Row>) {
        // The hub already scoped the page to the project, so the fold
        // only deduplicates.
        for mut row in rows {
            if self.feed.iter().any(|feed| feed.id == row.id)
                || self.recent.iter().any(|recent| recent.id == row.id)
            {
                continue;
            }
            row.key = self.fresh();
            self.recent.push(row);
        }
    }

    /// Stores a scoped answer's total keyed by project: the only source
    /// of that project's exited count.
    pub(crate) fn note_total(&mut self, project: &str, total: usize) {
        self.totals.insert(project.to_owned(), total);
    }

    /// The stored scoped total for `project`: `None` means count the
    /// loaded rows.
    pub(crate) fn total_for(&self, project: &str) -> Option<usize> {
        self.totals.get(project).copied()
    }

    /// Drops every held total and dirty flag, on a reconnect: pending
    /// scoped ids match nothing afterwards. Rows stay.
    pub(crate) fn drop_all_totals(&mut self) {
        self.totals.clear();
        self.dirty.clear();
    }

    /// Marks `project` dirty: a trigger arrived while its scoped ask is
    /// in flight, so the answer sends exactly one follow-up. The caller
    /// marks only while an ask for the project is in flight.
    pub(crate) fn mark_dirty(&mut self, project: &str) {
        self.dirty.insert(project.to_owned());
    }

    /// Clears `project`'s dirty flag: its follow-up went out, or its
    /// answer is dropped without one.
    pub(crate) fn clear_dirty(&mut self, project: &str) {
        self.dirty.remove(project);
    }

    /// Whether `project` owes exactly one follow-up scoped ask.
    pub(crate) fn is_dirty(&self, project: &str) -> bool {
        self.dirty.contains(project)
    }

    /// The live feed rows, in feed order.
    pub(crate) fn live(&self) -> Vec<&Row> {
        self.feed.iter().filter(|row| row.left.is_none()).collect()
    }

    /// The feed rows the rail draws as cards: live and crashed, not
    /// exited and never closing, in feed order (`docs/tui.md`, "The
    /// rail"). A closing row has no card and no ⌥N until it exits.
    pub(crate) fn cards(&self) -> Vec<&Row> {
        self.feed
            .iter()
            .filter(|row| {
                row.left != Some(Left::Exited)
                    && !row.status.as_ref().is_some_and(|status| status.closing)
            })
            .collect()
    }

    /// Whether an older unscoped `recent` page may exist: the last
    /// unscoped answer carried rows.
    pub(crate) fn more(&self) -> bool {
        self.more
    }

    /// The last row of the unscoped `recent` pages, whose id pages the
    /// older unscoped rows. Scoped pages never move it.
    pub(crate) fn last_recent(&self) -> Option<&Row> {
        self.unscoped_last
            .as_ref()
            .and_then(|id| self.recent.iter().find(|row| &row.id == id))
    }

    /// The project of the newest row in this workspace, first row on ties.
    pub(crate) fn project_of(&self, workspace: &str) -> Option<&str> {
        self.all()
            .into_iter()
            .filter(|row| row.workspace == workspace)
            // min_by_key keeps the first row on equal timestamps.
            .min_by_key(|row| std::cmp::Reverse(row.status.as_ref().map(|status| status.since)))
            .map(|row| row.project.as_str())
    }

    /// Whether any row, live or exited, is in `workspace` inside git.
    pub(crate) fn in_git(&self, workspace: &str) -> bool {
        self.feed
            .iter()
            .chain(self.recent.iter())
            .any(|row| row.workspace == workspace && row.git)
    }

    /// The row for `id`, live or exited.
    pub(crate) fn row(&self, id: &SessionId) -> Option<&Row> {
        self.feed
            .iter()
            .chain(self.recent.iter())
            .find(|row| row.id == *id)
    }

    /// The row with `key`, live or exited; a key naming no row opens
    /// nothing.
    pub(crate) fn by_key(&self, key: u64) -> Option<&Row> {
        self.feed
            .iter()
            .chain(self.recent.iter())
            .find(|row| row.key == key)
    }

    /// A refusal shows on the row, in place of its detail, as well as a
    /// notice; an unknown id changes nothing.
    pub(crate) fn note(&mut self, id: &SessionId, note: String) {
        for row in self.feed.iter_mut().chain(self.recent.iter_mut()) {
            if row.id == *id {
                row.note = Some(note);
                return;
            }
        }
    }

    /// Drops the row for `id`, live or exited: an accepted delete. A
    /// first-page `recent` asked again fills the gap.
    pub(crate) fn remove(&mut self, id: &SessionId) {
        self.feed.retain(|row| row.id != *id);
        self.recent.retain(|row| row.id != *id);
    }

    /// The next key: unique, never reused, even after a row drops.
    fn fresh(&mut self) -> u64 {
        let key = self.next_key;
        self.next_key = self.next_key.saturating_add(1);
        key
    }
}

/// A `session_status` state as its row's glyph names it, with what it
/// waits on: "approval: \<summary\>" or "question: \<summary\>".
fn state_of(state: &SessionState) -> (State, Option<String>) {
    match state {
        SessionState::Streaming | SessionState::Tool { .. } => (State::Working, None),
        SessionState::Retrying => (State::Retrying, None),
        SessionState::Waiting { waiting } => {
            let kind = match waiting.kind {
                WaitingKind::Approval => "approval",
                WaitingKind::Question => "question",
                WaitingKind::Offer => "offer",
            };
            (State::Waiting, Some(format!("{kind}: {}", waiting.summary)))
        }
        SessionState::Jobs => (State::Jobs, None),
        SessionState::Idle => (State::Idle, None),
    }
}

/// A feed row from a `session_status` envelope. "Cannot read" is a newer
/// `schema_version` than this terminal reads, or a payload that does not
/// parse: the row is [`State::Unreadable`].
pub(crate) fn from_status(envelope: &Envelope) -> Row {
    let status = (envelope.schema_version == contract::SCHEMA_VERSION)
        .then(|| {
            serde_json::from_value::<SessionStatus>(Value::Object(envelope.payload.clone())).ok()
        })
        .flatten();
    let Some(status) = status else {
        return Row {
            key: 0,
            id: envelope.session_id.clone(),
            name: String::new(),
            workspace: String::new(),
            project: String::new(),
            git: false,
            state: State::Unreadable,
            left: None,
            waiting: None,
            spend: 0.0,
            jobs: 0,
            delegates: 0,
            clients: 0,
            note: None,
            status: None,
        };
    };
    let (state, waiting) = state_of(&status.state);
    Row {
        status: Some(status.clone()),
        key: 0,
        id: envelope.session_id.clone(),
        name: status.name,
        workspace: status.workspace,
        project: status.project,
        git: status.git.is_some(),
        state,
        left: None,
        waiting,
        spend: status.spend.cost.unwrap_or(0.0) + status.spend.subscription_cost,
        jobs: status.jobs,
        delegates: status.delegates,
        clients: status.clients,
        note: None,
    }
}

/// The rows of a `recent` answer's `result`: `{"sessions":[…]}`, each
/// with its session, project, workspace, name, how it ended, and its last
/// status when it wrote one, plus the answer's `total`: the hub's count
/// of listed rows, `None` when it carries none or no integer. A row
/// without a session id is skipped.
pub(crate) fn recent_rows(result: &Value) -> (Vec<Row>, Option<usize>) {
    let total = result
        .get("total")
        .and_then(Value::as_u64)
        .and_then(|total| usize::try_from(total).ok());
    let rows = result
        .get("sessions")
        .and_then(Value::as_array)
        .map(|sessions| {
            sessions
                .iter()
                .filter_map(|session| {
                    let id = session.get("session_id").and_then(Value::as_str)?;
                    let status = session.get("status").and_then(|status| {
                        serde_json::from_value::<SessionStatus>(status.clone()).ok()
                    });
                    let (state, waiting, spend) = match &status {
                        None => (State::Idle, None, 0.0),
                        Some(status) => {
                            let (state, waiting) = state_of(&status.state);
                            (
                                state,
                                waiting,
                                status.spend.cost.unwrap_or(0.0) + status.spend.subscription_cost,
                            )
                        }
                    };
                    // A row without a last status ran nothing: no jobs,
                    // delegates or clients.
                    let (jobs, delegates, clients) = match &status {
                        None => (0, 0, 0),
                        Some(status) => (status.jobs, status.delegates, status.clients),
                    };
                    let how = match session.get("how").and_then(Value::as_str) {
                        Some("crashed") => Left::Crashed,
                        _ => Left::Exited,
                    };
                    Some(Row {
                        key: 0,
                        id: SessionId(id.to_owned()),
                        name: session
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        workspace: session
                            .get("workspace")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        git: status.as_ref().is_some_and(|status| status.git.is_some()),
                        project: session
                            .get("project")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        state,
                        left: Some(how),
                        waiting,
                        spend,
                        jobs,
                        delegates,
                        clients,
                        note: None,
                        status,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    (rows, total)
}

/// The quit question's count line: how many sessions work, and of them
/// how many are also open elsewhere. The overlay draws it under its
/// title (`docs/tui.md`, "Quit").
pub(crate) fn quit_count(working: usize, elsewhere: usize) -> String {
    let sessions = if working == 1 {
        "1 session working".to_owned()
    } else {
        format!("{working} sessions working")
    };
    let elsewhere = if elsewhere == 0 {
        String::new()
    } else if elsewhere == 1 {
        ", 1 also open elsewhere".to_owned()
    } else {
        format!(", {elsewhere} also open elsewhere")
    };
    format!("{sessions}{elsewhere}")
}

/// The close-all question's count line: how many live top-level sessions
/// it counted, of them how many work, and how many are also open
/// elsewhere. The overlay draws it under its title (`docs/tui.md`,
/// "Quit").
pub(crate) fn close_all_count(sessions: usize, working: usize, elsewhere: usize) -> String {
    let sessions = if sessions == 1 {
        "1 session".to_owned()
    } else {
        format!("{sessions} sessions")
    };
    format!("{sessions}, {working} working, {elsewhere} open elsewhere")
}

/// A resume line on exit: the session's id and the command resuming
/// it. The terminal is restored first, and one line prints per live
/// session.
pub(crate) fn exit_line(id: &SessionId, hub: Option<&str>) -> String {
    let suffix = hub.map(|hub| format!(" --hub {hub}")).unwrap_or_default();
    format!("{}  fiber resume {}{suffix}", id.0, id.0)
}

/// The delete question on the foot: the session and that deleting is
/// permanent. Deleting through the hub is permanent, so the terminal
/// asks first, naming the session.
pub(crate) fn delete_line(row: &Row) -> String {
    format!(
        "Delete {} ({})? It cannot be undone · Enter delete · Esc keep",
        title(row),
        row.id.0
    )
}

/// The import offer's head: what the import copies and that what is
/// already here stays. The frame below draws the per-source legend
/// and the footer from the offer itself.
pub(crate) fn import_head() -> String {
    "Bring settings from another harness into this home? Skills, commands, instructions and MCP servers copy over; files already here are left behind, never overwritten."
        .to_owned()
}

/// The cascade question on the foot: the session and every session
/// `--cascade` would add, named and counted. The hub names them in its
/// refusal, and the terminal asks again with the set it would remove.
pub(crate) fn cascade_line(row: &Row, others: &[SessionId]) -> String {
    let names: Vec<&str> = others.iter().map(|id| id.0.as_str()).collect();
    let continued = if others.len() == 1 {
        format!("and 1 session that continues it: {}", names.join(", "))
    } else {
        format!(
            "and {} sessions that continue it: {}",
            others.len(),
            names.join(", ")
        )
    };
    format!(
        "Delete {} ({}) {}? It cannot be undone · Enter delete all · Esc keep",
        title(row),
        row.id.0,
        continued
    )
}

/// The row's name as home draws it, or its id when it never had one:
/// each control character draws as a space, so a name never breaks the
/// row's line or the delete question (`docs/tui.md`, "Naming the
/// session"). The stored name keeps its characters.
pub(crate) fn title(row: &Row) -> String {
    if row.name.is_empty() {
        row.id.0.clone()
    } else {
        crate::text::controls_to_space(&row.name)
    }
}

/// The sessions a hub refusal names: every `s_` and 16 lowercase hex
/// token inside backticks, once, in order. Both the dependents refusal
/// and the stale cascade refusal name the root too, so the caller puts
/// the row first and deduplicates keeping first occurrence.
pub(crate) fn dependents(message: &str) -> Vec<SessionId> {
    let mut out = Vec::new();
    // Odd segments sit inside backticks; a trailing one without its
    // closing backtick names nothing.
    let mut segments = message.split('`');
    segments.next();
    while let Some(token) = segments.next() {
        if segments.next().is_none() {
            break;
        }
        let id = token
            .strip_prefix("s_")
            .filter(|hex| {
                hex.len() == 16
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
            .map(|_| SessionId(token.to_owned()));
        if let Some(id) = id
            && !out.contains(&id)
        {
            out.push(id);
        }
    }
    out
}

/// A row's state glyph, the one set home's list, the rail and the
/// terminal title share (`docs/tui.md`, "State glyphs").
pub(crate) fn glyph(row: &Row) -> &'static str {
    // The still form: a working row's spinner draws through the frame
    // clock, which spins the glyph the frame asks for.
    match (&row.left, &row.state) {
        (Some(Left::Exited), _) => "○",
        (Some(Left::Crashed), _) => "✗",
        (None, State::Working | State::Retrying | State::Jobs) => "●",
        (None, State::Waiting) => "!",
        (None, State::Idle) => "✓",
        (None, State::Unreadable) => "?",
    }
}

/// A row's line: the glyph, the name or the id, the detail, and the
/// workspace's last path segment outside the launch project, joined by
/// two spaces with empty parts left out.
pub(crate) fn line(row: &Row, launch_project: Option<&str>) -> String {
    line_with(row, launch_project, glyph(row))
}

/// A row's line with `glyph` drawn first: the frame's spinner while the
/// row works, else the still glyph.
pub(crate) fn line_with(row: &Row, launch_project: Option<&str>, glyph: &str) -> String {
    let name = title(row);
    let mut detail = row.note.clone().unwrap_or_default();
    if detail.is_empty() {
        if row.state == State::Unreadable {
            detail = "cannot attach".to_owned();
        } else {
            let mut parts = Vec::new();
            if let Some(waiting) = &row.waiting {
                parts.push(waiting.clone());
            }
            if row.spend > 0.0 {
                parts.push(crate::format::money(row.spend));
            }
            detail = parts.join(" · ");
        }
    }
    let mut segment = String::new();
    if Some(row.project.as_str()) != launch_project {
        segment = row
            .workspace
            .split('/')
            .rfind(|part| !part.is_empty())
            .unwrap_or_default()
            .to_owned();
    }
    [glyph.to_owned(), name, detail, segment]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("  ")
}

#[cfg(test)]
#[path = "home_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::home_test_rows;
