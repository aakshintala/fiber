//! Terminal state: the draft, the attach phase, the folded stream, the
//! approval queue and the repository offer (`docs/tui.md`, "Turns",
//! "Steering", "Quit", "Approvals and questions", "Approving what a
//! repository ships").

use std::collections::{BTreeSet, HashMap};
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use contract::events::{
    CommandAccepted, CommandRejected, CommandResult, Notice, SessionNamed, ShellCommand,
    SteeringQueue, TurnStarted,
};
use contract::shapes::ContentPart;
use contract::{Envelope, HubLine, Seq, SessionId};
use jiff::tz::TimeZone;
use serde_json::{Map, Value};

use crate::approvals::{self, Panel, PanelKey, Queue};
use crate::ext::{Handle, Renderer};
use crate::home::Level;
use crate::import_report::ImportReport;
use crate::input::Draft;
use crate::keys::Key;
use crate::link::Line;
use crate::shell;
use crate::window::{Applied, Pages};
use notices::Notices;
use overlay::Overlay;
use steering::Steering;

#[cfg(test)]
pub(crate) use notices::FADE;
pub(crate) use notices::NoticeLine;

#[path = "notices.rs"]
mod notices;
#[path = "steering.rs"]
mod steering;

mod attention;
mod chrome;
pub(crate) mod commands;
mod completion;
mod config_views;
#[path = "copy.rs"]
pub(crate) mod copy;
mod delegates;
mod drag;
mod find;
#[path = "app_focus.rs"]
mod focus;
mod form;
#[path = "history.rs"]
mod history;
mod home;
mod images;
pub(crate) mod items;
mod keyboard;
mod links;
pub(crate) mod model_picker;
#[path = "app_mouse.rs"]
mod mouse;
mod moving;
mod offer;
mod outbox;
mod overlay;
pub(crate) mod panel;
mod paste;
pub(crate) mod rail;
mod reconnect;
pub(crate) mod results;
mod screen;
mod select;
mod session_views;
mod status_rows;

use screen::Screen;

pub(crate) use config_views::ConfigView;
pub(crate) use find::{FindBar, Snippet};
pub(crate) use outbox::{Kind, Link, Phase, history_args, mint, session_command};
pub(crate) use session_views::SessionView;

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
    /// Write a feedback report for `description`, off the loop thread:
    /// the worker posts the path with the issue link, or the notice
    /// saying why there is none.
    Feedback {
        /// The description `/feedback` ran with.
        description: String,
    },
    /// Open this file in the editor (`docs/tui.md`, "Swapped views").
    OpenFile(PathBuf),
}

/// What clicking a line, or Enter on it, opens, keyed by an id that stays
/// with the item when a page is folded again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Target {
    /// A tool group's ledger.
    Group(usize),
    /// A call's diff, output or error.
    Call(usize),
    /// A call's whole view, opened from its more row.
    CallView(usize),
    /// A thinking block's text.
    Thought(usize),
    /// The login a failed turn offers.
    Login,
    /// A handoff's note.
    Note(usize),
    /// An image's line: clicking it, or Enter on it while focused,
    /// opens it in the system viewer (`docs/tui.md`, "Images").
    Image(u32),
    /// The jobs a resumed process marked orphaned.
    Orphans(usize),
    /// A reply's `block`th code block's `copy` cells: they copy its code.
    Copy { reply: usize, block: usize },
}

/// An opening pass awaiting its reopening line or final sequence.
enum Pass {
    Pending,
    Until(Seq),
}

/// The terminal's state.
pub(crate) struct App {
    /// Sessions whose opening passes hold history requests.
    passes: Vec<(SessionId, Pass)>,
    /// What an open holds over the attached screen.
    overlay: overlay::Overlay,
    /// History command ids whose late answers are discarded after reopening.
    abandoned_history: BTreeSet<String>,
    /// The launch directory `start` names: none on a remote home, which
    /// carries no local workspace.
    workspace: Option<PathBuf>,
    /// Home's state, once `run` sets it; `None` keeps today's screen.
    home: Option<home::Home>,
    draft: Draft,
    /// The one clipboard image read at a time, and its ticket.
    paste: paste::Paste,
    phase: Phase,
    /// The commands waiting for their answer, with the link they go out
    /// on, the lines held until the hub connects, and the written lines
    /// a reconnect sends again.
    pub(crate) outbox: outbox::Outbox,
    /// The accepted-import report, while open on home or over a
    /// session (`docs/tui.md`, "Notices").
    pub(crate) import_report: Option<ImportReport>,
    /// Every sent `import`'s source by command id: the named source,
    /// or `None` for a bare `/import` covering both. The answer folds
    /// the entry: an acceptance opens the report with it, any other
    /// fold drops it, so a late answer finds nothing.
    import_sources: HashMap<String, Option<String>>,
    /// The attached session's image fetches and queued viewer opens
    /// (`docs/tui.md`, "Images").
    images: images::Images,
    /// The notices floating over the conversation.
    notices: Notices,
    /// The conversation's size, scroll and pages (`docs/tui.md`, "History
    /// and paging").
    screen: Screen,
    /// The installed ledger renderer, if any: shared with the screen's
    /// pages, so one install reaches every page they draw.
    handle: Handle,
    /// Every extension and slot already named by the fallback notice:
    /// each reports once, until the reload command clears the set again.
    renderer_reported: BTreeSet<(String, String)>,
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
    /// The `/` and `@` panels.
    completion: completion::Completion,
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
    /// The drag resizing the rail or the panel, and the shares waiting
    /// to be saved.
    drag: drag::DragState,
    /// The narrow layout's rows below the conversation.
    status_rows: status_rows::StatusRowsState,
    /// The attached session's folded panel data (`docs/tui.md`, "The panel").
    panel_state: panel::PanelState,
    /// Each delegate's latest `session_status`, by session: what the
    /// Delegates card draws while its `summary` subscription lives
    /// (`docs/tui.md`, "The panel").
    delegate_rows: HashMap<SessionId, crate::home::Row>,
    /// The session rail's numbers and wall time (`docs/tui.md`, "The rail").
    rail_state: rail::RailState,
    /// The drag selecting conversation text, and a copy waiting on dropped
    /// pages (`docs/tui.md`, "Selection and copy").
    select: select::Selection,
    /// Conversation search (`docs/tui.md`, "Search").
    find: find::Find,
    /// The effective bindings (`docs/tui.md`, "Bindings").
    keyboard: keyboard::Keyboard,
    /// The model picker: the installed models and the reads it owes.
    model_picker: crate::model_picker::ModelPicker,
    /// Whether a link opener is on `PATH` (`docs/tui.md`, "Links").
    opener: bool,
    /// What the hub's `attention` lines queued (`docs/tui.md`, "Getting
    /// the person's attention").
    attention: attention::State,
    /// The configuration views (`docs/tui.md`, "Swapped views").
    config_views: config_views::ConfigViews,
    /// The attached session's views (`docs/tui.md`, "Swapped views").
    session_views: session_views::SessionViews,
    /// The open delegate or job view and the per-job fold (`docs/tui.md`,
    /// "Swapped views").
    items: items::Items,
    /// Failures since the hub was last reached (`docs/tui.md`, "A dropped
    /// connection").
    reconnect: reconnect::Reconnect,
    /// Bare `fiber`'s `--model` and `-c`, normalized into the per-run
    /// layer: each `start` carries them in `overrides`
    /// (`docs/invocation.md`, "Sessions").
    launch_overrides: Vec<String>,
    /// What moves on screen, and when it next moves (`docs/tui.md`,
    /// "The working line").
    motion: crate::motion::Motion,
}

impl App {
    /// An empty app in `workspace`, sized 80x24 until the loop sets it.
    /// `None` means a remote home with no local workspace; an empty path
    /// maps to none at the boundary, so no empty-path sentinel remains.
    pub(crate) fn new(workspace: impl Into<Option<PathBuf>>) -> Self {
        // An empty launch directory is no workspace: the default launch
        // carries an empty `Local` path, and a remote home carries none.
        let workspace = workspace.into().filter(|path| !path.as_os_str().is_empty());
        let mut app = Self {
            passes: Vec::new(),
            overlay: overlay::Overlay::None,
            abandoned_history: BTreeSet::new(),
            workspace,
            home: None,
            draft: Draft::default(),
            paste: paste::Paste::default(),
            phase: Phase::Starting,
            outbox: outbox::Outbox::default(),
            images: images::Images::default(),
            import_report: None,
            import_sources: HashMap::new(),
            notices: Notices::default(),
            screen: Screen::new(),
            handle: Handle::default(),
            renderer_reported: BTreeSet::new(),
            armed_at: None,
            kitty: false,
            queue: Queue::default(),
            offer: crate::offer::Offer::default(),
            steering: Steering::default(),
            name: None,
            completion: completion::Completion::default(),
            history: history::History::default(),
            copied: false,
            pending_turn: None,
            focus: None,
            stops: Vec::new(),
            regions: crate::focus::Regions::default(),
            chrome: chrome::Chrome::default(),
            drag: drag::DragState::default(),
            status_rows: status_rows::StatusRowsState::default(),
            panel_state: panel::PanelState::default(),
            delegate_rows: HashMap::new(),
            rail_state: rail::RailState::default(),
            select: select::Selection::default(),
            find: find::Find::default(),
            keyboard: keyboard::Keyboard::default(),
            model_picker: crate::model_picker::ModelPicker::default(),
            opener: false,
            attention: attention::State::default(),
            config_views: config_views::ConfigViews::default(),
            session_views: session_views::SessionViews::default(),
            items: items::Items::default(),
            reconnect: reconnect::Reconnect::default(),
            launch_overrides: Vec::new(),
            motion: crate::motion::Motion::default(),
        };
        // One install reaches every page the screen draws.
        app.screen.pages_mut().ext.handle = app.handle;
        app
    }

    /// Attaches straight to `session`. The `draw` jig uses it: an events
    /// file is one session's stream.
    pub(crate) fn attach(&mut self, session: SessionId) {
        self.end_overlay();
        self.switch_session(&session);
        // Another session's stale rows end with the attachment; the open
        // session's own notice stands until `stream_reopened` ends it.
        self.notices.clear_code_except("stream_closed", &session.0);
        // A fresh attachment supersedes an exit that arrived after the
        // last one: opening resumes the session, and its live update has
        // yet to land.
        if let Some(home) = self.home.as_mut()
            && home.left_after_attach.as_ref() == Some(&session)
        {
            home.left_after_attach = None;
        }
        self.phase = Phase::Attached {
            session,
            busy: false,
        };
        self.panel_state.attached();
        // A new session's images start over: a late answer or viewer
        // completion from the last one is dropped.
        self.forget_images();
    }

    /// Hands one key to what is on top: the key map above the model
    /// picker, the picker, the approval panel, a completion panel,
    /// then the input box.
    fn route_key(&mut self, key: Key, now: Instant) -> Effect {
        self.copied = false;
        if matches!(
            key,
            Key::PageUp
                | Key::PageDown
                | Key::End
                | Key::Up
                | Key::Down
                | Key::Tab
                | Key::BackTab
                | Key::Esc
                | Key::CtrlL
                | Key::CtrlR
        ) {
            self.screen.pages_mut().clear_anchor();
        }
        // The import report owns its keys while open, ahead of every
        // other key path: Esc closes it, arrows and pages scroll it,
        // Ctrl+C passes through to quit, and anything else is
        // swallowed so nothing reaches the draft, list or conversation
        // behind it.
        if let Some(effect) = self.report_key(&key) {
            return effect;
        }
        // The key map opens above the picker: Esc closes whatever is
        // on top, so its keys never reach the picker underneath.
        if let Some(effect) = self.keymap_key(&key) {
            return effect;
        }
        if let Some(effect) = self.model_picker_key(&key) {
            return effect;
        }
        // Ctrl+L opens the picker ahead of home, except while the quit
        // question is up, which keeps every key.
        if key == Key::CtrlL && !self.quit_open() {
            return self.open_model_picker(crate::model_picker::Mode::Choose);
        }
        if let Some(effect) = self.config_view_key(&key) {
            return effect;
        }
        if let Some(effect) = self.session_view_key(&key) {
            return effect;
        }
        if let Some(effect) = self.home_key(&key) {
            return effect;
        }
        if key == Key::CtrlC {
            return self.on_ctrl_c(now);
        }
        self.armed_at = None;
        if let Some(effect) = self.rail_key(&key) {
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
        // A running `tty` job's view types into the job ahead of the
        // input box, behind every overlay above.
        if let Some(effect) = self.item_job_key(&key) {
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
            Key::Esc if self.item_open() => self.item_esc(),
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
            Key::AltA => self.next_request(),
            Key::AltP => self.toggle_panel(),
            Key::AltR | Key::AltDigit(_) => Effect::None,
            Key::CtrlR => self.open_search(),
            Key::CtrlV => self.paste.press(self.draft.serial()),
            Key::CtrlF => Effect::None,
            // Ctrl+L opens the picker ahead of `home_key`, so it never
            // reaches here.
            Key::CtrlL => Effect::None,
            Key::CtrlG => self.open_in_editor(),
            Key::AltUp | Key::AltDown | Key::AltX => self.steering_key(&key),
        }
    }

    /// Folds one line from the hub, returning command lines to send. An
    /// answer releases the line kept for resending as it folds.
    pub(crate) fn on_line(&mut self, line: Line) -> Vec<String> {
        if self.pass_line(&line) {
            return Vec::new();
        }
        let mut lines = match self.home_line(&line) {
            Some(consumed) => consumed,
            None => match line {
                Line::Hub(hub) => self.on_hub(&hub),
                Line::Session(envelope) => self.on_session(&envelope),
            },
        };
        lines.extend(self.home_outgoing());
        lines.extend(self.sessions_outgoing());
        self.reconcile_attention();
        if matches!(&self.overlay, Overlay::Folding { landing: true, .. }) {
            if let Overlay::Folding { landing, .. } = &mut self.overlay {
                *landing = false;
            }
            self.land_pass();
        }
        self.settle();
        lines.extend(self.find_outgoing());
        lines
    }

    /// Whether any full stream is still folding its opening pass.
    pub(crate) fn pass_open(&self) -> bool {
        !self.passes.is_empty()
    }

    /// Whether the session's pass still waits for its boundary.
    fn until_waited(&self, session: &SessionId) -> bool {
        self.passes
            .iter()
            .any(|(held, pass)| held == session && matches!(pass, Pass::Until(_)))
    }

    /// The args of the open's next tail read: none when the walk
    /// reached the chain root or ended, when no read runs, or when the
    /// display filled while the held run still reads its own segment.
    /// Past a segment start the read keeps going for the joint total:
    /// a `seq 0` with an unfollowed parent still asks. The pass counts
    /// the lines above a filled display, so the open holds only the
    /// filled run plus each seam crossing.
    pub(crate) fn tail_next(&mut self) -> Option<Value> {
        let Overlay::Reading(tail) = &self.overlay else {
            return None;
        };
        if tail.chain_done() {
            return None;
        }
        let mut fresh = self.attached_screen().pages().fresh();
        if let Some(lines) = tail.turn_lines() {
            for line in lines {
                fresh.apply(line);
            }
        }
        if tail.filled(&fresh, self.conversation_height()) && !tail.seam_pending() {
            return None;
        }
        Some(tail.next_args())
    }

    /// Folds one tail answer into the open's read: an answer that
    /// leaves a gap, or a line carrying no seq, ends the tail as failed
    /// with the out-of-order notice.
    pub(crate) fn tail_lines(&mut self, lines: Vec<Envelope>) {
        let Overlay::Reading(tail) = &mut self.overlay else {
            return;
        };
        if tail.prepend(lines).is_err() {
            tail.end();
            self.notices.push(
                "io_failed",
                "Could not load history: the answer was out of order".to_owned(),
            );
        }
    }

    /// Ends the open's tail read: failed with the notice, abandoned or
    /// lost without one.
    pub(crate) fn tail_ended(&mut self, notice: Option<&str>) {
        if let Overlay::Reading(tail) = &mut self.overlay {
            tail.end();
        }
        if let Some(notice) = notice {
            self.notices
                .push("io_failed", format!("Could not load history: {notice}"));
        }
    }

    /// Installs the open's read tail as the preview: its lines from the
    /// first turn start, never cut inside a turn, or an empty
    /// conversation when no held line starts one. An empty log installs
    /// nothing and shows no history. The screen follows, the overlay
    /// shows the preview, and the screen freezes for the raise.
    pub(crate) fn install_tail(&mut self) {
        let Overlay::Reading(tail) = std::mem::replace(&mut self.overlay, Overlay::None) else {
            return;
        };
        let Some(session) = self.opening_session() else {
            return;
        };
        let mut preview = self.attached_screen().pages().fresh();
        let mut busy = None;
        if let Some(lines) = tail.turn_lines() {
            for line in lines {
                let applied = preview.apply(line);
                if applied.busy.is_some() {
                    busy = applied.busy;
                }
            }
        }
        *self.attached_screen_mut().pages_mut() = preview;
        self.attached_screen_mut().follow();
        self.overlay = Overlay::Shown {
            session,
            busy,
            held: false,
        };
        self.attached_screen_mut().set_frozen(true);
    }

    /// Starts the pass on the raise's ack: the preview stays on the
    /// screen and the session's lines fold into a fresh second `Pages`,
    /// held while a batch is open.
    fn begin_fold(&mut self, session: &SessionId, ack: &str) {
        let Overlay::Shown {
            session: shown,
            busy,
            held,
        } = &self.overlay
        else {
            return;
        };
        if shown != session || !self.raise_ack(session, ack) {
            return;
        }
        let busy = *busy;
        let held = *held;
        let fold = self.fresh_fold();
        self.overlay = Overlay::Folding {
            session: session.clone(),
            fold,
            busy,
            held,
            landing: false,
        };
    }

    /// A fresh second `Pages` for the pass, held while a batch is open.
    fn fresh_fold(&self) -> Box<Pages> {
        let mut fold = self.attached_screen().pages().fresh();
        if self.screen.pages().holding() {
            fold.hold();
        }
        Box::new(fold)
    }

    /// The preview's last folded `seq`, if any line was held.
    fn preview_last(&self) -> Option<Seq> {
        self.attached_screen()
            .pages()
            .index()
            .pages()
            .iter()
            .filter(|page| page.lines > 0)
            .map(|page| page.last_seq)
            .max()
    }

    /// The pages the session's line folds into: the overlay's second
    /// `Pages` while its pass runs, else the attached screen's. The
    /// guard and the reborrow are one shape test split for the borrow
    /// checker: no `folding` check, no `unreachable`.
    fn fold_pages(&mut self, session: &SessionId) -> &mut Pages {
        if let Overlay::Folding {
            session: folding,
            fold,
            ..
        } = &mut self.overlay
            && folding == session
        {
            return fold;
        }
        if let Some(open) = self.items.open.as_mut()
            && let Some(stashed) = open.stashed.as_mut()
        {
            return stashed.pages_mut();
        }
        self.screen.pages_mut()
    }

    /// Marks the folding pass to land at its boundary: the next line
    /// ending the pass swaps the fold in at the held anchor.
    fn mark_landing(&mut self) {
        if let Overlay::Folding { landing, .. } = &mut self.overlay {
            *landing = true;
        }
    }

    /// Lands the pass whose boundary the last line reached: when the
    /// overlay still folds for the attached session its fold swaps in,
    /// else the overlay ends and drops it.
    fn land_pass(&mut self) {
        let taken = std::mem::replace(&mut self.overlay, Overlay::None);
        let Overlay::Folding {
            session,
            fold,
            busy,
            held,
            ..
        } = taken
        else {
            self.end_overlay();
            return;
        };
        if self.session() != Some(&session) {
            self.end_overlay();
            return;
        }
        let fresh = self.fresh_fold();
        self.overlay = Overlay::Folding {
            session,
            fold: fresh,
            busy,
            held,
            landing: false,
        };
        self.land(*fold, held);
    }

    /// Swaps the pass's second `Pages` in at its boundary: the preview's
    /// opens carry, focus remaps to the same call or clears, the
    /// selection clears, the kept busy applies, and the overlay ends. A
    /// held landing keeps the preview's place: the top row's segment
    /// session with its own `seq` and its offset among that line's rows
    /// are read before the swap, and after the unfreeze applies any
    /// deferred width the top is the fold's row with that session and
    /// `seq`, at that offset kept within its rows, or the end when the
    /// fold lacks it. A dropped page keeps the old width's rows until
    /// it loads, so the pages hold the anchor and each load finds the
    /// top again.
    fn land(&mut self, mut fold: Pages, held: bool) {
        // The preview's closed ancestor runs land in the fold front
        // beside the carried opens, under their own sessions, before the
        // swap; the fold still replays the own log only.
        self.attached_screen().pages().carry_chain(&mut fold);
        self.attached_screen().pages().carry_opens(&mut fold);
        let focus = self.focus.take();
        self.focus = focus.and_then(|id| self.remap_focus(id, &fold));
        self.clear_selection();
        let anchor = held.then(|| self.top_anchor()).flatten();
        let busy = match &self.overlay {
            Overlay::Folding { busy, .. } => *busy,
            Overlay::None | Overlay::Shown { .. } | Overlay::Kept { .. } | Overlay::Reading(_) => {
                None
            }
        };
        std::mem::swap(self.attached_screen_mut().pages_mut(), &mut fold);
        self.end_overlay();
        if let Some((session, seq, offset)) = anchor {
            let pages = self.attached_screen_mut().pages_mut();
            pages.hold_anchor(&session, seq, offset);
            match pages.anchored_row() {
                Some(row) => {
                    self.screen.jump_kept(row);
                    self.settle();
                }
                None => self.attached_screen_mut().follow(),
            }
        }
        if let Some(busy) = busy {
            self.set_busy(busy);
        }
    }

    /// The preview's top row segment session with its `seq` and the
    /// row's offset among that line's rows: the anchor a held landing
    /// keeps. None while following or when the page holds no line.
    fn top_anchor(&self) -> Option<(SessionId, Seq, usize)> {
        let top = self.attached_screen().top()?;
        self.attached_screen().pages().row_seq(top)
    }

    /// Remaps focus across the swap: a line target follows its call by
    /// stable identity, a row link clears, and the rest stay.
    fn remap_focus(
        &self,
        id: crate::mouse::TargetId,
        fold: &Pages,
    ) -> Option<crate::mouse::TargetId> {
        use crate::mouse::TargetId;
        if let TargetId::Line(target) = id {
            return self
                .attached_screen()
                .pages()
                .same_target(target, fold)
                .map(TargetId::Line);
        }
        if matches!(id, TargetId::Link { .. }) {
            return None;
        }
        Some(id)
    }

    /// Whether the scroll bar draws its thumb: the counts are exact
    /// while the screen is not frozen, except on the attached screen
    /// while the overlay keeps the preview.
    pub(crate) fn counts_exact(&self) -> bool {
        if self.screen.frozen() {
            return false;
        }
        if matches!(self.overlay, Overlay::Kept { .. })
            && self
                .items
                .open
                .as_ref()
                .is_none_or(|open| open.stashed.is_none())
        {
            return false;
        }
        true
    }

    /// Holds page requests while an earlier stash waits to fold a reopening.
    pub(crate) fn reopen_pending(&mut self, session: SessionId) {
        self.passes.retain(|(held, _)| held != &session);
        self.passes.push((session, Pass::Pending));
    }

    /// Discards the late answer of a history wait ended by a reopening.
    pub(crate) fn abandon_history(&mut self, id: String) {
        self.abandoned_history.insert(id);
    }

    /// Updates pass boundaries before any view consumes the line. Returns
    /// true only for an abandoned answer, which must fold nowhere.
    fn pass_line(&mut self, line: &Line) -> bool {
        match line {
            Line::Session(line) => {
                if matches!(line.kind.as_str(), "command_accepted" | "command_rejected")
                    && let Some(id) = line.payload.get("command_id").and_then(Value::as_str)
                    && self.abandoned_history.remove(id)
                {
                    return true;
                }
                let waited = line
                    .seq
                    .is_some_and(|_| self.until_waited(&line.session_id));
                if let Some(seq) = line.seq {
                    self.passes.retain(|(session, pass)| match pass {
                        Pass::Pending => true,
                        Pass::Until(last) => session != &line.session_id || seq < *last,
                    });
                }
                if waited && !self.until_waited(&line.session_id) && self.folding(&line.session_id)
                {
                    self.mark_landing();
                }
                if line.kind == "command_accepted"
                    && let Some(accepted) = read!(line, CommandAccepted)
                {
                    if let Some(CommandResult::Subscribe { last_seq }) = accepted.result {
                        self.passes
                            .retain(|(session, _)| session != &line.session_id);
                        self.passes
                            .push((line.session_id.clone(), Pass::Until(last_seq)));
                        if let Some(id) = line.payload.get("command_id").and_then(Value::as_str) {
                            self.begin_fold(&line.session_id, id);
                        }
                    } else if let Some(id) = line.payload.get("command_id").and_then(Value::as_str)
                        && self.raise_ack(&line.session_id, id)
                    {
                        self.end_overlay();
                    }
                }
            }
            Line::Hub(line) => {
                let Some(session) = line.payload.get("session_id").and_then(Value::as_str) else {
                    return false;
                };
                let session = SessionId(session.to_owned());
                match line.kind.as_str() {
                    "stream_reopened" => {
                        // The cause ended for this session: its notice goes.
                        self.notices.clear_code("stream_closed", &session.0);
                        self.passes.retain(|(held, _)| held != &session);
                        if let Some(last) = line.payload.get("last_seq").and_then(Value::as_u64) {
                            self.passes.push((session.clone(), Pass::Until(Seq(last))));
                        }
                        match std::mem::replace(&mut self.overlay, Overlay::None) {
                            Overlay::Folding {
                                session: folding,
                                busy,
                                held,
                                ..
                            } if folding == session => {
                                let fold = self.fresh_fold();
                                self.overlay = Overlay::Folding {
                                    session,
                                    fold,
                                    busy,
                                    held,
                                    landing: false,
                                };
                            }
                            rest @ (Overlay::None
                            | Overlay::Shown { .. }
                            | Overlay::Folding { .. }
                            | Overlay::Kept { .. }
                            | Overlay::Reading(_)) => {
                                self.overlay = rest;
                            }
                        }
                    }
                    "stream_closed" => {
                        self.passes.retain(|(held, pass)| {
                            held != &session || matches!(pass, Pass::Pending)
                        });
                        if matches!(&self.overlay, Overlay::Shown { session: shown, .. } if shown == &session)
                        {
                            self.end_overlay();
                        } else {
                            self.keep_pass(&session);
                        }
                    }
                    "session_left" => {
                        self.passes.retain(|(held, pass)| {
                            held != &session || matches!(pass, Pass::Pending)
                        });
                        if matches!(&self.overlay, Overlay::Shown { session: shown, .. } if shown == &session)
                        {
                            self.end_overlay();
                        } else {
                            self.keep_pass(&session);
                        }
                    }
                    _ => {}
                }
            }
        }
        false
    }

    /// Keeps the preview when the pass's stream ends without its
    /// boundary, through `stream_closed` or `session_left` alike: the
    /// preview stays, unfrozen, and replays skip to its last line. With
    /// no preview the pass's lines are the only copy, so they land as
    /// the screen instead of dropping with the fold.
    fn keep_pass(&mut self, session: &SessionId) {
        let last = self.preview_last();
        match std::mem::replace(&mut self.overlay, Overlay::None) {
            Overlay::Folding {
                session: folding,
                fold,
                busy,
                held,
                ..
            } if &folding == session => {
                if last.is_none() {
                    // The tail read failed and installed no preview, so
                    // the pass's lines are the only copy: land them as
                    // the screen instead of dropping them with the fold.
                    let fresh = self.fresh_fold();
                    self.overlay = Overlay::Folding {
                        session: session.clone(),
                        fold: fresh,
                        busy,
                        held,
                        landing: false,
                    };
                    self.land(*fold, held);
                }
                self.overlay = Overlay::Kept { last };
                self.attached_screen_mut().set_frozen(false);
            }
            rest @ (Overlay::None
            | Overlay::Shown { .. }
            | Overlay::Folding { .. }
            | Overlay::Kept { .. }
            | Overlay::Reading(_)) => {
                self.overlay = rest;
            }
        }
    }

    /// Shows `text` as a notice under `code`.
    pub(crate) fn push_notice(&mut self, code: &str, text: String) {
        self.notices.push(code, text);
    }

    /// Births unborn notices and removes expired events; the frame calls
    /// it before each draw and again after, and `run_with` calls it
    /// before the first arm.
    pub(crate) fn sweep_notices(&mut self, now: Instant) {
        self.notices.sweep(now);
    }

    /// The hub could not be reached, or runs a schema this terminal cannot
    /// read: the notice for the first failure in a run, and a held `start`
    /// fails as if rejected.
    pub(crate) fn connect_failed(&mut self, notice: String) {
        self.outbox.link = Link::Down;
        if self.notice_due() {
            self.notices.push("connection_failed", notice);
        }
        self.outbox.held.clear();
        if let Phase::Pending { command_id } = &self.phase {
            let id = command_id.clone();
            self.fail(&id);
        }
        self.settle();
    }

    /// The hub connection ended; the loop retries it. A connection never
    /// connected, refused for its schema version, keeps the notice that
    /// says why.
    pub(crate) fn disconnected(&mut self) {
        self.passes.clear();
        self.abandoned_history.clear();
        self.end_overlay();
        if self.outbox.is_up() {
            self.outbox.link = Link::Down;
            self.notices
                .push("connection_failed", "Connection lost.".to_owned());
        }
        // A lost connection closes an open item view first, swapping the
        // screen back before anything clears it, and drops every wish:
        // the hub connection and its levels are gone.
        self.drop_items();
        self.sessions_dropped();
        self.images_disconnected();
        self.picker_disconnected();
        self.find_lost();
        self.abandon_copy();
        self.settle();
    }

    /// Writing `unsent`, command lines this app made, to the hub failed:
    /// the connection is lost, and the commands never written fail as if
    /// rejected, so a draft they carried returns to an empty draft. A line
    /// already written and kept for resending keeps its pending entry and
    /// leaves the draft alone, so the next connection sends it again
    /// (`docs/invocation.md`, "The command line"). A close from the
    /// quit question that was never written keeps its resume line.
    pub(crate) fn write_failed(&mut self, unsent: &[String]) {
        self.home_unsent(unsent);
        self.disconnected();
        for line in unsent {
            if let Some(id) = serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|line| line.get("id").and_then(Value::as_str).map(str::to_owned))
            {
                if self.outbox.is_kept(&id) {
                    continue;
                }
                self.fail(&id);
            }
        }
        self.settle();
    }

    /// Whether the hub spoke a `hub_hello` this terminal reads, and the
    /// connection has not ended since.
    pub(crate) fn connected(&self) -> bool {
        self.outbox.is_up()
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
        // A resize while open applies to both screens, so the restored
        // screen keeps the new size.
        if let Some(open) = self.items.open.as_mut()
            && let Some(stashed) = open.stashed.as_mut()
        {
            stashed.set_size(width, height);
        }
        self.settle();
    }

    /// The terminal's size in columns and rows: what full-screen
    /// overlays clamp against.
    pub(crate) fn screen_size(&self) -> (u16, u16) {
        (self.screen.width(), self.screen.height())
    }

    /// Sets the zone the time of day under a prompt bubble shows
    /// (`docs/tui.md`, "Turns").
    pub(crate) fn set_zone(&mut self, zone: TimeZone) {
        self.screen.pages_mut().zone = zone.clone();
        if let Some(open) = self.items.open.as_mut()
            && let Some(stashed) = open.stashed.as_mut()
        {
            stashed.pages_mut().zone = zone;
        }
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

    /// The width the draft wraps at: past the input box's stripe and
    /// gap, except on home, where the home box draws at its own width
    /// (`docs/tui.md`, "Look", "The input box"). Without colour the
    /// home box is thin too, so its branch wraps at the same boxed
    /// interior width the home renderer uses.
    pub(crate) fn draft_width(&self) -> u16 {
        if self.on_home() {
            let width = self.column_width();
            if crate::surface::box_fit(width) {
                width.saturating_sub(2)
            } else {
                width
            }
        } else {
            crate::surface::inset(self.column_width())
        }
    }

    /// The input box's rows: the draft's wrapped rows, at most a third of
    /// the screen and at least one.
    pub(crate) fn input_height(&self) -> usize {
        let cap = usize::from(self.screen.height() / 3).max(1);
        self.draft.rows(self.draft_width()).len().min(cap)
    }

    /// Whether the quit hint or the rail hint shows: armed by a first
    /// Ctrl+C, or while the rail holds focus. The quit
    /// question draws as an overlay, not through the hint.
    pub(crate) fn hint(&self) -> bool {
        self.armed_at.is_some() || self.rail_state.focused().is_some()
    }

    /// Whether new output arrived while scrolled up.
    pub(crate) fn has_new(&self) -> bool {
        self.screen.has_new()
    }

    /// The top wrapped row while scrolled up; `None` follows.
    pub(crate) fn top(&self) -> Option<usize> {
        self.screen.top()
    }

    /// The conversation's rows: the screen less the input box or the panel
    /// in its place, the steering queue, the banner, the badge and the
    /// hint, and the narrow layout's rows under the conversation. None on
    /// a screen too short for them.
    pub(crate) fn conversation_height(&self) -> usize {
        let below = self.below_rows();
        let height =
            usize::from(self.screen.height()).saturating_sub(below + self.narrow_rows(below));
        // The item view's header takes the conversation's top rows.
        if self.item_open() {
            height.saturating_sub(crate::view::item::ITEM_HEADER_ROWS)
        } else {
            height
        }
    }

    /// The rows below the conversation before the narrow layout's rows:
    /// the input box or the panel in its place, the steering queue, the
    /// banner, the badge and the hint.
    pub(crate) fn below_rows(&self) -> usize {
        // The same room the view draws the box and the panel in: the body
        // height, so the rows counted are the rows drawn
        // (`docs/tui.md`, "Layout").
        let room = usize::from(self.screen.height());
        let input = self.panel().map_or_else(
            || crate::surface::edged(self.input_height(), room),
            |panel| crate::view::request::height(&panel, self.column_width(), room),
        );
        let under = input + self.steering_below() + usize::from(self.working_row_shown());
        // The notice rows draw above the working line, so their room is
        // what is left above it; the badge and hint draw above them.
        let notices = self.notices.drawn().min(room.saturating_sub(under));
        under + notices + usize::from(self.badge().is_some()) + usize::from(self.hint())
    }

    /// The approval panel, while it is open: its form laid out at the
    /// inset width, past the panel's stripe and gap (`docs/tui.md`,
    /// "Look"). The session asking shows its name when known, else its
    /// id (`docs/tui.md`, "An approval").
    pub(crate) fn panel(&self) -> Option<Panel> {
        let width = crate::surface::inset(self.column_width());
        self.queue.panel_with(width, &|id| {
            let name = self
                .home
                .as_ref()
                .and_then(|home| home.sessions.row(id))
                .map(|row| row.name.clone());
            if name.as_deref().is_some_and(|name| !name.is_empty()) {
                return name;
            }
            if self.session().is_some_and(|session| session == id) {
                self.name().map(str::to_owned)
            } else {
                None
            }
        })
    }

    /// The badge row while the panel is closed and requests wait
    /// (`docs/tui.md`, "An approval").
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

    /// The pinned header, if any: none while an item view covers the
    /// conversation or while following.
    pub(crate) fn pinned_row(&self) -> Option<crate::window::Pin> {
        if self.item_open() {
            return None;
        }
        self.screen.pin(self.conversation_height())
    }

    /// The seq ranges of pages the next frame needs and does not hold,
    /// each with its segment's session.
    pub(crate) fn needs(&self) -> Vec<(SessionId, RangeInclusive<Seq>)> {
        if self.pass_open() {
            return Vec::new();
        }
        self.screen.needs(self.conversation_height())
    }

    /// Folds a fetched range's durable lines into their pages, the
    /// window's segment: every line in a page shares its session.
    pub(crate) fn load(&mut self, session: &SessionId, lines: Vec<Envelope>) {
        self.screen.pages_mut().load(session, &lines);
        // A held landing's top, found again at the width the load counted.
        // The pass stays live while a stale page is still owed, after it
        // closed: the anchor lets go only once every stale page is counted
        // or failed.
        let live = self.pass_open() || self.screen.pages().stale_owed();
        if live && let Some(row) = self.screen.pages_mut().anchored_row() {
            self.screen.jump_kept(row);
        }
        self.settle();
    }

    /// Loading `range` of segment `session` failed: its rows stay blank
    /// and the notice says why.
    /// A failed page ends a whole-turn copy waiting on dropped pages.
    pub(crate) fn load_failed(
        &mut self,
        session: &SessionId,
        range: &RangeInclusive<Seq>,
        message: &str,
    ) {
        self.screen.pages_mut().fail(session, *range.start());
        if !self.pass_open() {
            self.screen.pages_mut().clear_anchor();
        }
        self.cancel_pending_turn();
        self.notices
            .push("io_failed", format!("Could not load history: {message}"));
    }

    /// Scrolls so `row` is the top row, as dragging the scroll bar does.
    pub(crate) fn jump(&mut self, row: usize) {
        self.screen.pages_mut().clear_anchor();
        self.screen.jump(row);
        self.settle();
    }

    /// The pages.
    pub(crate) fn pages(&self) -> &Pages {
        self.screen.pages()
    }

    /// Installs the ledger renderer for tests without recounting: the
    /// first counted draw calls once per distinct input, which is what
    /// the paging tests assert. Production installs through
    /// `use_renderer`, which counts every page again.
    #[cfg(test)]
    pub(crate) fn install_renderer(&mut self, renderer: Rc<dyn Renderer>) {
        self.set_renderer(renderer);
    }

    /// Installs `renderer` through the shared handle, creating its slot
    /// the first time: a renderer installed on another thread resolves
    /// to nothing there, so moving the app across threads installs
    /// afresh here.
    fn set_renderer(&mut self, renderer: Rc<dyn Renderer>) {
        if self.handle.get().is_none() {
            self.handle = Handle::create();
            self.attached_screen_mut().pages_mut().ext.handle = self.handle;
        }
        self.handle.set(renderer);
    }

    /// Installs the ledger renderer: every page counts again under it.
    /// No production caller yet: Part 3 adapts the extensions VM to it.
    #[allow(
        dead_code,
        reason = "no production caller until Part 3 adapts the extensions VM"
    )]
    pub(crate) fn use_renderer(&mut self, renderer: Rc<dyn Renderer>) {
        self.set_renderer(renderer);
        self.attached_screen_mut().pages_mut().renderers_changed();
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
                let mut lines = std::mem::take(&mut self.outbox.held);
                lines.extend(self.reconnected());
                self.outbox.link = Link::Up;
                lines
            }
            "hub_hello" => {
                // The hub was reached, so the refusal always says why.
                self.hub_reached();
                self.connect_failed(format!(
                    "The hub runs schema version {}; this terminal reads {}.",
                    hub.schema_version,
                    contract::SCHEMA_VERSION
                ));
                self.outbox.link = Link::Refused;
                Vec::new()
            }
            "command_accepted" => {
                let Some(id) = command_id else {
                    return Vec::new();
                };
                if let Some(lines) = self.history_answered(&id, hub.payload.get("result")) {
                    return lines;
                }
                // A `read_file` for a viewer open answers here, never
                // in the pages: its bytes go to the loop's worker.
                if self.image_answered(&id, hub.payload.get("result")) {
                    return Vec::new();
                }
                let session = hub
                    .payload
                    .get("result")
                    .and_then(|result| result.get("session_id"))
                    .and_then(Value::as_str)
                    .map(|id| SessionId(id.to_owned()));
                match (self.outbox.answered(&id), session) {
                    (Some((Kind::Start, draft)), Some(session)) => {
                        self.model_picker.start_model = None;
                        self.started(session, draft)
                    }
                    _ => Vec::new(),
                }
            }
            "command_rejected" => {
                if let Some(id) = command_id {
                    let message = hub_string(&hub.payload, "message").unwrap_or_default();
                    let code = hub_code(&hub.payload);
                    if self.image_rejected(&id, &message) {
                        return Vec::new();
                    }
                    self.offer.file_refused(
                        &id,
                        &serde_json::from_value(Value::String(code.clone()))
                            .unwrap_or_else(|_| contract::ErrorCode::Other(code.clone())),
                        &message,
                    );
                    self.config_views_refused(&id, &message);
                    self.session_views_refused(&id, &message);
                    if !self.history_rejected(&id, &code, &message) {
                        self.rejected(&id, &code, message);
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
            // The hub holds no subscription for the open session any more:
            // keep what arrived, say so, and leave the retry to a reopen.
            "stream_closed" => {
                let closed = hub_string(&hub.payload, "session_id");
                if closed.as_deref() == self.session().map(|session| session.0.as_str())
                    && let Some(session) = closed
                {
                    self.notices.push_identified(
                        "stream_closed",
                        session,
                        STREAM_CLOSED.to_owned(),
                    );
                }
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
    fn rejected(&mut self, id: &str, code: &str, message: String) {
        // A refused `model` command drops the writes its acceptance
        // would have made.
        self.model_picker_rejected(id);
        match self.outbox.pending.get(id) {
            None => {}
            // Another client may have dropped or amended the row first.
            Some((Kind::Cancel | Kind::SteerDrop, _)) => {
                self.outbox.pending.remove(id);
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
                self.notices.push(code, message);
                self.fail(id);
            }
        }
        // A refusal settles the command: its kept line goes out no more.
        self.outbox.answered(id);
    }

    /// An accepted `import`: one report overlay naming what the import
    /// copied and left behind, and no notices, since imported items
    /// apply on the next reload. A late or duplicate answer finds no
    /// source entry and changes nothing. A rejection drops the entry
    /// through `refused` unchanged.
    fn import_answered(&mut self, source: Option<String>, accepted: &CommandAccepted) {
        let Some(CommandResult::Import {
            imported,
            left_behind,
        }) = &accepted.result
        else {
            return;
        };
        self.open_import_report(source, imported.clone(), left_behind.clone());
    }

    /// Drops a pending command, returning its draft to an empty box.
    fn fail(&mut self, id: &str) {
        if let Some((kind, draft)) = self.outbox.fail(id) {
            self.apply_failed(id, kind, draft);
        }
    }

    /// Applies a popped command's failure: its draft back in an empty
    /// box, `Starting` after a `start`, the queue and the offer after a
    /// `reply`. The only place reaching them for a failure.
    pub(crate) fn apply_failed(&mut self, id: &str, kind: Kind, draft: Draft) {
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

    /// Folds one session line, returning command lines to send.
    fn on_session(&mut self, envelope: &Envelope) -> Vec<String> {
        // The queue takes every session's requests; everything else is the
        // attached session's alone.
        if approvals::KINDS.contains(&envelope.kind.as_str()) {
            self.queue.fold(envelope);
            self.request_arrived(envelope);
        }
        let mut send = self.reply_ack(envelope);
        // A `model` command's acceptance settles what it waited on,
        // from its own session: choosing may span a switch, but a late
        // accept after one, or another session's, names no row.
        if envelope.kind == "command_accepted"
            && let Some(accepted) = read!(envelope, CommandAccepted)
        {
            let id = accepted.command_id.0.clone();
            self.model_picker_accepted(&id, &envelope.session_id);
            self.credential_accepted(&accepted);
        }
        // The open delegate's lines fold into the swapped screen, never
        // touching the attached busy flag; their command answers still
        // settle below, as any non-attached session's do.
        if self.is_item_session(&envelope.session_id) {
            self.item_session_line(envelope);
            match envelope.kind.as_str() {
                "command_accepted" => {
                    if let Some(accepted) = read!(envelope, CommandAccepted) {
                        self.outbox.answered(&accepted.command_id.0);
                    }
                }
                "command_rejected" => {
                    if let Some(rejected) = read!(envelope, CommandRejected)
                        && let Some(id) = rejected.command_id
                    {
                        self.refused(&id.0, &rejected.code, rejected.message);
                    }
                }
                _ => {}
            }
            return send;
        }
        if self.session() != Some(&envelope.session_id) {
            // A resent command for another session settles its pending
            // entry when its answer arrives, as on screen
            // (`docs/invocation.md`, "The command line"). `reply_ack`
            // above already settled a `reply`, including its decline's
            // `cancel`, so a second settle here finds nothing to do.
            match envelope.kind.as_str() {
                "command_accepted" => {
                    if let Some(accepted) = read!(envelope, CommandAccepted) {
                        // No `commands_answered`: a `commands` answer from
                        // another session is ignored, and a resent command
                        // needs only its pending entry closed.
                        self.outbox.answered(&accepted.command_id.0);
                    }
                }
                "command_rejected" => {
                    if let Some(rejected) = read!(envelope, CommandRejected)
                        && let Some(id) = rejected.command_id
                    {
                        self.refused(&id.0, &rejected.code, rejected.message);
                    }
                }
                _ => {}
            }
            return send;
        }
        // The search's fetch answers here, never in the pages: its lines
        // fold into the scan and go no further.
        if let Some(lines) = self.find_answered(envelope) {
            send.extend(lines);
            return send;
        }
        send.extend(self.panel_line(envelope));
        self.items_line(envelope);
        self.output_line(envelope);
        self.session_views_line(envelope);
        send.extend(self.config_views_line(envelope));
        if envelope.kind == "turn_started"
            && let Some(started) = read!(envelope, TurnStarted)
        {
            self.history.saw(&envelope.session_id, &started.input);
        }
        let folding = self.folding(&envelope.session_id);
        let kept = self.kept_skips(&envelope.session_id, envelope.seq);
        let applied = if kept {
            Applied {
                changed: false,
                busy: None,
            }
        } else {
            self.fold_pages(&envelope.session_id).apply(envelope)
        };
        let mut changed = applied.changed;
        match envelope.kind.as_str() {
            "command_accepted" => {
                if let Some(accepted) = read!(envelope, CommandAccepted) {
                    let sent = self.outbox.answered(&accepted.command_id.0);
                    self.commands_answered(&accepted);
                    // The accepted import's source entry is consumed by
                    // its answer: a late or duplicate answer finds
                    // nothing and changes nothing.
                    if let Some(source) = self.import_sources.remove(&accepted.command_id.0) {
                        self.import_answered(source, &accepted);
                    }
                    changed |= self.offer.file_answered(&accepted);
                    let shell = sent.filter(|(kind, _)| *kind == Kind::Shell);
                    let item = shell
                        .and_then(|(_, draft)| shell::answered(&draft.expand(), accepted.result));
                    changed |= !kept && self.fold_pages(&envelope.session_id).add_shell(item);
                }
            }
            "shell_command" => {
                if let Some(ran) = read!(envelope, ShellCommand) {
                    changed |= !kept
                        && self
                            .fold_pages(&envelope.session_id)
                            .add_shell(Some(shell::ran(&ran)));
                }
            }
            "command_rejected" => {
                if let Some(rejected) = read!(envelope, CommandRejected)
                    && let Some(id) = rejected.command_id
                {
                    self.offer
                        .file_refused(&id.0, &rejected.code, &rejected.message);
                    // A refused import drops its source entry with its
                    // answer, as an accepted one consumes it.
                    self.import_sources.remove(&id.0);
                    self.refused(&id.0, &rejected.code, rejected.message);
                }
            }
            "reloaded" => {
                if self.outbox.is_up() {
                    send.push(self.ask_commands(&envelope.session_id));
                }
            }
            "usage_recorded" => self.config_views_usage(envelope),
            "session_named" => {
                if let Some(named) = read!(envelope, SessionNamed) {
                    self.name = named.name;
                }
            }
            "notice" => {
                if let Some(notice) = read!(envelope, Notice) {
                    let code = crate::format::code(&notice.code);
                    self.notices.push(&code, notice.message);
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
            if folding {
                if let Overlay::Folding { busy: held, .. } = &mut self.overlay {
                    *held = Some(busy);
                }
            } else {
                self.set_busy(busy);
            }
        }
        if changed && !folding {
            self.attached_screen_mut().changed();
        }
        send
    }

    fn set_busy(&mut self, busy: bool) {
        if let Phase::Attached { busy: flag, .. } = &mut self.phase {
            let ending = *flag && !busy;
            *flag = busy;
            if ending {
                // The turn applied every pending choice: the applied
                // shares go with it.
                self.notices.clear_code_all("switch_pending");
            }
        }
    }

    /// PageUp and PageDown move by the conversation height less one.
    fn page(&mut self, up: bool) {
        let height = self.conversation_height();
        self.screen.page(up, height);
        self.settle();
    }
}

/// The text parts of a message, joined.
pub(crate) fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
        })
        .collect()
}

/// The text of the notice for a `stream_closed` on the open session
/// (`docs/tui.md`, "Notices").
const STREAM_CLOSED: &str =
    "this session's log can't be read past here; reopen the session to try again";

fn hub_string(payload: &Map<String, Value>, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// A hub `command_rejected`'s code: `malformed` when its answer carries
/// none the terminal can read as a command answer.
fn hub_code(payload: &Map<String, Value>) -> String {
    hub_string(payload, "code").unwrap_or_else(|| "malformed".to_owned())
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
