//! `/login` (`docs/tui.md`, "Logging in"): the installed providers and
//! the secrets installed extensions declare, a hidden field for a key or a
//! value, and the same store steps as `fiber login`. The key is never
//! drawn: the field shows one dot per character, and no frame, `Debug`,
//! notice or log holds it.

use std::fmt;

use contract::Secret;

use crate::configure::{LoginKind, LoginTarget};
use crate::format;
use crate::input::Draft;
use crate::keys::{Edit, Key};
use crate::login_worker::{LoginStep, LoginWorker};
use crate::settings_view::{Act, Ctx};
use crate::swapped::{Frame, Ink, List, Spot, rows_height};

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

/// One row of the view: a heading, one target, or the note in place of
/// every row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    /// A group's heading; Enter on it does nothing.
    Heading(Group),
    /// One provider or secret: its index in the targets.
    Target(usize),
    /// The note shown with no provider and no secret.
    Note,
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
}

/// A browser login in flight.
#[derive(Debug)]
struct Waiting {
    /// The login's ticket: only its events land.
    ticket: u64,
    /// The provider logging in.
    name: String,
    /// The URL shown, once the login opens one.
    url: Option<String>,
    /// The device code shown, when the login shows one.
    code: Option<String>,
    /// The running login, once started.
    worker: Option<LoginWorker>,
}

/// The `/login` view's state.
#[derive(Debug)]
pub(crate) struct Login {
    targets: Vec<LoginTarget>,
    items: Vec<Item>,
    list: List,
    mode: Mode,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
    /// The width the view draws in, in columns.
    width: usize,
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
            list: List::default(),
            mode: Mode::Rows,
            said,
            width: ctx.width,
        };
        let shown = rows_height(&login.frame(ctx.width), ctx.height);
        login
            .list
            .select(login.list.selected(), login.items.len(), shown);
        login
    }

    /// The selected row.
    fn selected(&self) -> Option<Item> {
        self.items.get(self.list.selected()).copied()
    }

    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        self.width = ctx.width;
        if matches!(self.mode, Mode::Panel(_)) {
            self.panel_key(key, ctx);
            Act::Stay
        } else if matches!(self.mode, Mode::Waiting(_)) {
            self.waiting_key(key)
        } else {
            self.rows_key(key, ctx)
        }
    }

    /// A key over the rows: move, open the selected row, or close.
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
        let shown = rows_height(&self.frame(self.width), ctx.height);
        if self.list.key(key, self.items.len(), shown) {
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
    /// nothing, `y` copies the shown URL, and anything else waits on.
    fn waiting_key(&mut self, key: &Key) -> Act {
        if *key == Key::Esc {
            self.mode = Mode::Rows;
            self.said.clear();
            return Act::Stay;
        }
        if *key == Key::Char('y')
            && let Mode::Waiting(waiting) = &self.mode
            && let Some(url) = &waiting.url
        {
            return Act::Copy(url.clone());
        }
        Act::Stay
    }

    /// Waits on the browser login `ticket`: the rows stay, and the view
    /// shows the login's progress below them until it ends.
    pub(crate) fn wait(&mut self, ticket: u64) {
        let name = self
            .selected()
            .and_then(|item| match item {
                Item::Target(at) => self.targets.get(at).map(|target| target.name.clone()),
                Item::Heading(_) | Item::Note => None,
            })
            .unwrap_or_default();
        self.said.clear();
        self.mode = Mode::Waiting(Waiting {
            ticket,
            name,
            url: None,
            code: None,
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
            Mode::Rows | Mode::Panel(_) | Mode::Waiting(_) => drop(worker),
        }
    }

    /// Folds the waiting ticket's progress into the view: an event for
    /// another ticket changes nothing and opens nothing. `Open` returns
    /// the URL to open; the end returns to the rows saying what the login
    /// stored, or why it failed.
    pub(crate) fn step(&mut self, ticket: u64, step: LoginStep) -> Option<String> {
        let Mode::Waiting(waiting) = &mut self.mode else {
            return None;
        };
        if waiting.ticket != ticket {
            return None;
        }
        match step {
            LoginStep::Open(url) => {
                waiting.url = Some(url.clone());
                waiting.code = None;
                Some(url)
            }
            LoginStep::Code { url, code } => {
                waiting.url = Some(url);
                waiting.code = Some(code);
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
            Mode::Rows | Mode::Waiting(_) => return,
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

    /// A click: the ✕ closes, a row over the rows selects it, and a row
    /// while the panel is open or a login waits keeps its mode.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        self.width = ctx.width;
        match spot {
            Spot::Close => Act::Close,
            Spot::Switch { .. } | Spot::Revoke(_) | Spot::Cell(_, _) | Spot::Item(_) => Act::Stay,
            Spot::Row(at) => {
                if matches!(self.mode, Mode::Panel(_) | Mode::Waiting(_)) {
                    return Act::Stay;
                }
                let shown = rows_height(&self.frame(self.width), ctx.height);
                self.list.select(at, self.items.len(), shown);
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
                .map(|item| {
                    let ink = match item {
                        Item::Heading(_) => Ink::Heading,
                        Item::Target(_) => Ink::Plain,
                        Item::Note => Ink::Muted,
                    };
                    vec![(self.line(*item), None, ink)]
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
        let (below, footer) = match &waiting.url {
            None => (
                vec![format!("Starting the login to {name}…")],
                "Esc cancel".to_owned(),
            ),
            Some(url) => match &waiting.code {
                None => {
                    let mut below = vec![format!("Open this URL to log in to {name}:")];
                    below.extend(format::wrap(url, width));
                    (below, "y copy URL · Esc cancel".to_owned())
                }
                Some(code) => {
                    let mut below = vec![format!(
                        "Enter the code {code} at this URL to log in to {name}:"
                    )];
                    below.extend(format::wrap(url, width));
                    (below, "y copy URL · Esc cancel".to_owned())
                }
            },
        };
        Frame {
            title: "Log in".to_owned(),
            rows: self
                .items
                .iter()
                .map(|item| {
                    let ink = match item {
                        Item::Heading(_) => Ink::Heading,
                        Item::Target(_) => Ink::Plain,
                        Item::Note => Ink::Muted,
                    };
                    vec![(self.line(*item), None, ink)]
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
            Item::Note => {
                "No provider is installed, and no installed extension declares a secret.".to_owned()
            }
        }
    }
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
