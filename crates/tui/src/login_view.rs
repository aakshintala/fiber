//! `/login` (`docs/tui.md`, "Logging in"): the installed providers and
//! the secrets installed extensions declare, a hidden field for a key or a
//! value, and the same store steps as `fiber login`. The key is never
//! drawn: the field shows one dot per character, and no frame, `Debug`,
//! notice or log holds it.

use std::fmt;

use contract::{ErrorCode, Secret};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::App;
use crate::configure::{HostGap, LoginKind, LoginTarget};
use crate::format;
use crate::input::Draft;
use crate::keys::{Edit, Key};
use crate::login_worker::{LoginStep, LoginTicket, LoginWorker};
use crate::mouse::{Target, TargetId};
use crate::rows::Join;
use crate::settings_view::{Act, Ctx};
use crate::swapped::Spot;
use crate::theme::Role;
use crate::view::overlay;

/// The key or value being typed. It never prints: `Debug` shows only its
/// length (`docs/code-quality.md`, "Errors"). No `Clone`, no `PartialEq`.
struct SecretField(String);

impl fmt::Debug for SecretField {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretField({} chars)", self.0.chars().count())
    }
}

impl SecretField {
    /// Types `ch` at the end; the caret stays at the end.
    fn push(&mut self, ch: char) {
        self.0.push(ch);
    }

    /// Appends pasted text as is: no paste token, because the view takes
    /// the edit before `input::route` builds tokens.
    fn push_str(&mut self, text: &str) {
        self.0.push_str(text);
    }

    /// Deletes the last character.
    fn pop(&mut self) {
        self.0.pop();
    }

    /// Drops the key, storing nothing.
    fn clear(&mut self) {
        self.0.clear();
    }

    /// One dot per character.
    fn dots(&self) -> String {
        self.0.chars().map(|_| '•').collect()
    }

    /// The key trimmed, moved out so the field keeps no copy.
    fn take(&mut self) -> Secret {
        Secret::new(std::mem::take(&mut self.0).trim().to_owned())
    }
}

/// A heading's group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    /// The installed providers.
    Providers,
    /// The declared secrets.
    Secrets,
}

/// One row of the view: a heading, one target, the blank row between
/// the groups, or the note in place of every row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    /// A group's heading; Enter on it does nothing.
    Heading(Group),
    /// One provider or secret: its index in the targets.
    Target(usize),
    /// The blank row between the providers and the secrets.
    Gap,
    /// The note shown with no provider and no secret.
    Note,
}

/// The list overlay's chrome in rows, measured with a one-row probe so
/// the count never depends on the body: the edges, the pads, the title
/// and its gap, and the footer and its gap.
fn list_chrome() -> usize {
    let probe = overlay::Overlay {
        title: Some(("Log in".to_owned(), Some(Span::raw("✕")))),
        close: None,
        body: vec![overlay::Row {
            spans: Vec::new(),
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        }],
        footer: Some(overlay::legend(&[
            ("↑↓", "move"),
            ("Enter", "log in"),
            ("Esc", "close"),
        ])),
        prefer: 96,
    };
    usize::from(overlay::height(&probe)).saturating_sub(1)
}

/// How many body rows the list overlay shows at `height` rows tall:
/// the height less its chrome, or none when the chrome fills it.
fn shown_body_rows(height: usize) -> usize {
    height.saturating_sub(list_chrome())
}

/// Where the list overlay's window starts so `focused` shows in `shown`
/// rows of `rows`: past the rows shown with it when the focused row
/// would fall below the window, else the top. An empty body or window
/// starts at the top.
fn window_start(focused: usize, shown: usize, rows: usize) -> usize {
    if rows == 0 || shown == 0 || focused < shown {
        0
    } else {
        focused.saturating_add(1).saturating_sub(shown)
    }
}

/// Which field Enter stores from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    /// The hidden key or value.
    Key,
    /// The provider's label.
    Label,
}

/// The open panel over one target: the hidden key or value, the
/// provider's label, and which one takes typing.
#[derive(Debug)]
struct Panel {
    /// The target's index.
    target: usize,
    /// The key or value being typed.
    key: SecretField,
    /// The provider's label; a secret has none.
    label: Draft,
    /// Which field takes typing.
    focus: Focus,
}

/// What the view shows below its header.
#[derive(Debug)]
enum Mode {
    /// The rows.
    Rows,
    /// The key or value panel over the selected target.
    Panel(Panel),
    /// A browser login running: its ticket, the provider, what it showed
    /// and the worker, owned here so every way out cancels it.
    Waiting(Waiting),
    /// A reviewer read in flight after a stored credential: its ticket
    /// and the provider. The key is already stored; only the choice is
    /// pending, so Esc returns to the rows storing nothing more.
    Preparing(Preparing),
    /// The host panel: one presence gap at a time on the preparing
    /// ticket, before the reviewer. Esc returns to the rows storing
    /// nothing more.
    Host(Host),
    /// The reviewer picker: the provider's models with the saved pick
    /// preselected. Holds no secret.
    Choices(Choices),
    /// A login's end, kept until Esc or the ✕.
    Ended(Ended),
}

/// How a login ended (`docs/code-quality.md`, "Types").
#[derive(Debug)]
enum Outcome {
    /// The login stored its credentials.
    Done,
    /// The login failed with the seam's message.
    Failed(String),
    /// Stored, with this note: an empty or unreadable model list, or a
    /// reviewer save that failed naming its key. The credential stands.
    StoredWithNote(String),
}

/// A reviewer read in flight after a stored credential.
#[derive(Debug)]
struct Preparing {
    /// The read's ticket: only its result lands.
    ticket: LoginTicket,
    /// The provider logging in.
    name: String,
    /// The host settings saved this login: a gaps read naming one
    /// never reopens its panel.
    answered: Vec<String>,
    /// The project-mask notes the host saves reported, carried to the
    /// reviewer choices.
    notices: Vec<String>,
}

/// One presence gap at a time after a stored credential: the setting
/// with its declared environment variable, asked on the preparing
/// ticket before the reviewer. The answer echoes as typed: a host is
/// not a `Secret`.
#[derive(Debug)]
struct Host {
    /// The gaps read's ticket: only its result opened this panel.
    ticket: LoginTicket,
    /// The provider logging in.
    name: String,
    /// The unanswered gaps, in order: the head takes typing.
    gaps: Vec<HostGap>,
    /// The host settings saved this login.
    answered: Vec<String>,
    /// The answer being typed for the head gap.
    typed: Draft,
    /// The refused answer's line, until typing replaces it.
    refused: Option<String>,
    /// The project-mask notes the host saves reported.
    notices: Vec<String>,
}

/// The reviewer picker.
#[derive(Debug)]
struct Choices {
    /// The provider logging in.
    name: String,
    /// The provider's model IDs, in its order.
    models: Vec<String>,
    /// The highlighted row, if any: the preselected row at open, else
    /// none until the person moves or clicks.
    cursor: Option<usize>,
    /// The preselected model ID: the saved pick or package default, kept
    /// even when the list does not name it.
    selected: Option<String>,
    /// Discovery notices, shown under the list.
    notices: Vec<String>,
}

impl Choices {
    /// The picker over `choices`: the preselected row opens highlighted;
    /// a stale or absent preselection opens with no row highlighted.
    fn open(name: String, choices: crate::configure::ReviewerModels) -> Self {
        let cursor = choices
            .selected
            .as_deref()
            .and_then(|id| choices.models.iter().position(|model| model == id));
        Self {
            name,
            models: choices.models,
            cursor,
            selected: choices.selected,
            notices: choices.notices,
        }
    }

    /// Whether `id` is the preselected row.
    fn is_saved(&self, id: &str) -> bool {
        self.selected.as_deref().is_some_and(|saved| saved == id)
    }

    /// Whether the preselection names no listed row.
    fn is_stale(&self) -> bool {
        self.selected
            .as_deref()
            .is_some_and(|saved| !self.models.iter().any(|model| model == saved))
    }
}

/// A login's end: the provider and how it ended.
#[derive(Debug)]
struct Ended {
    /// The provider that was logging in.
    name: String,
    /// How it ended.
    outcome: Outcome,
}

/// What a waiting browser login shows below the rows: starting, the
/// URL to open, or the device code to enter at its URL
/// (`docs/code-quality.md`, "Types").
#[derive(Debug)]
enum Progress {
    /// Nothing arrived yet.
    Starting,
    /// The URL to open.
    Url(String),
    /// The device code to enter at `url`; opens nothing.
    DeviceCode { url: String, code: String },
}

/// A browser login in flight.
#[derive(Debug)]
struct Waiting {
    /// The login's ticket: only its events land.
    ticket: LoginTicket,
    /// The provider logging in.
    name: String,
    /// What the login showed so far.
    progress: Progress,
    /// The running login, once started.
    worker: Option<LoginWorker>,
}

/// The `/login` view's state.
#[derive(Debug)]
pub(crate) struct Login {
    targets: Vec<LoginTarget>,
    items: Vec<Item>,
    /// The focused row, an index into `items`: a target whenever one
    /// exists, else 0. Headings, the gap and the note never take it.
    selected: usize,
    /// The first body row shown: the wheel moves it three rows a step,
    /// and the keys move it only as far as needed to keep the focus
    /// shown.
    top: usize,
    mode: Mode,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
}

/// The note with the carried mask notes under it.
fn noted(mut note: String, carried: &[String]) -> String {
    for line in carried {
        note.push('\n');
        note.push_str(line);
    }
    note
}

impl Login {
    /// The view, its rows read.
    pub(crate) fn open(ctx: &Ctx<'_>) -> Self {
        let (targets, items, said) = match ctx.seam.login_targets() {
            Ok(targets) => {
                let items = build(&targets);
                (targets, items, Vec::new())
            }
            Err(error) => (Vec::new(), Vec::new(), vec![error.message]),
        };
        let mut login = Self {
            targets,
            items,
            selected: 0,
            top: 0,
            mode: Mode::Rows,
            said,
        };
        // Focus opens on the first target; with none it stays at 0.
        login.selected = login
            .items
            .iter()
            .position(|item| matches!(item, Item::Target(_)))
            .unwrap_or(0);
        login
    }

    /// The selected row.
    fn selected(&self) -> Option<Item> {
        self.items.get(self.selected).copied()
    }

    /// Draws the list in the shared overlay frame, centred: dim
    /// headings, one choice row per target with its tag, the gap, then
    /// anything the last action said, at least three body rows before
    /// the window clips them, and the legend footer. A step draws its
    /// own overlay; any other mode draws nothing yet.
    /// The frame as drawn, without selection noting: tests draw it
    /// standalone, while production notes through [`draw_noted`].
    #[cfg(test)]
    pub(crate) fn draw(&self, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) -> Rect {
        let Some((framed, _)) = self.framed(area) else {
            return Rect::default();
        };
        overlay::draw(buf, area, &framed, overlay::Place::Centre, targets)
    }

    /// Draws the list or the open step as [`draw`], and notes the
    /// overlay's kept body rows for the selection with their copy text
    /// beside painting its highlight
    /// (`docs/tui.md`, "Selection and copy").
    pub(crate) fn draw_noted(
        &self,
        app: &App,
        area: Rect,
        buf: &mut Buffer,
        targets: &mut Vec<Target>,
    ) -> Rect {
        let Some((framed, joins)) = self.framed(area) else {
            return Rect::default();
        };
        let slab = overlay::draw(buf, area, &framed, overlay::Place::Centre, targets);
        overlay::note_overlay(
            app,
            buf,
            slab,
            &framed,
            area,
            overlay::Place::Centre,
            &joins,
        );
        slab
    }

    /// The overlay [`draw`] shows at `area` with each kept body row's
    /// copy join: the list or the open step's frame, `None` when a step
    /// draws nothing yet. The frame is what the draw draws, so the
    /// selection notes the rows the frame kept.
    fn framed(&self, area: Rect) -> Option<(overlay::Overlay, Vec<Join>)> {
        if matches!(
            self.mode,
            Mode::Waiting(_)
                | Mode::Panel(_)
                | Mode::Preparing(_)
                | Mode::Host(_)
                | Mode::Choices(_)
                | Mode::Ended(_)
        ) {
            return self.step_framed(area);
        }
        Some(self.list_framed(area))
    }

    /// The provider list's frame at `area` with each body row's copy
    /// join.
    fn list_framed(&self, area: Rect) -> (overlay::Overlay, Vec<Join>) {
        let width = overlay::width(u16::MAX, 96, area.width);
        let inner_w = overlay::inner(width);
        // The tags keep one column: the widest tag among the targets
        // stays on screen, so names are cut to what is left first.
        let tag = self
            .targets
            .iter()
            .map(|target| {
                format::width(match target.kind {
                    LoginKind::Browser => "browser",
                    LoginKind::Key => "key",
                    LoginKind::Secret => "",
                })
            })
            .max()
            .unwrap_or(0);
        let budget = usize::from(inner_w).saturating_sub(4).saturating_sub(tag);
        // One choice row per target, each exactly one row: the cut
        // names leave every tag a single row at any area width.
        let mut cut: Vec<(usize, String, &'static str)> = Vec::new();
        for (at, item) in self.items.iter().enumerate() {
            if let Item::Target(t) = *item
                && let Some(target) = self.targets.get(t)
            {
                cut.push((
                    at,
                    format::cut(&target.name, budget),
                    match target.kind {
                        LoginKind::Browser => "browser",
                        LoginKind::Key => "key",
                        LoginKind::Secret => "",
                    },
                ));
            }
        }
        let listed: Vec<(&str, &str)> = cut
            .iter()
            .map(|(_, name, tag)| (name.as_str(), *tag))
            .collect();
        let focused = cut
            .iter()
            .position(|(at, _, _)| *at == self.selected)
            .unwrap_or(0);
        let mut choices = overlay::choices(&listed, focused, u16::MAX);
        // Each target row takes clicks over its whole row; headings,
        // the gap and the note carry none.
        for (row, (at, _, _)) in choices.iter_mut().zip(cut.iter()) {
            row.targets
                .push((0, u16::MAX, TargetId::View(Spot::Row(*at))));
        }
        let mut choices = choices.into_iter();
        let dim = Style::new().add_modifier(Modifier::DIM);
        // Each kept body row with its copy join: single rows start
        // their own line, wrapped continuations join with the space
        // the wrap dropped (`docs/tui.md`, "Selection and copy").
        let mut body: Vec<(overlay::Row, Join)> = Vec::new();
        for item in &self.items {
            match *item {
                Item::Heading(Group::Providers) => {
                    body.push((text_row("Providers", dim), Join::Break));
                }
                Item::Heading(Group::Secrets) => {
                    body.push((text_row("Secrets", dim), Join::Break));
                }
                Item::Target(_) => {
                    body.push((choices.next().unwrap_or_else(blank_row), Join::Break));
                }
                Item::Gap => body.push((blank_row(), Join::Break)),
                Item::Note => body.push((
                    text_row(
                        "No provider is installed, and no installed extension declares a secret.",
                        dim,
                    ),
                    Join::Break,
                )),
            }
        }
        // What the last action said follows the items, one blank row
        // between; with no items it is the whole body. The rows wrap
        // at the preferred width and are never cut sideways.
        if !self.said.is_empty() {
            if !body.is_empty() {
                body.push((blank_row(), Join::Break));
            }
            for line in &self.said {
                body.extend(wrapped_rows(line, usize::from(inner_w), Style::new()));
            }
        }
        for _ in body.len()..3 {
            body.push((blank_row(), Join::Break));
        }
        // The footer carries trailing blanks up to the widest of every
        // body row, not only the shown ones, so the overlay's width
        // never depends on which rows the window shows.
        let wide = body
            .iter()
            .map(|(row, _)| {
                row.spans
                    .iter()
                    .map(|span| format::width(&span.content))
                    .sum()
            })
            .max()
            .unwrap_or(0);
        let mut footer = overlay::legend(&[("↑↓", "move"), ("Enter", "log in"), ("Esc", "close")]);
        let have: usize = footer
            .spans
            .iter()
            .map(|span| format::width(&span.content))
            .sum();
        footer
            .spans
            .push(Span::raw(" ".repeat(wide.saturating_sub(have))));
        let shown = shown_body_rows(usize::from(area.height));
        // The window starts at the wheel's top row, moved only as far
        // as needed to keep the focus shown: the keys never re-derive
        // the window from the focus, so a wheeled position survives
        // them, and a shorter body clamps it back into range.
        let start = if body.is_empty() || shown == 0 {
            0
        } else {
            let end = body.len().saturating_sub(shown);
            let low = self.selected.saturating_add(1).saturating_sub(shown);
            self.top.clamp(low, self.selected).min(end)
        };
        let end = start.saturating_add(shown).min(body.len());
        let kept: Vec<(overlay::Row, Join)> = body
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect();
        let joins = kept.iter().map(|(_, join)| *join).collect();
        let framed = overlay::Overlay {
            title: Some(("Log in".to_owned(), Some(Span::raw("✕")))),
            close: Some(TargetId::View(Spot::Close)),
            body: kept.into_iter().map(|(row, _)| row).collect(),
            footer: Some(footer),
            prefer: 96,
        };
        (framed, joins)
    }

    /// The target rows' indices in display order.
    fn target_rows(&self) -> Vec<usize> {
        self.items
            .iter()
            .enumerate()
            .filter_map(|(at, item)| matches!(item, Item::Target(_)).then_some(at))
            .collect()
    }

    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if matches!(self.mode, Mode::Panel(_)) {
            self.panel_key(key, ctx)
        } else if matches!(self.mode, Mode::Waiting(_)) {
            self.waiting_key(key)
        } else if matches!(self.mode, Mode::Preparing(_)) {
            // The read is in flight: Esc returns to the rows storing
            // nothing more, and anything else waits on.
            if *key == Key::Esc {
                self.mode = Mode::Rows;
                self.said.clear();
            }
            Act::Stay
        } else if matches!(self.mode, Mode::Host(_)) {
            self.host_key(key, ctx)
        } else if matches!(self.mode, Mode::Choices(_)) {
            self.choices_key(key, ctx)
        } else if matches!(self.mode, Mode::Ended(_)) {
            // Done and failed stay until Esc or the ✕; Esc goes back
            // to the list with the focus kept.
            if *key == Key::Esc {
                self.mode = Mode::Rows;
            }
            Act::Stay
        } else {
            self.rows_key(key, ctx)
        }
    }

    /// A key over the rows: move over the target rows only, open the
    /// selected row, or close.
    fn rows_key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            return Act::Close;
        }
        if *key == Key::Enter {
            if let Some(Item::Target(at)) = self.selected() {
                return self.open_target(at);
            }
            return Act::Stay;
        }
        if !matches!(*key, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
            return Act::Stay;
        }
        // With no body row shown both selections stay put.
        if shown_body_rows(ctx.height) == 0 {
            return Act::Stay;
        }
        let targets = self.target_rows();
        if targets.is_empty() {
            return Act::Stay;
        }
        let before = self.selected;
        if *key == Key::Up {
            // The nearest target above, clamped at the first.
            self.selected = targets
                .iter()
                .copied()
                .rfind(|at| *at < before)
                .unwrap_or(before);
        } else if *key == Key::Down {
            // The nearest target below, clamped at the last.
            self.selected = targets
                .iter()
                .copied()
                .find(|at| *at > before)
                .unwrap_or(before);
        } else {
            // A page is the shown body rows less one, at least one.
            let page = shown_body_rows(ctx.height).saturating_sub(1).max(1);
            if *key == Key::PageUp {
                let land = before.saturating_sub(page);
                // The nearest target at or above the landing row, else
                // the first target below it.
                self.selected = targets
                    .iter()
                    .copied()
                    .rfind(|at| *at <= land)
                    .or(targets.first().copied())
                    .unwrap_or(before);
            } else {
                let land = before
                    .saturating_add(page)
                    .min(self.items.len().saturating_sub(1));
                // The nearest target at or below the landing row, else
                // the last target above it.
                self.selected = targets
                    .iter()
                    .copied()
                    .find(|at| *at >= land)
                    .or(targets.last().copied())
                    .unwrap_or(before);
            }
        }
        Act::Stay
    }

    /// Shifts the provider list's window one wheel step: three body
    /// rows, clamped at either end, keeping the focus while it shows,
    /// else moving it to the nearest shown target row. Steps and detail
    /// views take no wheel: they do not scroll, so the wheel is
    /// swallowed there.
    pub(crate) fn wheel(&mut self, up: bool, ctx: &Ctx<'_>) -> Act {
        if !matches!(self.mode, Mode::Rows) {
            return Act::Stay;
        }
        let shown = shown_body_rows(ctx.height);
        let targets = self.target_rows();
        (self.selected, self.top) = crate::swapped::wheel_targets_window(
            up,
            self.selected,
            self.top,
            self.body_len(ctx.width),
            shown,
            &targets,
        );
        Act::Stay
    }

    /// How many body rows the provider list holds at `width` wide, shared
    /// with `/logout` so both windows shift alike.
    fn body_len(&self, width: usize) -> usize {
        crate::swapped::body_len(self.items.len(), &self.said, width)
    }

    /// Opens the selected row: a key or secret row opens the panel, a
    /// browser row starts the browser login through the same steps as
    /// `fiber login`.
    fn open_target(&mut self, at: usize) -> Act {
        let Some((name, kind)) = self
            .targets
            .get(at)
            .map(|target| (target.name.clone(), target.kind))
        else {
            return Act::Stay;
        };
        match kind {
            LoginKind::Key | LoginKind::Secret => {
                self.said.clear();
                self.mode = Mode::Panel(Panel {
                    target: at,
                    key: SecretField(String::new()),
                    label: Draft::default(),
                    focus: Focus::Key,
                });
                Act::Stay
            }
            LoginKind::Browser => Act::Login(name),
        }
    }

    /// A key while a browser login waits: Esc returns to the rows storing
    /// nothing, `y` copies the shown URL or the shown code, and anything
    /// else waits on.
    fn waiting_key(&mut self, key: &Key) -> Act {
        if *key == Key::Esc {
            self.mode = Mode::Rows;
            self.said.clear();
            return Act::Stay;
        }
        if *key == Key::Char('y')
            && let Mode::Waiting(waiting) = &self.mode
        {
            match &waiting.progress {
                Progress::Url(url) => return Act::Copy(url.clone()),
                Progress::DeviceCode { url, .. } => return Act::Copy(url.clone()),
                Progress::Starting => {}
            }
        }
        Act::Stay
    }

    /// Waits on the browser login `ticket`: the rows stay, and the view
    /// shows the login's progress below them until it ends.
    pub(crate) fn wait(&mut self, ticket: LoginTicket) {
        let name = self
            .selected()
            .and_then(|item| match item {
                Item::Target(at) => self.targets.get(at).map(|target| target.name.clone()),
                Item::Heading(_) | Item::Gap | Item::Note => None,
            })
            .unwrap_or_default();
        self.said.clear();
        self.mode = Mode::Waiting(Waiting {
            ticket,
            name,
            progress: Progress::Starting,
            worker: None,
        });
    }

    /// Keeps the started worker when waiting on its ticket; a worker for
    /// another ticket is dropped, cancelling it at once. A preparing or
    /// choosing view holds no worker: the browser run already ended, and
    /// a reviewer-only read owns no cancel handle.
    pub(crate) fn started(&mut self, worker: LoginWorker) {
        match &mut self.mode {
            Mode::Waiting(waiting) if waiting.ticket == worker.ticket() => {
                waiting.worker = Some(worker);
            }
            Mode::Rows
            | Mode::Panel(_)
            | Mode::Waiting(_)
            | Mode::Preparing(_)
            | Mode::Host(_)
            | Mode::Choices(_)
            | Mode::Ended(_) => drop(worker),
        }
    }

    /// Folds the waiting ticket's progress into the view: an event for
    /// another ticket changes nothing and opens nothing. `Open` returns
    /// the URL to open. Gaps and reviewer results fold on their own
    /// tickets, never here.
    pub(crate) fn step(&mut self, ticket: LoginTicket, step: LoginStep) -> Option<String> {
        match step {
            LoginStep::Reviewer(result) => self.step_reviewer(ticket, result),
            LoginStep::Host(result) => self.step_host(ticket, result),
            LoginStep::Open(_) | LoginStep::Code { .. } | LoginStep::Done(_) => {
                self.step_waiting(ticket, step)
            }
        }
    }

    /// Folds the waiting ticket's progress into the view: an event for
    /// another ticket changes nothing and opens nothing. `Open` returns
    /// the URL to open. A successful browser `Done` keeps its ticket
    /// through preparation into the choices; any other end stays as its
    /// step until Esc or the ✕.
    fn step_waiting(&mut self, ticket: LoginTicket, step: LoginStep) -> Option<String> {
        let Mode::Waiting(waiting) = &mut self.mode else {
            return None;
        };
        if waiting.ticket != ticket {
            return None;
        }
        match step {
            LoginStep::Open(url) => {
                waiting.progress = Progress::Url(url.clone());
                Some(url)
            }
            LoginStep::Code { url, code } => {
                waiting.progress = Progress::DeviceCode { url, code };
                None
            }
            LoginStep::Done(result) => {
                let name = waiting.name.clone();
                match result {
                    // The credential is stored; the host stage asks the
                    // presence gaps on this same ticket before the
                    // reviewer.
                    Ok(_) => {
                        self.mode = Mode::Preparing(Preparing {
                            ticket,
                            name,
                            answered: Vec::new(),
                            notices: Vec::new(),
                        });
                    }
                    Err(error) => {
                        self.mode = Mode::Ended(Ended {
                            name,
                            outcome: Outcome::Failed(error.message),
                        });
                    }
                }
                None
            }
            LoginStep::Reviewer(_) | LoginStep::Host(_) => {
                unreachable!("gaps and reviewer results fold before the waiting step")
            }
        }
    }

    /// Folds the gaps read into a preparing view on its ticket: gaps
    /// still unanswered open the host panel, while none, or a failed
    /// read, leave the reviewer read to land. Any other mode or ticket
    /// changes nothing: a late result cannot reopen or write.
    fn step_host(
        &mut self,
        ticket: LoginTicket,
        result: Result<Vec<HostGap>, crate::configure::ConfigureError>,
    ) -> Option<String> {
        let (name, answered, notices) = match &self.mode {
            Mode::Preparing(preparing) if preparing.ticket == ticket => (
                preparing.name.clone(),
                preparing.answered.clone(),
                preparing.notices.clone(),
            ),
            Mode::Rows
            | Mode::Panel(_)
            | Mode::Waiting(_)
            | Mode::Preparing(_)
            | Mode::Host(_)
            | Mode::Choices(_)
            | Mode::Ended(_) => return None,
        };
        let gaps: Vec<HostGap> = match result {
            Ok(gaps) => gaps
                .into_iter()
                .filter(|gap| !answered.contains(&gap.name))
                .collect(),
            // The reviewer error follows into the stored-with-note path.
            Err(_) => return None,
        };
        if gaps.is_empty() {
            return None;
        }
        self.mode = Mode::Host(Host {
            ticket,
            name,
            gaps,
            answered,
            typed: Draft::default(),
            refused: None,
            notices,
        });
        None
    }

    /// Folds the read's result into a preparing view on its ticket: the
    /// choices, or the stored end with its note. Any other mode or ticket
    /// changes nothing: a late result cannot reopen or write.
    fn step_reviewer(
        &mut self,
        ticket: LoginTicket,
        result: Result<crate::configure::ReviewerModels, crate::configure::ConfigureError>,
    ) -> Option<String> {
        let Mode::Preparing(preparing) = &self.mode else {
            return None;
        };
        if preparing.ticket != ticket {
            return None;
        }
        let name = preparing.name.clone();
        let carried = preparing.notices.clone();
        match result {
            Ok(choices) => {
                if choices.models.is_empty() {
                    let mut note = choices.notices.join("\n");
                    if note.is_empty() {
                        note = format!(
                            "`{name}` lists no models; the credential stays, with no reviewer set."
                        );
                    }
                    self.mode = Mode::Ended(Ended {
                        name,
                        outcome: Outcome::StoredWithNote(noted(note, &carried)),
                    });
                } else {
                    let mut opened = Choices::open(name, choices);
                    opened.notices.extend(carried);
                    self.mode = Mode::Choices(opened);
                }
            }
            Err(error) => {
                self.mode = Mode::Ended(Ended {
                    name,
                    outcome: Outcome::StoredWithNote(noted(error.message, &carried)),
                });
            }
        }
        None
    }

    /// The reviewer read queued for `ticket` after `name`'s key was
    /// stored: the panel gives way to the preparing step.
    pub(crate) fn wait_reviewer(&mut self, ticket: LoginTicket, name: String) {
        self.said.clear();
        self.mode = Mode::Preparing(Preparing {
            ticket,
            name,
            answered: Vec::new(),
            notices: Vec::new(),
        });
    }

    /// A key in the panel: type, move between the fields, store, or close
    /// it storing nothing. A stored key queues its reviewer read; a
    /// stored secret ends the login at once.
    fn panel_key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            self.mode = Mode::Rows;
            self.said.clear();
            return Act::Stay;
        }
        if *key == Key::Enter {
            return self.submit(ctx);
        }
        let Mode::Panel(panel) = &mut self.mode else {
            return Act::Stay;
        };
        let secret = self
            .targets
            .get(panel.target)
            .is_some_and(|target| target.kind == LoginKind::Secret);
        if *key == Key::Tab || *key == Key::BackTab {
            if !secret {
                panel.focus = match panel.focus {
                    Focus::Key => Focus::Label,
                    Focus::Label => Focus::Key,
                };
            }
            return Act::Stay;
        }
        match panel.focus {
            Focus::Key => {
                if let Key::Char(ch) = key {
                    panel.key.push(*ch);
                } else if *key == Key::Backspace {
                    panel.key.pop();
                }
            }
            Focus::Label => {
                if let Key::Char(ch) = key {
                    panel.label.insert(*ch);
                } else if *key == Key::Backspace {
                    panel.label.backspace();
                }
            }
        }
        Act::Stay
    }

    /// A key over the host panel: type the head gap's answer, save it
    /// through the seam, or go back storing nothing more. A refused
    /// answer stays, with its line, until typing replaces it.
    fn host_key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            self.mode = Mode::Rows;
            self.said.clear();
            return Act::Stay;
        }
        let Mode::Host(host) = &mut self.mode else {
            return Act::Stay;
        };
        if *key == Key::Enter {
            return self.submit_host(ctx);
        }
        if let Key::Char(ch) = key {
            host.typed.insert(*ch);
            host.refused = None;
        } else if *key == Key::Backspace {
            host.typed.backspace();
            host.refused = None;
        }
        Act::Stay
    }

    /// Saves the head gap's answer: the answer trimmed, global through
    /// the seam into the same file `fiber login` writes. A saved gap
    /// asks the next; the last queues its reviewer read. A refused
    /// answer is not a host: the Usage refusal names it and asks again,
    /// anything else ends the stored login with its note. A
    /// project-layer empty hiding the saved value is masked: its note
    /// rides to the choices.
    fn submit_host(&mut self, ctx: &Ctx<'_>) -> Act {
        let (name, gap, value) = match &mut self.mode {
            Mode::Host(host) => match host.gaps.first() {
                Some(gap) => (
                    host.name.clone(),
                    gap.clone(),
                    host.typed.expand().trim().to_owned(),
                ),
                None => return Act::Stay,
            },
            Mode::Rows
            | Mode::Waiting(_)
            | Mode::Preparing(_)
            | Mode::Panel(_)
            | Mode::Choices(_)
            | Mode::Ended(_) => return Act::Stay,
        };
        match ctx.seam.save_host(&name, &gap.name, &value) {
            Ok(saved) => {
                let Mode::Host(host) = &mut self.mode else {
                    return Act::Stay;
                };
                host.answered.push(gap.name.clone());
                if let Some(masked) = saved.masked {
                    // The same one-note text `fiber login` prints, so
                    // the terminal and the TUI never drift.
                    host.notices.push(format!(
                        "fiber: saved `{}` globally, but {} sets it empty and hides it.",
                        gap.name,
                        masked.display()
                    ));
                }
                host.gaps.remove(0);
                host.typed = Draft::default();
                host.refused = None;
                if !host.gaps.is_empty() {
                    return Act::Stay;
                }
                let Mode::Host(host) = &self.mode else {
                    return Act::Stay;
                };
                let name = host.name.clone();
                self.mode = Mode::Preparing(Preparing {
                    ticket: host.ticket,
                    name: name.clone(),
                    answered: host.answered.clone(),
                    notices: host.notices.clone(),
                });
                Act::Reviewer(name)
            }
            Err(error) => {
                if error.code == ErrorCode::Usage {
                    // Only a not-a-host answer refuses with Usage here:
                    // the line names the setting, never the value.
                    let Mode::Host(host) = &mut self.mode else {
                        return Act::Stay;
                    };
                    host.refused = Some(format!("`{}` is not a host; type a host.", gap.name));
                } else {
                    self.mode = Mode::Ended(Ended {
                        name,
                        outcome: Outcome::StoredWithNote(error.message),
                    });
                }
                Act::Stay
            }
        }
    }

    /// A key over the reviewer picker: move over the models, save the
    /// highlighted row or the kept preselection, or close storing nothing.
    /// With no row highlighted and nothing preselected Enter stays open;
    /// text and pastes never reach the draft. Saving writes the one
    /// provider key through the seam.
    fn choices_key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            self.mode = Mode::Rows;
            self.said.clear();
            return Act::Stay;
        }
        let Mode::Choices(choices) = &mut self.mode else {
            return Act::Stay;
        };
        if choices.models.is_empty() {
            return Act::Stay;
        }
        if *key == Key::Down {
            choices.cursor = Some(match choices.cursor {
                None => 0,
                Some(at) => (at.saturating_add(1)).min(choices.models.len().saturating_sub(1)),
            });
        } else if *key == Key::Up {
            choices.cursor = Some(match choices.cursor {
                None => choices.models.len().saturating_sub(1),
                Some(at) => at.saturating_sub(1),
            });
        } else if *key == Key::Enter {
            let pick = choices
                .cursor
                .and_then(|at| choices.models.get(at).cloned())
                .or_else(|| choices.selected.clone());
            let Some(id) = pick else {
                return Act::Stay;
            };
            let name = choices.name.clone();
            match ctx.seam.set_reviewer(&name, &id) {
                Ok(()) => {
                    self.mode = Mode::Ended(Ended {
                        name,
                        outcome: Outcome::Done,
                    });
                }
                Err(error) => {
                    self.mode = Mode::Ended(Ended {
                        name,
                        outcome: Outcome::StoredWithNote(error.message),
                    });
                }
            }
        }
        Act::Stay
    }

    /// Stores the panel's key: the key trimmed, the label trimmed with an
    /// empty one meaning none. A stored key queues its reviewer read; a
    /// stored secret ends the login at once. Refusal ends as the failed
    /// step; a refusal drops the key and the label.
    fn submit(&mut self, ctx: &Ctx<'_>) -> Act {
        let (index, label, secret) = match &mut self.mode {
            Mode::Panel(panel) => (panel.target, panel.label.expand(), panel.key.take()),
            Mode::Rows
            | Mode::Waiting(_)
            | Mode::Preparing(_)
            | Mode::Host(_)
            | Mode::Choices(_)
            | Mode::Ended(_) => {
                return Act::Stay;
            }
        };
        let Some(target) = self.targets.get(index) else {
            return Act::Stay;
        };
        let (name, kind) = (target.name.clone(), target.kind);
        let label = if kind == LoginKind::Secret {
            None
        } else {
            let trimmed = label.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        };
        match ctx.seam.store_key(&name, label.as_deref(), secret) {
            Ok(_) => {
                if kind == LoginKind::Secret {
                    self.mode = Mode::Ended(Ended {
                        name,
                        outcome: Outcome::Done,
                    });
                    Act::Stay
                } else {
                    Act::Reviewer(name)
                }
            }
            Err(error) => {
                self.mode = Mode::Ended(Ended {
                    name,
                    outcome: Outcome::Failed(error.message),
                });
                Act::Stay
            }
        }
    }

    /// Handles one editing key: the key field takes a paste whole and
    /// clears on a deleted word, ignoring every other edit; the label
    /// field edits as `/settings`' does.
    pub(crate) fn edit_key(&mut self, edit: &Edit) {
        if let Mode::Host(host) = &mut self.mode {
            // The host pastes whole like the label: it is not a secret.
            match edit {
                Edit::Paste(text) => text.chars().for_each(|ch| host.typed.insert(ch)),
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
                | Edit::Delete => host.typed.edit(edit.clone()),
            }
            return;
        }
        let Mode::Panel(panel) = &mut self.mode else {
            return;
        };
        match panel.focus {
            Focus::Key => match edit {
                Edit::Paste(text) => panel.key.push_str(text),
                // The masked key has no cursor to move: both clear it.
                Edit::DeleteWord | Edit::DeleteToLineStart => panel.key.clear(),
                Edit::Left
                | Edit::Right
                | Edit::ShiftEnter
                | Edit::CtrlJ
                | Edit::WordLeft
                | Edit::WordRight
                | Edit::LineStart
                | Edit::LineEnd
                | Edit::Delete => {}
            },
            Focus::Label => match edit {
                Edit::Paste(text) => text.chars().for_each(|ch| panel.label.insert(ch)),
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
                | Edit::Delete => panel.label.edit(edit.clone()),
            },
        }
    }

    /// A click: the ✕ closes, a target row over the rows selects it, a
    /// model row over the choices highlights it, and a row while any
    /// other step is open keeps its mode. A click on a heading, the gap,
    /// the note, past the last item or past the last model does nothing.
    pub(crate) fn click(&mut self, spot: Spot, _ctx: &Ctx<'_>) -> Act {
        match spot {
            Spot::Close => Act::Close,
            Spot::Switch { .. } | Spot::Revoke(_) | Spot::Cell(_, _) | Spot::Item(_) => Act::Stay,
            Spot::Row(at) => {
                if let Mode::Choices(choices) = &mut self.mode {
                    if choices.models.get(at).is_some() {
                        choices.cursor = Some(at);
                    }
                    return Act::Stay;
                }
                if matches!(
                    self.mode,
                    Mode::Panel(_)
                        | Mode::Waiting(_)
                        | Mode::Preparing(_)
                        | Mode::Host(_)
                        | Mode::Ended(_)
                ) {
                    return Act::Stay;
                }
                if !matches!(self.items.get(at), Some(Item::Target(_))) {
                    return Act::Stay;
                }
                self.selected = at;
                self.said.clear();
                Act::Stay
            }
        }
    }
}

impl Login {
    /// The step's provider name, if any mode but the rows is open.
    fn step_name(&self) -> Option<String> {
        match &self.mode {
            Mode::Rows => None,
            Mode::Waiting(waiting) => Some(waiting.name.clone()),
            Mode::Preparing(preparing) => Some(preparing.name.clone()),
            Mode::Host(host) => Some(host.name.clone()),
            Mode::Choices(choices) => Some(choices.name.clone()),
            Mode::Panel(panel) => self
                .targets
                .get(panel.target)
                .map(|target| target.name.clone()),
            Mode::Ended(ended) => Some(ended.name.clone()),
        }
    }

    /// The step body's row count: the list's body rows when no read
    /// error exists, at least three. A step is reachable only with a
    /// target, so the list's `said` is empty and the items are the body.
    fn body_count(&self) -> usize {
        self.items.len().max(3)
    }

    /// The open step's frame at `area` with each kept body row's copy
    /// join: one overlay titled with the provider's name, the body
    /// exactly the list's row count and the footer padded to the shared
    /// width, so the overlay never jumps between steps. The body is
    /// built top to bottom and cut at the row count in order; over a
    /// short area it is clipped to the body budget the list shows
    /// there. `None` when the step draws nothing yet. Each kept row
    /// carries its copy join: single rows start their own line,
    /// wrapped continuations join with the space the wrap dropped
    /// (`docs/tui.md`, "Selection and copy").
    fn step_framed(&self, area: Rect) -> Option<(overlay::Overlay, Vec<Join>)> {
        let name = self.step_name()?;
        let content = step_content_width(&name, &self.targets);
        let content_u16 = u16::try_from(content).unwrap_or(u16::MAX);
        let slab_w = overlay::width(content_u16, 96, area.width);
        let inner = usize::from(overlay::inner(slab_w));
        let dim = Style::new().add_modifier(Modifier::DIM);
        let (content_rows, legend): (Vec<(overlay::Row, Join)>, overlay::Row) = match &self.mode {
            Mode::Rows => return None,
            Mode::Waiting(waiting) => match &waiting.progress {
                Progress::Starting => (
                    wrapped_rows(
                        &format!("Starting the login to {}…", name),
                        inner,
                        Style::new(),
                    ),
                    overlay::legend(&[("Esc", "back")]),
                ),
                Progress::Url(url) => {
                    let mut rows = wrapped_rows(
                        &format!("Open this URL to log in to {name}:"),
                        inner,
                        Style::new(),
                    );
                    rows.push((
                        text_row(tail(url, inner), Style::new().fg(Role::Accent.color())),
                        Join::Break,
                    ));
                    rows.extend(wrapped_rows("Waiting for the browser…", inner, dim));
                    (rows, overlay::legend(&[("y", "copy URL"), ("Esc", "back")]))
                }
                Progress::DeviceCode { url, code } => {
                    let mut rows = wrapped_rows(
                        &format!("Enter the code {code} at this URL to log in to {name}:"),
                        inner,
                        Style::new(),
                    );
                    rows.push((
                        text_row(tail(url, inner), Style::new().fg(Role::Accent.color())),
                        Join::Break,
                    ));
                    rows.extend(wrapped_rows("Waiting for the browser…", inner, dim));
                    (rows, overlay::legend(&[("y", "copy URL"), ("Esc", "back")]))
                }
            },
            Mode::Panel(panel) => {
                let target = self.targets.get(panel.target)?;
                if target.kind == LoginKind::Secret {
                    let mut rows =
                        wrapped_rows(&format!("Value for {}:", name), inner, Style::new());
                    rows.push((dots_row(&panel.key.dots(), inner, true), Join::Break));
                    (
                        rows,
                        overlay::legend(&[("Enter", "submit"), ("Esc", "back")]),
                    )
                } else if panel.focus == Focus::Label {
                    let mut rows = vec![(label_edit_row(&panel.label, inner), Join::Break)];
                    rows.extend(wrapped_rows(
                        &format!("Key for {name}:"),
                        inner,
                        Style::new(),
                    ));
                    rows.push((dots_row(&panel.key.dots(), inner, false), Join::Break));
                    (
                        rows,
                        overlay::legend(&[
                            ("Tab", "key or label"),
                            ("Enter", "submit"),
                            ("Esc", "back"),
                        ]),
                    )
                } else {
                    let expanded = panel.label.expand();
                    let shown = if expanded.trim().is_empty() {
                        "default".to_owned()
                    } else {
                        expanded
                    };
                    // Cut to one row so a long label never pushes the
                    // key prompt, the dots and the cursor out of the body.
                    let mut rows = vec![(
                        text_row(format::cut(&format!("Label (--as): {shown}."), inner), dim),
                        Join::Break,
                    )];
                    rows.extend(wrapped_rows(
                        &format!("Key for {name}:"),
                        inner,
                        Style::new(),
                    ));
                    rows.push((dots_row(&panel.key.dots(), inner, true), Join::Break));
                    (
                        rows,
                        overlay::legend(&[
                            ("Tab", "key or label"),
                            ("Enter", "submit"),
                            ("Esc", "back"),
                        ]),
                    )
                }
            }
            Mode::Ended(ended) => match &ended.outcome {
                Outcome::Done => (
                    wrapped_rows(
                        &format!("✓ Logged in to {name}."),
                        inner,
                        Style::new().fg(Role::Success.color()),
                    ),
                    overlay::legend(&[("Esc", "back")]),
                ),
                Outcome::Failed(reason) => {
                    let head = format!("Login to {name} failed: ");
                    let dot = if reason.ends_with('.') { "" } else { "." };
                    (
                        failed_rows(&head, reason, dot, inner),
                        overlay::legend(&[("Esc", "back")]),
                    )
                }
                Outcome::StoredWithNote(note) => {
                    let mut rows = wrapped_rows(
                        &format!("✓ Logged in to {name}."),
                        inner,
                        Style::new().fg(Role::Success.color()),
                    );
                    rows.extend(wrapped_rows(note, inner, Style::new()));
                    (rows, overlay::legend(&[("Esc", "back")]))
                }
            },
            Mode::Preparing(_) => (
                wrapped_rows(
                    &format!("Reading {name}'s reviewer models…"),
                    inner,
                    Style::new(),
                ),
                overlay::legend(&[("Esc", "back")]),
            ),
            Mode::Host(host) => {
                let gap = host.gaps.first()?;
                let prompt = match gap.env.as_deref() {
                    Some(env) => format!("`{}` (`{env}`):", gap.name),
                    None => format!("`{}`:", gap.name),
                };
                let mut rows = wrapped_rows(&prompt, inner, Style::new());
                rows.push((host_edit_row(&host.typed, inner), Join::Break));
                if let Some(refused) = host.refused.as_deref() {
                    rows.extend(wrapped_rows(refused, inner, Style::new()));
                }
                for notice in &host.notices {
                    rows.extend(wrapped_rows(notice, inner, dim));
                }
                (rows, overlay::legend(&[("Enter", "save"), ("Esc", "back")]))
            }
            Mode::Choices(choices) => {
                choice_rows(choices, inner, self.body_count(), usize::from(area.height))
            }
        };
        let kept = self.body_count();
        let mut body = content_rows;
        body.truncate(kept);
        while body.len() < kept {
            body.push((blank_row(), Join::Break));
        }
        body.truncate(shown_body_rows(usize::from(area.height)));
        let mut footer = legend;
        let have: usize = footer
            .spans
            .iter()
            .map(|span| format::width(&span.content))
            .sum();
        footer
            .spans
            .push(Span::raw(" ".repeat(content.saturating_sub(have))));
        let joins = body.iter().map(|(_, join)| *join).collect();
        let framed = overlay::Overlay {
            title: Some((name, Some(Span::raw("✕")))),
            close: Some(TargetId::View(Spot::Close)),
            body: body.into_iter().map(|(row, _)| row).collect(),
            footer: Some(footer),
            prefer: 96,
        };
        Some((framed, joins))
    }
}

/// The picker rows for `choices` at `inner` columns: the stale line when
/// the preselection names no listed row, the models windowed so the
/// highlighted row stays visible, then the discovery notices while room
/// remains. The stale line is clipped to leave a row for a model whenever
/// the body has room for one, so a long stale preselection never hides
/// every choice while arrows still move over them. The window shares the
/// step's fixed body count, clipped to what the area shows, so the overlay
/// never jumps between steps. Model IDs are cut to one row; the whole ID
/// is still saved.
fn choice_rows(
    choices: &Choices,
    inner: usize,
    kept: usize,
    height: usize,
) -> (Vec<(overlay::Row, Join)>, overlay::Row) {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let budget = kept.min(shown_body_rows(height));
    let mut rows: Vec<(overlay::Row, Join)> = Vec::new();
    // One selectable model row stays visible while the body has room:
    // the stale notice gives up its last row rather than hiding every
    // choice an invisible Enter could still save. At a zero budget
    // nothing draws either way, so the reserve needs no budget check.
    let reserve = usize::from(!choices.models.is_empty());
    let stale_cap = budget.saturating_sub(reserve);
    if let Some(saved) = choices.selected.as_deref().filter(|_| choices.is_stale()) {
        for pair in wrapped_rows(
            &format!("Saved reviewer `{saved}` is not listed; Enter keeps it."),
            inner,
            dim,
        ) {
            if rows.len() >= stale_cap {
                break;
            }
            rows.push(pair);
        }
    }
    let room = budget.saturating_sub(rows.len());
    let total = choices.models.len();
    let cursor = choices.cursor.unwrap_or(0).min(total.saturating_sub(1));
    let start = window_start(cursor, room, total);
    let gutter = format::width(&format!("  {}) ", total.max(1)));
    for (index, model) in choices.models.iter().enumerate().skip(start).take(room) {
        let mut label = format!(
            "  {}) {}",
            index.saturating_add(1),
            format::cut(model, inner.saturating_sub(gutter))
        );
        if choices.is_saved(model) {
            label.push_str(" (saved)");
        }
        rows.push((
            overlay::Row {
                spans: vec![Span::raw(label)],
                right: Vec::new(),
                targets: vec![(0, u16::MAX, TargetId::View(Spot::Row(index)))],
                barred: choices.cursor == Some(index),
            },
            Join::Break,
        ));
    }
    for notice in &choices.notices {
        if rows.len() >= budget {
            break;
        }
        rows.extend(wrapped_rows(notice, inner, dim));
    }
    (
        rows,
        overlay::legend(&[("↑↓", "move"), ("Enter", "save"), ("Esc", "back")]),
    )
}

/// One overlay body row holding `text` in `style`.
fn text_row(text: impl Into<String>, style: Style) -> overlay::Row {
    overlay::Row {
        spans: vec![Span::styled(text.into(), style)],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// One blank overlay body row.
fn blank_row() -> overlay::Row {
    overlay::Row {
        spans: Vec::new(),
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// `text` cut from the left to at most `max` columns, keeping its tail:
/// whole when it fits, else "…" plus the longest tail that fits.
/// `max` 0 gives "".
fn tail(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if format::width(text) <= max {
        return text.to_owned();
    }
    let budget = max.saturating_sub(1);
    let mut width = 0;
    let mut kept: Vec<char> = Vec::new();
    for ch in text.chars().rev() {
        let cell = format::width(&ch.to_string());
        if width + cell > budget {
            break;
        }
        width += cell;
        kept.push(ch);
    }
    kept.reverse();
    let mut out = String::from("…");
    for ch in kept {
        out.push(ch);
    }
    out
}

/// The widest row the provider list shows, measured as drawn: the
/// headings and the choice rows with uncut names, so the step width
/// never depends on what the list clips away.
fn list_widest(targets: &[LoginTarget]) -> usize {
    let mut wide = 0;
    if targets
        .iter()
        .any(|target| target.kind != LoginKind::Secret)
    {
        wide = wide.max(format::width("Providers"));
    }
    if targets
        .iter()
        .any(|target| target.kind == LoginKind::Secret)
    {
        wide = wide.max(format::width("Secrets"));
    }
    let listed: Vec<(&str, &str)> = targets
        .iter()
        .map(|target| {
            (
                target.name.as_str(),
                match target.kind {
                    LoginKind::Browser => "browser",
                    LoginKind::Key => "key",
                    LoginKind::Secret => "",
                },
            )
        })
        .collect();
    if !listed.is_empty() {
        for row in overlay::choices(&listed, 0, u16::MAX) {
            let width: usize = row
                .spans
                .iter()
                .map(|span| format::width(&span.content))
                .sum();
            wide = wide.max(width);
        }
    }
    wide
}

/// The step slab's content width for `name`: the title, the widest list
/// row, every fixed line and every footer. Lines with a variable part
/// (the device-code prompt, the failed line, the URL, the dots, the
/// typed label) never count, so nothing typed or received moves the
/// overlay.
fn step_content_width(name: &str, targets: &[LoginTarget]) -> usize {
    let mut wide = 0;
    wide = wide.max(format::width(name).saturating_add(2));
    wide = wide.max(list_widest(targets));
    for line in [
        format!("Starting the login to {name}…"),
        format!("Open this URL to log in to {name}:"),
        "Waiting for the browser…".to_owned(),
        "Label (--as): default.".to_owned(),
        format!("Key for {name}:"),
        format!("Value for {name}:"),
        format!("✓ Logged in to {name}."),
    ] {
        wide = wide.max(format::width(&line));
    }
    for footer in [
        "Esc back",
        "y copy URL · Esc back",
        "Tab key or label · Enter submit · Esc back",
        "Enter submit · Esc back",
        "↑↓ move · Enter save · Esc back",
    ] {
        wide = wide.max(format::width(footer));
    }
    wide
}

/// One plain row per wrapped line of `text`.
/// `text` wrapped at `inner` columns, each row with its copy join:
/// the row a line of `text` starts begins its own line, one broken at
/// a space joins with that space, and one broken inside a word joins
/// with nothing (`docs/tui.md`, "Selection and copy"). The rows read
/// as [`format::wrap`] reads them, so the draw keeps its rows.
fn wrapped_rows(text: &str, inner: usize, style: Style) -> Vec<(overlay::Row, Join)> {
    format::wrap_joined(text, inner.max(1))
        .into_iter()
        .map(|(row, join)| (text_row(row, style), join))
        .collect()
}

/// The dots row: the last dots that fit, with the cursor's block after
/// them when `cursor` is set. Dots only, never an ellipsis.
fn dots_row(dots: &str, inner: usize, cursor: bool) -> overlay::Row {
    let count = dots.chars().count();
    if cursor {
        let room = inner.saturating_sub(1);
        let shown: String = dots.chars().skip(count.saturating_sub(room)).collect();
        let (_, style) = crate::view::cursor::cursor_cell(None, Role::Surface, Role::Text);
        overlay::Row {
            spans: vec![
                Span::styled(shown, Style::new()),
                Span::styled("█".to_owned(), style),
            ],
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        }
    } else {
        let shown: String = dots.chars().skip(count.saturating_sub(inner)).collect();
        overlay::Row {
            spans: vec![Span::styled(shown, Style::new())],
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        }
    }
}

/// The label row while the label takes typing: the caret's cell reversed,
/// or "█" after the text when the caret is at the end. A label longer
/// than the room is cut so the caret's cell stays in the shown row.
/// The host answer's edit row: the typed answer echoes as typed — a
/// host is not a secret — with the caret, cut to one row.
fn host_edit_row(typed: &Draft, inner: usize) -> overlay::Row {
    if inner == 0 {
        return blank_row();
    }
    let text = typed.expand();
    let caret = typed.position().min(text.chars().count());
    // The caret's cell draws the covered character in the slab's colour
    // on the text colour, or a block in the text colour past the end
    // (`docs/tui.md`, "The input box"). `tail` keeps the row's right
    // end, so a cut caret maps back by how many characters fell off the
    // left; a caret cut away leaves the plain row, as the block fell off
    // with it before.
    if caret >= text.chars().count() {
        let (_, style) = crate::view::cursor::cursor_cell(None, Role::Surface, Role::Text);
        return overlay::Row {
            spans: vec![
                Span::raw(tail(&text, inner.saturating_sub(1))),
                Span::styled("█".to_owned(), style),
            ],
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        };
    }
    let shown = tail(&text, inner);
    let kept: Vec<char> = shown.strip_prefix('…').unwrap_or(&shown).chars().collect();
    let Some(at) = caret.checked_sub(text.chars().count().saturating_sub(kept.len())) else {
        return text_row(shown, Style::new());
    };
    let mut before = String::new();
    let mut covered = String::new();
    let mut after = String::new();
    for (index, ch) in kept.iter().enumerate() {
        if index < at {
            before.push(*ch);
        } else if index == at {
            covered.push(*ch);
        } else {
            after.push(*ch);
        }
    }
    let (symbol, style) =
        crate::view::cursor::cursor_cell(Some(&covered), Role::Surface, Role::Text);
    let first = if shown.starts_with('…') {
        format!("…{before}")
    } else {
        before
    };
    overlay::Row {
        spans: vec![
            Span::raw(first),
            Span::styled(symbol, style),
            Span::raw(after),
        ],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

fn label_edit_row(label: &Draft, inner: usize) -> overlay::Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let prefix = "Label (--as): ";
    if inner <= format::width(prefix) {
        return text_row(format::cut(prefix, inner), dim);
    }
    let room = inner.saturating_sub(format::width(prefix));
    let text = label.expand();
    let chars: Vec<char> = text.chars().collect();
    let caret = label.position().min(chars.len());
    if caret >= chars.len() {
        let shown = tail(&text, room.saturating_sub(1));
        let (_, cursor) = crate::view::cursor::cursor_cell(None, Role::Surface, Role::Text);
        return overlay::Row {
            spans: vec![
                Span::styled(prefix.to_owned(), dim),
                Span::styled(shown, dim),
                Span::styled("█".to_owned(), cursor),
            ],
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        };
    }
    let cell = |ch: char| format::width(&ch.to_string());
    let width_between = |from: usize, to: usize| -> usize {
        chars
            .iter()
            .skip(from)
            .take(to.saturating_sub(from))
            .map(|ch| cell(*ch))
            .sum()
    };
    let mut start = caret;
    let mut end = caret.saturating_add(1);
    for to in end.saturating_add(1)..=chars.len() {
        if width_between(start, to) > room {
            break;
        }
        end = to;
    }
    for from in (0..start).rev() {
        if width_between(from, end) > room {
            break;
        }
        start = from;
    }
    let before: String = chars
        .iter()
        .skip(start)
        .take(caret.saturating_sub(start))
        .collect();
    // The early return covers the caret at the end, so this always
    // finds the caret's cell; the fallback never draws.
    let Some(at_ch) = chars.get(caret).copied() else {
        return blank_row();
    };
    let after: String = chars
        .iter()
        .skip(caret.saturating_add(1))
        .take(end.saturating_sub(caret.saturating_add(1)))
        .collect();
    let (symbol, cursor) =
        crate::view::cursor::cursor_cell(Some(&at_ch.to_string()), Role::Surface, Role::Text);
    overlay::Row {
        spans: vec![
            Span::styled(prefix.to_owned(), dim),
            Span::styled(before, dim),
            Span::styled(symbol, cursor),
            Span::styled(after, dim),
        ],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The failed line wrapped at `inner` as one line: the actual wrapped
/// rows, the head and the closing dot plain, the reason in `error`.
/// Rows past the wrap are the caller's to drop in order. The walk is
/// the shared `overlay::head_tail_rows` one, so the login failure and
/// the import report's left-behind rows wrap alike. The head starts its
/// own line and each wrapped continuation joins with the space the
/// wrap dropped (`docs/tui.md`, "Selection and copy").
fn failed_rows(head: &str, reason: &str, dot: &str, inner: usize) -> Vec<(overlay::Row, Join)> {
    overlay::head_tail_rows(
        head,
        Style::new(),
        reason,
        Style::new().fg(Role::Error.color()),
        dot,
        inner,
    )
    .into_iter()
    .enumerate()
    .map(|(at, row)| {
        (
            row,
            if at == 0 {
                Join::Break
            } else {
                Join::WrapSpace
            },
        )
    })
    .collect()
}

/// The view's rows for `targets`: a heading over each group that has rows.
/// With neither, one note in place of the rows.
fn build(targets: &[LoginTarget]) -> Vec<Item> {
    let mut items = Vec::new();
    if targets
        .iter()
        .any(|target| target.kind != LoginKind::Secret)
    {
        items.push(Item::Heading(Group::Providers));
        for (at, target) in targets.iter().enumerate() {
            if target.kind != LoginKind::Secret {
                items.push(Item::Target(at));
            }
        }
    }
    if targets
        .iter()
        .any(|target| target.kind == LoginKind::Secret)
    {
        // The blank row between the groups, only when both have rows.
        if !items.is_empty() {
            items.push(Item::Gap);
        }
        items.push(Item::Heading(Group::Secrets));
        for (at, target) in targets.iter().enumerate() {
            if target.kind == LoginKind::Secret {
                items.push(Item::Target(at));
            }
        }
    }
    if items.is_empty() {
        items.push(Item::Note);
    }
    items
}

#[cfg(test)]
#[path = "login_view_tests.rs"]
mod tests;
