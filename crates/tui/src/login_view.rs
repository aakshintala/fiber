//! `/login` (`docs/tui.md`, "Logging in"): the installed providers and
//! the secrets installed extensions declare, a hidden field for a key or a
//! value, and the same store steps as `fiber login`. The key is never
//! drawn: the field shows one dot per character, and no frame, `Debug`,
//! notice or log holds it.

use std::fmt;

use contract::Secret;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::configure::{LoginKind, LoginTarget};
use crate::format;
use crate::input::Draft;
use crate::keys::{Edit, Key};
use crate::login_worker::{LoginStep, LoginTicket, LoginWorker};
use crate::mouse::{Target, TargetId};
use crate::settings_view::{Act, Ctx};
use crate::swapped::{Frame, Ink, List, Spot, rows_height};
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
    list: List,
    mode: Mode,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
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
            list: List::default(),
            mode: Mode::Rows,
            said,
        };
        // Focus opens on the first target; with none it stays at 0.
        login.selected = login
            .items
            .iter()
            .position(|item| matches!(item, Item::Target(_)))
            .unwrap_or(0);
        login.sync_list(ctx);
        login
    }

    /// The selected row.
    fn selected(&self) -> Option<Item> {
        self.items.get(self.selected).copied()
    }

    /// `selected`'s row in the legacy frame, which never holds the gap:
    /// past the gap the frame row sits one higher.
    fn frame_row(&self, selected: usize) -> usize {
        let gap = self.items.iter().position(|item| matches!(item, Item::Gap));
        match gap {
            Some(at) if selected > at => selected.saturating_sub(1),
            _ => selected,
        }
    }

    /// Moves the step viewport to `selected`'s frame row, with today's
    /// rows shown: the legacy frame never holds the gap, so the list
    /// keeps exactly the `selected` and `top` today's code gives for
    /// the same focused target.
    fn sync_list(&mut self, ctx: &Ctx<'_>) {
        let frame = self.frame(ctx.width);
        let shown = rows_height(&frame, ctx.height);
        self.list
            .select(self.frame_row(self.selected), frame.rows.len(), shown);
    }

    /// Whether the list is open: exactly in row mode. The panel and
    /// the browser wait keep today's drawing through the legacy frame.
    pub(crate) fn list_open(&self) -> bool {
        matches!(self.mode, Mode::Rows)
    }

    /// Draws the list in the shared overlay frame, centred: dim
    /// headings, one choice row per target with its tag, the gap, then
    /// anything the last action said, at least three body rows before
    /// the window clips them, and the legend footer. A browser wait and
    /// key entry draw their step; any other mode draws nothing yet.
    pub(crate) fn draw(&self, area: Rect, buf: &mut Buffer, targets: &mut Vec<Target>) {
        if matches!(self.mode, Mode::Waiting(_) | Mode::Panel(_)) {
            self.draw_step(area, buf, targets);
            return;
        }
        if !self.list_open() {
            return;
        }
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
        let mut body: Vec<overlay::Row> = Vec::new();
        for item in &self.items {
            match *item {
                Item::Heading(Group::Providers) => body.push(text_row("Providers", dim)),
                Item::Heading(Group::Secrets) => body.push(text_row("Secrets", dim)),
                Item::Target(_) => body.push(choices.next().unwrap_or_else(blank_row)),
                Item::Gap => body.push(blank_row()),
                Item::Note => body.push(text_row(
                    "No provider is installed, and no installed extension declares a secret.",
                    dim,
                )),
            }
        }
        // What the last action said follows the items, one blank row
        // between; with no items it is the whole body. The rows wrap
        // at the preferred width and are never cut sideways.
        if !self.said.is_empty() {
            if !body.is_empty() {
                body.push(blank_row());
            }
            for line in &self.said {
                for row in format::wrap(line, usize::from(inner_w)) {
                    body.push(text_row(row, Style::new()));
                }
            }
        }
        for _ in body.len()..3 {
            body.push(blank_row());
        }
        // The footer carries trailing blanks up to the widest of every
        // body row, not only the shown ones, so the overlay's width
        // never depends on which rows the window shows.
        let wide = body
            .iter()
            .map(|row| {
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
        let start = window_start(self.selected, shown, body.len());
        let end = start.saturating_add(shown).min(body.len());
        let framed = overlay::Overlay {
            title: Some(("Log in".to_owned(), Some(Span::raw("✕")))),
            close: Some(TargetId::View(Spot::Close)),
            body: body
                .into_iter()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect(),
            footer: Some(footer),
            prefer: 96,
        };
        overlay::draw(buf, area, &framed, overlay::Place::Centre, targets);
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
            self.panel_key(key, ctx);
            Act::Stay
        } else if matches!(self.mode, Mode::Waiting(_)) {
            self.waiting_key(key)
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
        // The step viewport below shows no row: both selections stay
        // put.
        if rows_height(&self.frame(ctx.width), ctx.height) == 0 {
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
        if self.selected != before {
            self.sync_list(ctx);
            self.said.clear();
        }
        Act::Stay
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
                Progress::DeviceCode { code, .. } => return Act::Copy(code.clone()),
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
    /// another ticket is dropped, cancelling it at once.
    pub(crate) fn started(&mut self, worker: LoginWorker) {
        match &mut self.mode {
            Mode::Waiting(waiting) if waiting.ticket == worker.ticket() => {
                waiting.worker = Some(worker);
            }
            Mode::Rows | Mode::Panel(_) | Mode::Waiting(_) | Mode::Ended(_) => drop(worker),
        }
    }

    /// Folds the waiting ticket's progress into the view: an event for
    /// another ticket changes nothing and opens nothing. `Open` returns
    /// the URL to open; the end returns to the rows saying what the login
    /// stored, or why it failed.
    pub(crate) fn step(&mut self, ticket: LoginTicket, step: LoginStep) -> Option<String> {
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
                match result {
                    Ok(stored) => {
                        self.said = vec![format!("Stored {}.", stored.path)];
                    }
                    Err(error) => {
                        self.said = vec![error.message];
                    }
                }
                self.mode = Mode::Rows;
                None
            }
        }
    }

    /// A key in the panel: type, move between the fields, store, or close
    /// it storing nothing.
    fn panel_key(&mut self, key: &Key, ctx: &Ctx<'_>) {
        if *key == Key::Esc {
            self.mode = Mode::Rows;
            self.said.clear();
            return;
        }
        if *key == Key::Enter {
            self.submit(ctx);
            return;
        }
        let Mode::Panel(panel) = &mut self.mode else {
            return;
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
            return;
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
    }

    /// Stores the panel's key: the key trimmed, the label trimmed with an
    /// empty one meaning none. Success closes the panel; a refusal clears
    /// the key and keeps the label, with the key focused.
    fn submit(&mut self, ctx: &Ctx<'_>) {
        let (index, label, secret) = match &mut self.mode {
            Mode::Panel(panel) => (panel.target, panel.label.expand(), panel.key.take()),
            Mode::Rows | Mode::Waiting(_) | Mode::Ended(_) => return,
        };
        let Some(target) = self.targets.get(index) else {
            return;
        };
        let (name, kind) = (target.name.clone(), target.kind);
        let label = if kind == LoginKind::Secret {
            None
        } else {
            let trimmed = label.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_owned())
        };
        match ctx.seam.store_key(&name, label.as_deref(), secret) {
            Ok(stored) => {
                self.mode = Mode::Rows;
                let done = if stored.replaced {
                    "Replaced"
                } else {
                    "Stored"
                };
                self.said = vec![format!("{done} {}.", stored.path)];
            }
            Err(error) => {
                self.said = vec![error.message];
                let Mode::Panel(panel) = &mut self.mode else {
                    return;
                };
                panel.focus = Focus::Key;
            }
        }
    }

    /// Handles one editing key: the key field takes a paste whole and
    /// clears on a deleted word, ignoring every other edit; the label
    /// field edits as `/settings`' does.
    pub(crate) fn edit_key(&mut self, edit: &Edit) {
        let Mode::Panel(panel) = &mut self.mode else {
            return;
        };
        match panel.focus {
            Focus::Key => match edit {
                Edit::Paste(text) => panel.key.push_str(text),
                Edit::DeleteWord => panel.key.clear(),
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
                | Edit::LineStart
                | Edit::LineEnd
                | Edit::Delete => panel.label.edit(edit.clone()),
            },
        }
    }

    /// A click: the ✕ closes, a target row over the rows selects it,
    /// and a row while the panel is open or a login waits keeps its
    /// mode. A click on a heading, the gap, the note or past the last
    /// item does nothing.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        match spot {
            Spot::Close => Act::Close,
            Spot::Switch { .. } | Spot::Revoke(_) | Spot::Cell(_, _) | Spot::Item(_) => Act::Stay,
            Spot::Row(at) => {
                if matches!(self.mode, Mode::Panel(_) | Mode::Waiting(_)) {
                    return Act::Stay;
                }
                if !matches!(self.items.get(at), Some(Item::Target(_))) {
                    return Act::Stay;
                }
                self.selected = at;
                self.sync_list(ctx);
                self.said.clear();
                Act::Stay
            }
        }
    }

    /// The frame to draw at `width` columns: the full URL is always
    /// copyable with `y`, and on screen it is wrapped by display width so
    /// no row is wider than the view.
    pub(crate) fn frame(&self, width: usize) -> Frame {
        if let Mode::Waiting(waiting) = &self.mode {
            return self.waiting_frame(waiting, width);
        }
        let mut below = self.said.clone();
        let mut field = None;
        let mut footer = "↑↓ move · Enter log in · Esc close".to_owned();
        if let Mode::Panel(panel) = &self.mode
            && let Some(target) = self.targets.get(panel.target)
        {
            let name = target.name.as_str();
            // A refused store keeps its message in `said`: draw it above
            // the prompts so the failure is visible with the panel open.
            let mut lines = self.said.clone();
            if target.kind == LoginKind::Secret {
                lines.push(format!("Value for {name}:"));
                below = lines;
                let dots = panel.key.dots();
                field = Some((dots.clone(), dots.chars().count()));
                footer = "Enter store · Esc cancel".to_owned();
            } else {
                let label = panel.label.expand();
                match panel.focus {
                    Focus::Key => {
                        let shown = if label.trim().is_empty() {
                            "default".to_owned()
                        } else {
                            label
                        };
                        lines.push(format!("Label (--as): {shown}."));
                        lines.push(format!("Key for {name}:"));
                        below = lines;
                        let dots = panel.key.dots();
                        field = Some((dots.clone(), dots.chars().count()));
                    }
                    Focus::Label => {
                        lines.push(format!("Key for {name}: {}", panel.key.dots()));
                        lines.push("Label (--as), empty for default:".to_owned());
                        below = lines;
                        field = Some((label.clone(), panel.label.position()));
                    }
                }
                footer = "Tab key or label · Enter store · Esc cancel".to_owned();
            }
        }
        Frame {
            title: "Log in".to_owned(),
            rows: self
                .items
                .iter()
                .filter_map(|item| {
                    // The legacy frame never holds the gap, so its row
                    // indices stay gapless and today's snapshots hold.
                    let ink = match item {
                        Item::Heading(_) => Ink::Heading,
                        Item::Target(_) => Ink::Plain,
                        Item::Gap => return None,
                        Item::Note => Ink::Muted,
                    };
                    Some(vec![(self.line(*item), None, ink)])
                })
                .collect(),
            list: self.list,
            below,
            field,
            footer,
        }
    }

    /// The waiting frame: the rows stay, and below them the login's
    /// progress: starting, the URL to open, or the code to enter at it.
    fn waiting_frame(&self, waiting: &Waiting, width: usize) -> Frame {
        let name = waiting.name.as_str();
        let (below, footer) = match &waiting.progress {
            Progress::Starting => (
                vec![format!("Starting the login to {name}…")],
                "Esc cancel".to_owned(),
            ),
            Progress::Url(url) => {
                let mut below = vec![format!("Open this URL to log in to {name}:")];
                below.extend(format::wrap(url, width));
                (below, "y copy URL · Esc cancel".to_owned())
            }
            Progress::DeviceCode { url, code } => {
                let mut below = vec![format!(
                    "Enter the code {code} at this URL to log in to {name}:"
                )];
                below.extend(format::wrap(url, width));
                (below, "y copy URL · Esc cancel".to_owned())
            }
        };
        Frame {
            title: "Log in".to_owned(),
            rows: self
                .items
                .iter()
                .filter_map(|item| {
                    // The legacy frame never holds the gap, so its row
                    // indices stay gapless and today's snapshots hold.
                    let ink = match item {
                        Item::Heading(_) => Ink::Heading,
                        Item::Target(_) => Ink::Plain,
                        Item::Gap => return None,
                        Item::Note => Ink::Muted,
                    };
                    Some(vec![(self.line(*item), None, ink)])
                })
                .collect(),
            list: self.list,
            below,
            field: None,
            footer,
        }
    }

    /// One row's text.
    fn line(&self, item: Item) -> String {
        match item {
            Item::Heading(Group::Providers) => "Providers".to_owned(),
            Item::Heading(Group::Secrets) => "Secrets".to_owned(),
            Item::Target(at) => match self.targets.get(at) {
                Some(target) => match target.kind {
                    LoginKind::Key => format!("  {}  key", target.name),
                    LoginKind::Browser => format!("  {}  browser", target.name),
                    LoginKind::Secret => format!("  {}", target.name),
                },
                None => String::new(),
            },
            Item::Gap => String::new(),
            Item::Note => {
                "No provider is installed, and no installed extension declares a secret.".to_owned()
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

    /// Draws the open step over `area`: one overlay titled with the
    /// provider's name, the body exactly the list's row count and the
    /// footer padded to the shared width, so the overlay never jumps
    /// between steps. The body is built top to bottom and cut at the
    /// row count in order; over a short area it is clipped to the body
    /// budget the list shows there.
    fn draw_step(&self, area: Rect, buf: &mut Buffer, targets_out: &mut Vec<Target>) {
        let Some(name) = self.step_name() else {
            return;
        };
        let content = step_content_width(&name, &self.targets);
        let content_u16 = u16::try_from(content).unwrap_or(u16::MAX);
        let slab_w = overlay::width(content_u16, 96, area.width);
        let inner = usize::from(overlay::inner(slab_w));
        let dim = Style::new().add_modifier(Modifier::DIM);
        let (content_rows, legend): (Vec<overlay::Row>, overlay::Row) = match &self.mode {
            Mode::Rows => return,
            Mode::Waiting(waiting) => match &waiting.progress {
                Progress::Starting => (
                    wrapped_rows(&format!("Starting the login to {}…", name), inner, Style::new()),
                    overlay::legend(&[("Esc", "back")]),
                ),
                Progress::Url(url) => {
                    let mut rows = wrapped_rows(
                        &format!("Open this URL to log in to {name}:"),
                        inner,
                        Style::new(),
                    );
                    rows.push(text_row(
                        tail(url, inner),
                        Style::new().fg(Role::Accent.color()),
                    ));
                    rows.extend(wrapped_rows("Waiting for the browser…", inner, dim));
                    (
                        rows,
                        overlay::legend(&[("y", "copy URL"), ("Esc", "back")]),
                    )
                }
                Progress::DeviceCode { url, code } => {
                    let mut rows = wrapped_rows(
                        &format!("Enter the code {code} at this URL to log in to {name}:"),
                        inner,
                        Style::new(),
                    );
                    rows.push(text_row(
                        tail(url, inner),
                        Style::new().fg(Role::Accent.color()),
                    ));
                    rows.extend(wrapped_rows("Waiting for the browser…", inner, dim));
                    (
                        rows,
                        overlay::legend(&[("y", "copy code"), ("Esc", "back")]),
                    )
                }
            },
            Mode::Panel(panel) => {
                let Some(target) = self.targets.get(panel.target) else {
                    return;
                };
                if target.kind == LoginKind::Secret {
                    let mut rows = wrapped_rows(&format!("Value for {}:", name), inner, Style::new());
                    rows.push(dots_row(&panel.key.dots(), inner, true));
                    (rows, overlay::legend(&[("Enter", "submit"), ("Esc", "back")]))
                } else if panel.focus == Focus::Label {
                    let mut rows = vec![label_edit_row(&panel.label, inner)];
                    rows.extend(wrapped_rows(&format!("Key for {name}:"), inner, Style::new()));
                    rows.push(dots_row(&panel.key.dots(), inner, false));
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
                    let mut rows = wrapped_rows(&format!("Label (--as): {shown}."), inner, dim);
                    rows.extend(wrapped_rows(&format!("Key for {name}:"), inner, Style::new()));
                    rows.push(dots_row(&panel.key.dots(), inner, true));
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
                    (failed_rows(&head, reason, dot, inner), overlay::legend(&[("Esc", "back")]))
                }
            },
        };
        let kept = self.body_count();
        let mut body = content_rows;
        body.truncate(kept);
        while body.len() < kept {
            body.push(blank_row());
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
        let framed = overlay::Overlay {
            title: Some((name, Some(Span::raw("✕")))),
            close: Some(TargetId::View(Spot::Close)),
            body,
            footer: Some(footer),
            prefer: 96,
        };
        overlay::draw(buf, area, &framed, overlay::Place::Centre, targets_out);
    }
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
    if targets.iter().any(|target| target.kind != LoginKind::Secret) {
        wide = wide.max(format::width("Providers"));
    }
    if targets.iter().any(|target| target.kind == LoginKind::Secret) {
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
        "y copy code · Esc back",
        "Tab key or label · Enter submit · Esc back",
        "Enter submit · Esc back",
    ] {
        wide = wide.max(format::width(footer));
    }
    wide
}

/// One plain row per wrapped line of `text`.
fn wrapped_rows(text: &str, inner: usize, style: Style) -> Vec<overlay::Row> {
    format::wrap(text, inner.max(1))
        .into_iter()
        .map(|row| text_row(row, style))
        .collect()
}

/// The dots row: the last dots that fit, with the block cursor
/// after them when `cursor` is set. Dots only, never an ellipsis.
fn dots_row(dots: &str, inner: usize, cursor: bool) -> overlay::Row {
    let dim = Style::new().add_modifier(Modifier::DIM);
    let count = dots.chars().count();
    if cursor {
        let room = inner.saturating_sub(1);
        let shown: String = dots.chars().skip(count.saturating_sub(room)).collect();
        overlay::Row {
            spans: vec![
                Span::styled(shown, Style::new()),
                Span::styled("█".to_owned(), dim),
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
        return overlay::Row {
            spans: vec![
                Span::styled(prefix.to_owned(), dim),
                Span::styled(shown, dim),
                Span::styled("█".to_owned(), dim),
            ],
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        };
    }
    let cell = |ch: char| format::width(&ch.to_string());
    let mut start = caret;
    let mut end = caret.saturating_add(1);
    while end < chars.len() {
        let width: usize = chars[start..end.saturating_add(1)]
            .iter()
            .map(|ch| cell(*ch))
            .sum();
        if width > room {
            break;
        }
        end = end.saturating_add(1);
    }
    while start > 0 {
        let width: usize = chars[start.saturating_sub(1)..end]
            .iter()
            .map(|ch| cell(*ch))
            .sum();
        if width > room {
            break;
        }
        start = start.saturating_sub(1);
    }
    let before: String = chars[start..caret].iter().collect();
    let at = chars[caret].to_string();
    let after: String = chars[caret.saturating_add(1)..end].iter().collect();
    overlay::Row {
        spans: vec![
            Span::styled(prefix.to_owned(), dim),
            Span::styled(before, dim),
            Span::styled(
                at,
                Style::new().add_modifier(Modifier::DIM | Modifier::REVERSED),
            ),
            Span::styled(after, dim),
        ],
        right: Vec::new(),
        targets: Vec::new(),
        barred: false,
    }
}

/// The failed line wrapped at `inner` as one line: the head and the
/// closing dot plain, the reason in `error`. Rows past the wrap are the
/// caller’s to drop in order.
fn failed_rows(head: &str, reason: &str, dot: &str, inner: usize) -> Vec<overlay::Row> {
    let combined = format!("{head}{reason}{dot}");
    let rows = format::wrap(&combined, inner.max(1));
    let combined_chars: Vec<char> = combined.chars().collect();
    let head_len = head.chars().count();
    let reason_len = reason.chars().count();
    let error = Style::new().fg(Role::Error.color());
    let mut pos = 0;
    let mut out = Vec::new();
    for row in rows {
        let row_chars: Vec<char> = row.chars().collect();
        let mut start = pos;
        if combined_chars[start..]
            .iter()
            .take(row_chars.len())
            .collect::<String>()
            != row
        {
            // The wrap dropped the single space or line break the row
            // broke at; skip it so the kept rows stay a prefix.
            if matches!(combined_chars.get(start), Some(' ' | '\n')) {
                start = start.saturating_add(1);
            }
        }
        let end = start.saturating_add(row_chars.len()).min(combined_chars.len());
        let mut spans = Vec::new();
        let mut at = start;
        while at < end {
            let styled_error = at >= head_len && at < head_len.saturating_add(reason_len);
            let mut run = String::new();
            while at < end
                && (at >= head_len && at < head_len.saturating_add(reason_len))
                    == styled_error
            {
                run.push(combined_chars[at]);
                at = at.saturating_add(1);
            }
            spans.push(Span::styled(
                run,
                if styled_error { error } else { Style::new() },
            ));
        }
        pos = end;
        out.push(overlay::Row {
            spans,
            right: Vec::new(),
            targets: Vec::new(),
            barred: false,
        });
    }
    out
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
