//! `/settings` (`docs/tui.md`, "Swapped views", `/settings`): every
//! configuration key with no `*` in its path on the tab its schema entry
//! names, each edited by the control its schema type gives it, saved at
//! once through the same path as `fiber config set` to the layer the
//! value already comes from, and when a written key applies
//! (`docs/configuration.md`, "When Fiber reads configuration").

use std::path::{Path, PathBuf};

use contract::settings::{SettingControl, SettingsMetadata, SettingsTab};
use ratatui::style::{Modifier, Style};
use serde_json::Value;

use crate::ThemeSetting;
use crate::configure::{Configure, Layer, SettingRow, Shown, WriteScope};
use crate::input::Draft;
use crate::keys::{Edit, Key};
use crate::swapped::{Chrome, Frame, GUTTER, Ink, Inline, List, Span, Spot, rows_height, tinted};
use crate::text;

/// The themes offered before the theme files: following the terminal,
/// then the built-ins.
pub(crate) const BUILT_IN_THEMES: [&str; 3] = ["auto", "dark", "light"];

/// A screen the `/settings` view lists as a row of its own, because the
/// keys it stands for have a `*` in their path or are not keys at all
/// (`docs/tui.md`, "Swapped views", `/settings`, "Rows").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenRow {
    /// The row's label, its description and the tab it ends.
    pub metadata: SettingsMetadata,
    /// The slash command that opens the screen.
    pub command: &'static str,
    /// The schema entries this row stands for, spelled as in the schema
    /// (`tools.*.deferred`, never the family `tools.*`). Such an entry has
    /// no tab of its own, so the view draws it once, here.
    pub covers: &'static [&'static str],
}

const fn screen(
    tab: SettingsTab,
    label: &'static str,
    description: &'static str,
    command: &'static str,
    covers: &'static [&'static str],
) -> ScreenRow {
    ScreenRow {
        metadata: SettingsMetadata {
            tab: Some(tab),
            label,
            description,
        },
        command,
        covers,
    }
}

/// The screens, in the order they end their tab: Key bindings ends
/// Appearance, Credentials ends Models, Rules then Tools end Permissions,
/// and Skills then MCP servers end Sessions.
pub const SCREEN_ROWS: [ScreenRow; 6] = [
    screen(
        SettingsTab::Appearance,
        "Key bindings",
        "Rebind the terminal's keys, one action at a time.",
        "/keys",
        &["keys.*"],
    ),
    screen(
        SettingsTab::Models,
        "Credentials",
        "Choose which stored credential each provider's sessions use.",
        "/credential",
        &["providers.*.credential"],
    ),
    screen(
        SettingsTab::Permissions,
        "Rules",
        "Review and revoke the standing rules that allow a call without asking.",
        "/rules",
        &[],
    ),
    screen(
        SettingsTab::Permissions,
        "Tools",
        "Switch an MCP server's or extension's tools on or off, and see which are deferred.",
        "/tools",
        &[
            "tools.*.max_result_bytes",
            "tools.*.deferred",
            "extensions.*.tools.enabled",
            "extensions.*.tools.disabled",
        ],
    ),
    screen(
        SettingsTab::Sessions,
        "Skills",
        "Switch skills on or off.",
        "/skills",
        &["skills.disabled"],
    ),
    screen(
        SettingsTab::Sessions,
        "MCP servers",
        "See each MCP server's state and tool count, and reconnect or log in.",
        "/mcp",
        &[
            "mcp.servers.*.command",
            "mcp.servers.*.args",
            "mcp.servers.*.env",
            "mcp.servers.*.url",
            "mcp.servers.*.required",
            "mcp.servers.*.startup_timeout_ms",
            "mcp.servers.*.timeout_ms",
            "mcp.servers.*.declare_in_full",
            "mcp.servers.*.tools.enabled",
            "mcp.servers.*.tools.disabled",
            "mcp.servers.*.tools.*.hints",
        ],
    ),
];

/// The six screens the screen rows open, in [`SCREEN_ROWS`] order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Screen {
    /// `/keys`.
    Keys,
    /// `/credential`.
    Credentials,
    /// `/rules`.
    Rules,
    /// `/tools`.
    Tools,
    /// `/skills`.
    Skills,
    /// `/mcp`.
    Mcp,
}

impl Screen {
    /// Every screen, in [`SCREEN_ROWS`] order.
    pub(crate) const ALL: [Self; 6] = [
        Self::Keys,
        Self::Credentials,
        Self::Rules,
        Self::Tools,
        Self::Skills,
        Self::Mcp,
    ];

    /// The screen's row in [`SCREEN_ROWS`].
    pub(crate) fn row(self) -> Option<&'static ScreenRow> {
        Self::ALL
            .iter()
            .position(|screen| *screen == self)
            .and_then(|at| SCREEN_ROWS.get(at))
    }
}

/// What the view asks the app to do for it, beyond its own state: the app
/// takes one after the view handles a key, an edit or a click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Request {
    /// Open this screen.
    Screen(Screen),
    /// Open the model picker; its choice saves to the key and layer the
    /// view kept in `awaiting`.
    Model,
}

/// The key the theme choice edits.
pub(crate) const THEME: &str = "tui.theme";

/// The columns between a row's label and its value.
const GAP: usize = 2;

/// The widest label column; a longer label is cut.
const LABEL_MAX: usize = 30;

/// The body rows a frame keeps for the controls before it gives up the
/// description and detail lines under the list.
const MIN_BODY: usize = 3;

/// The height from which the view spaces its rows one blank line apart
/// and keeps blank lines in its top rows.
const TALL: usize = 18;

/// The layers a write to `scope` may choose, in Tab's order, global first.
pub(crate) fn layers(scope: WriteScope) -> Vec<Layer> {
    match scope {
        WriteScope::Any { repo: true } => vec![Layer::Global, Layer::Project, Layer::Repository],
        WriteScope::Any { repo: false } | WriteScope::PersonFiles => {
            vec![Layer::Global, Layer::Project]
        }
        WriteScope::GlobalOnly => vec![Layer::Global],
        WriteScope::RepoOnly => vec![Layer::Repository],
    }
}

/// A value as `fiber config set` takes it: a JSON string bare, anything
/// else as its JSON.
pub(crate) fn as_typed(value: &str) -> String {
    match serde_json::from_str::<Value>(value) {
        Ok(Value::String(text)) => text,
        Ok(_) | Err(_) => value.to_owned(),
    }
}

/// A string as the JSON text `fiber config set` reads back as that string.
fn quoted(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

/// A layer's name, as the view says it.
fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Global => "global",
        Layer::Project => "project",
        Layer::Repository => "repository",
    }
}

/// The layer a row's `layer` word names, if it is one of the files.
fn layer_named(word: &str) -> Option<Layer> {
    [Layer::Global, Layer::Project, Layer::Repository]
        .into_iter()
        .find(|layer| layer_name(*layer) == word)
}

/// What a key or click asks of the app.
#[derive(Debug)]
pub(crate) enum Act {
    /// Nothing beyond the view's own change.
    Stay,
    /// Close the view.
    Close,
    /// Apply this theme now.
    Theme(ThemeSetting),
    /// Open this file in the editor.
    Open(PathBuf),
    /// Open a client-owned file; a default row uses the client's global file.
    OpenLocal(Option<PathBuf>),
    /// Start the browser login for this provider.
    Login(String),
    /// Reconnect this MCP server: the hub restarts the one slot.
    McpReconnect(String),
    /// Log in this MCP server: the hub runs the OAuth device flow.
    McpLogin(String),
    /// Read the reviewer choices for this stored provider in the
    /// background, after its key was stored.
    Reviewer(String),
    /// Copy this URL through OSC 52 and the system clipboard.
    Copy(String),
}

/// What a view's call needs from the app: the seam, the workspace the
/// view is about, the view's height in rows and its width in columns,
/// and the last call's prompt size on the session on screen.
pub(crate) struct Ctx<'a> {
    pub(crate) seam: &'a dyn Configure,
    pub(crate) workspace: &'a Path,
    pub(crate) height: usize,
    pub(crate) width: usize,
    pub(crate) usage: Option<u64>,
}

/// When a written key takes effect (`docs/configuration.md`, "When Fiber
/// reads configuration").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Applies {
    /// At once: the terminal applies it.
    Now,
    /// When the terminal starts again.
    TerminalStart,
    /// At this client's next connection.
    NextConnection,
    /// When the hub starts again.
    HubStart,
    /// When each Fiber process starts again.
    ProcessStart,
    /// Route settings apply only to sessions started after the write.
    NewSessions,
    /// At the next turn start.
    NextTurn,
    /// On `/reload`, which rebuilds the prompt cache.
    Reload,
}

/// When a write to `key` applies.
pub(crate) fn applies(key: &str) -> Applies {
    if key == THEME {
        Applies::Now
    } else if key.starts_with("tui.") {
        Applies::TerminalStart
    } else if key == "hub.default" || key.starts_with("hubs.") {
        Applies::NextConnection
    } else if key.starts_with("hub.") {
        Applies::HubStart
    } else if key == "diagnostics.level" {
        Applies::ProcessStart
    } else if key == "model"
        || key == "thinking"
        || matches!(
            key.split('.').collect::<Vec<_>>().as_slice(),
            ["models", _, .., "thinking"]
                | ["providers", _, "credential"]
                | ["providers", _, "credentials", ..]
        )
    {
        Applies::NewSessions
    } else if key == "skills.disabled" {
        Applies::NextTurn
    } else {
        Applies::Reload
    }
}

/// What a view says a change to `key` costs: that it needs a reload, and
/// the reload's cache rebuild in tokens when known (`docs/tui.md`,
/// "Swapped views"). `usage` is the last call's prompt size on the
/// session on screen.
fn reload_text(key: &str, usage: Option<u64>) -> String {
    match usage {
        Some(tokens) => format!(
            "changing {key} needs a reload ({})",
            crate::model_picker::rebuild_cost(tokens, None)
        ),
        None => format!("changing {key} needs a reload"),
    }
}

/// What the view says about when `key` applies. `usage` is the last
/// call's prompt size on the session on screen, which a reload's cache
/// rebuild costs; without it the view names no number.
pub(crate) fn applies_text(key: &str, usage: Option<u64>) -> String {
    match applies(key) {
        Applies::Now => "Applies now.".to_owned(),
        Applies::TerminalStart => "Applies when the terminal starts again.".to_owned(),
        Applies::NextConnection => "Applies at this client's next connection.".to_owned(),
        Applies::HubStart => "Applies when the hub starts again.".to_owned(),
        Applies::ProcessStart => "Applies when each Fiber process starts again.".to_owned(),
        Applies::NewSessions => "Applies to new sessions.".to_owned(),
        Applies::NextTurn => "Reaches the model at the next turn start.".to_owned(),
        Applies::Reload => reload_text(key, usage),
    }
}

/// What a row on a tab is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    /// A configuration key: its index in the rows read.
    Key(usize),
    /// A screen row.
    Screen(Screen),
}

/// The focused row, by identity: a re-read or a tab change keeps it where
/// it still exists.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Focus {
    Key(String),
    Screen(Screen),
}

/// One display line of the body.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Line {
    Item(Item),
    Blank,
    Note(&'static str),
}

/// The edit field of a number or free text.
struct Field {
    key: String,
    layer: Layer,
    /// Whether the text goes to `set` as typed (a number) or as a JSON
    /// string (free text).
    number: bool,
    draft: Draft,
}

/// The list sub-page's field: the entry it edits, or none for "+ Add".
struct PageField {
    at: Option<usize>,
    draft: Draft,
}

/// The list sub-page of a string list or `repository_extensions`
/// (`docs/tui.md`, "Swapped views", `/settings`): the committed entries,
/// the entry in focus and the open field. Every change saves the whole
/// list before the entries change.
struct Page {
    key: String,
    label: String,
    layer: Layer,
    /// Whether the entries are `{path, required}` objects.
    objects: bool,
    entries: Vec<Value>,
    list: List,
    field: Option<PageField>,
}

/// What the view shows below its header.
#[derive(Debug)]
enum Mode {
    /// The tab's rows.
    Rows,
    /// The edit field in the focused row's value place.
    Field(Field),
    /// The theme choices `/theme` opens on: their names, the current one,
    /// the selection.
    Choices {
        names: Vec<String>,
        current: Option<String>,
        list: List,
    },
}

/// The field prints its draft redacted: it may hold a typed secret
/// (`docs/code-quality.md`, "Errors").
impl std::fmt::Debug for Field {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Field")
            .field("key", &self.key)
            .field("layer", &self.layer)
            .field("number", &self.number)
            .field("draft", &"redacted")
            .finish()
    }
}

impl std::fmt::Debug for PageField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageField")
            .field("at", &self.at)
            .field("draft", &"redacted")
            .finish()
    }
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("key", &self.key)
            .field("layer", &self.layer)
            .field("entries", &self.entries.len())
            .field("field", &self.field)
            .finish()
    }
}

/// Everything the body of one frame is made of, worked out once so the
/// draw, the keys and the clicks agree.
struct Layout {
    lines: Vec<Line>,
    /// The display line of each item.
    takes: Vec<usize>,
    top: Vec<Vec<Span>>,
    below: Vec<String>,
    /// The body rows the frame keeps.
    body: usize,
    /// The display line of the focused item.
    focus: Option<usize>,
    label_col: usize,
}

/// The `/settings` view's state.
#[derive(Debug)]
pub(crate) struct Settings {
    rows: Vec<SettingRow>,
    /// The run's `-c` overrides, in order: every re-read uses them.
    overrides: Vec<String>,
    tab: SettingsTab,
    focus: Option<Focus>,
    list: List,
    mode: Mode,
    page: Option<Page>,
    /// The entry `tui.theme`, resolved once when the view opened: a
    /// cancel of the theme choices restores this, whatever a preview
    /// queued since.
    entry_theme: ThemeSetting,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
    /// What the app is asked to do next.
    request: Option<Request>,
    /// The model picker is open above the view for this key and layer.
    awaiting: Option<(String, Layer)>,
}

impl Settings {
    /// The view over `ctx`'s workspace, its rows read with the run's
    /// `overrides`, on the first tab.
    pub(crate) fn open(ctx: &Ctx<'_>, overrides: Vec<String>) -> Self {
        let mut settings = Self {
            rows: Vec::new(),
            overrides,
            tab: SettingsTab::Appearance,
            focus: None,
            list: List::gutter(),
            mode: Mode::Rows,
            page: None,
            entry_theme: ThemeSetting::Follow,
            said: Vec::new(),
            request: None,
            awaiting: None,
        };
        settings.reread(ctx);
        settings.entry_theme = settings.entry_setting(ctx);
        settings
    }

    /// The view opened directly in the theme choices, as `/theme` opens
    /// it: the `tui.theme` row focused with its choices open. With no
    /// theme row the view opens on its rows, as `open` does.
    pub(crate) fn open_theme(ctx: &Ctx<'_>, overrides: Vec<String>) -> Self {
        let mut settings = Self::open(ctx, overrides);
        if settings.rows.iter().any(|row| row.key == THEME) {
            settings.focus = Some(Focus::Key(THEME.to_owned()));
            settings.tab = settings
                .rows
                .iter()
                .find(|row| row.key == THEME)
                .and_then(|row| row.presentation.tab)
                .unwrap_or(SettingsTab::Appearance);
            settings.place(ctx);
            settings.open_choices(ctx);
        }
        settings
    }

    /// The entry `tui.theme`, resolved against `ctx`'s seam: the theme
    /// the choices restore on a cancel.
    fn entry_setting(&self, ctx: &Ctx<'_>) -> ThemeSetting {
        ctx.seam.theme(ctx.workspace, &self.theme_name())
    }

    /// The effective `tui.theme` name: `auto` when none is set.
    fn theme_name(&self) -> String {
        self.rows
            .iter()
            .find(|row| row.key == THEME)
            .and_then(|row| match &row.value {
                Shown::Value(value) => Some(as_typed(value)),
                Shown::Unset | Shown::Union { .. } | Shown::Redacted(_) => None,
            })
            .unwrap_or_else(|| "auto".to_owned())
    }

    /// The captured entry theme, while the theme choices are open: what
    /// a cancel restores. `None` on the rows and in the field.
    pub(crate) fn choices_entry(&self) -> Option<ThemeSetting> {
        match &self.mode {
            Mode::Choices { .. } => Some(self.entry_theme.clone()),
            Mode::Rows | Mode::Field(_) => None,
        }
    }

    /// Reads the rows again with the run's overrides, keeping the focused
    /// key where it is; a failed read says why.
    pub(crate) fn reread(&mut self, ctx: &Ctx<'_>) {
        match ctx.seam.settings(ctx.workspace, &self.overrides) {
            Ok(rows) => self.rows = rows,
            Err(error) => {
                self.rows = Vec::new();
                self.said = vec![error.message];
            }
        }
        self.place(ctx);
    }

    /// The request the app owes the view, once.
    pub(crate) fn take_request(&mut self) -> Option<Request> {
        self.request.take()
    }

    /// Whether the model picker is open above the view for one of its
    /// keys.
    pub(crate) fn awaiting_model(&self) -> bool {
        self.awaiting.is_some()
    }

    /// The model picker closed without a choice: nothing was written.
    pub(crate) fn model_cancelled(&mut self) {
        self.awaiting = None;
    }

    /// Saves `reference`, byte for byte, to the key and layer the picker
    /// was opened for. A saved choice ends the wait; a refused one keeps it,
    /// so the picker can choose again, and says why. Whether it saved.
    pub(crate) fn choose_model(&mut self, reference: &str, ctx: &Ctx<'_>) -> bool {
        let Some((key, layer)) = self.awaiting.clone() else {
            return false;
        };
        if self.save(ctx, layer, &key, &quoted(reference)) {
            self.awaiting = None;
            true
        } else {
            false
        }
    }

    /// The last thing said, for a notice when the view is covered.
    pub(crate) fn last_said(&self) -> Option<&str> {
        self.said.last().map(String::as_str)
    }

    // The tab's rows.

    /// The rows the open tab holds: its keys in the order read, then its
    /// screen rows.
    fn items(&self) -> Vec<Item> {
        let mut items: Vec<Item> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.presentation.tab == Some(self.tab))
            .map(|(at, _)| Item::Key(at))
            .collect();
        items.extend(
            Screen::ALL
                .into_iter()
                .filter(|screen| {
                    screen
                        .row()
                        .is_some_and(|row| row.metadata.tab == Some(self.tab))
                })
                .map(Item::Screen),
        );
        items
    }

    fn row(&self, at: usize) -> Option<&SettingRow> {
        self.rows.get(at)
    }

    fn focus_of(&self, item: Item) -> Option<Focus> {
        match item {
            Item::Key(at) => self.row(at).map(|row| Focus::Key(row.key.clone())),
            Item::Screen(screen) => Some(Focus::Screen(screen)),
        }
    }

    /// The focused item on the open tab; the first when the focus is on
    /// no row of it.
    fn focused(&self, items: &[Item]) -> Option<Item> {
        items
            .iter()
            .copied()
            .find(|item| self.focus_of(*item) == self.focus)
            .or_else(|| items.first().copied())
    }

    /// The focused key's row, outside the screen rows.
    fn selected(&self) -> Option<&SettingRow> {
        match self.focused(&self.items())? {
            Item::Key(at) => self.row(at),
            Item::Screen(_) => None,
        }
    }

    /// Puts the focus on a row of the open tab and the list on it.
    fn place(&mut self, ctx: &Ctx<'_>) {
        let items = self.items();
        self.focus = self.focused(&items).and_then(|item| self.focus_of(item));
        let layout = self.layout(ctx.width, ctx.height, ctx.usage);
        self.list = self.list_over(&layout);
    }

    /// The list scrolled so the focused row shows in `layout`'s body.
    fn list_over(&self, layout: &Layout) -> List {
        let mut list = self.list;
        if let Some(focus) = layout.focus {
            list.select(focus, layout.lines.len(), layout.body);
        }
        list
    }

    /// Moves the focus to the item on display line `line`, if it is one.
    fn focus_line(&mut self, line: usize, ctx: &Ctx<'_>) {
        let layout = self.layout(ctx.width, ctx.height, ctx.usage);
        if let Some(Line::Item(item)) = layout.lines.get(line) {
            self.focus = self.focus_of(*item);
        }
        let layout = self.layout(ctx.width, ctx.height, ctx.usage);
        self.list = self.list_over(&layout);
    }

    /// The layout of the open tab at `width` by `height`.
    fn layout(&self, width: usize, height: usize, usage: Option<u64>) -> Layout {
        let items = self.items();
        let focused = self.focused(&items);
        let compact = height < TALL;
        // The fixed rows: what saving does, then the tab bar.
        let mut top = vec![vec![(
            "Changes save automatically".to_owned(),
            Ink::Muted.style(),
        )]];
        if !compact {
            top.push(Vec::new());
        }
        top.push(self.tab_bar(width));
        if !compact {
            top.push(Vec::new());
        }
        let mut details = focused
            .map(|item| self.details(item, usage))
            .unwrap_or_default();
        details.extend(self.said.iter().cloned());
        // The controls keep their room: a short frame drops the
        // description, then the detail line, and keeps what was said.
        let room = height
            .saturating_sub(2 + top.len() + MIN_BODY)
            .min(details.len());
        let below = details.split_off(details.len().saturating_sub(room));
        let body = height.saturating_sub(2 + top.len() + below.len());
        let spaced = !items.is_empty() && items.len().saturating_mul(2).saturating_sub(1) <= body;
        let mut lines = Vec::new();
        let mut takes = Vec::new();
        for (at, item) in items.iter().enumerate() {
            if at > 0 && spaced {
                lines.push(Line::Blank);
            }
            takes.push(lines.len());
            lines.push(Line::Item(*item));
        }
        if lines.is_empty() {
            lines.push(Line::Note("Nothing is set on this tab."));
        }
        let focus = focused.and_then(|item| items.iter().position(|it| *it == item));
        let label_col = items
            .iter()
            .map(|item| text::width(&self.label_of(*item)))
            .max()
            .unwrap_or(0)
            .min(LABEL_MAX);
        Layout {
            focus: focus.and_then(|at| takes.get(at).copied()),
            lines,
            takes,
            top,
            below,
            body,
            label_col,
        }
    }

    /// The tab bar: the open tab bold on the `surface` tint, the rest
    /// plain. A bar wider than the frame drops tabs from the left, never
    /// the open one.
    fn tab_bar(&self, width: usize) -> Vec<Span> {
        let open = Style::new().add_modifier(Modifier::BOLD);
        let spans: Vec<(SettingsTab, Span)> = SettingsTab::ALL
            .into_iter()
            .map(|tab| {
                let name = format!(" {} ", tab_name(tab));
                let style = if tab == self.tab {
                    tinted(open)
                } else {
                    Style::new()
                };
                (tab, (name, style))
            })
            .collect();
        let fits = |from: usize| {
            let used: usize = spans
                .iter()
                .skip(from)
                .map(|(_, (name, _))| text::width(name) + 1)
                .sum();
            used <= width.saturating_add(1)
        };
        // The first tab from which the rest fit, or the open one.
        let start = spans
            .iter()
            .enumerate()
            .find(|(from, (tab, _))| *tab == self.tab || fits(*from))
            .map_or(0, |(from, _)| from);
        let mut bar = Vec::new();
        for (at, (_, span)) in spans.into_iter().enumerate().skip(start) {
            if at > start {
                bar.push((" ".to_owned(), Style::new()));
            }
            bar.push(span);
        }
        bar
    }

    /// The label a row shows.
    fn label_of(&self, item: Item) -> String {
        match item {
            Item::Key(at) => self
                .row(at)
                .map_or_else(String::new, |row| row.presentation.label.clone()),
            Item::Screen(screen) => screen
                .row()
                .map_or_else(String::new, |row| row.metadata.label.to_owned()),
        }
    }

    /// The lines under the list for the focused row: its description, then
    /// its key path, where it is set and when a change applies.
    fn details(&self, item: Item, usage: Option<u64>) -> Vec<String> {
        match item {
            Item::Screen(screen) => screen.row().map_or_else(Vec::new, |row| {
                vec![
                    row.metadata.description.to_owned(),
                    format!("{} · opens its own screen", row.command),
                ]
            }),
            Item::Key(at) => self.row(at).map_or_else(Vec::new, |row| {
                vec![
                    row.presentation.description.clone(),
                    format!(
                        "{} · {} · {}",
                        row.key,
                        origin(row),
                        applies_text(&row.key, usage)
                    ),
                ]
            }),
        }
    }

    /// The body rows that fit at `ctx`'s size.
    fn shown(&self, ctx: &Ctx<'_>) -> usize {
        rows_height(&self.frame(ctx.width, ctx.height, ctx.usage), ctx.height)
    }
}

/// A tab's name.
fn tab_name(tab: SettingsTab) -> &'static str {
    match tab {
        SettingsTab::Appearance => "Appearance",
        SettingsTab::Notifications => "Notifications",
        SettingsTab::Models => "Models",
        SettingsTab::Permissions => "Permissions",
        SettingsTab::Sessions => "Sessions",
        SettingsTab::Advanced => "Advanced",
    }
}

/// Where a row's value comes from, as the line under the list says it.
fn origin(row: &SettingRow) -> String {
    if is_run(row) {
        return "set by -c for this run".to_owned();
    }
    match (&row.value, row.layer.as_str()) {
        (Shown::Unset, _) => "not set".to_owned(),
        (_, "default") => "using the default".to_owned(),
        (_, layer) => format!("set in {layer}"),
    }
}

/// Whether the run's `-c` sets the row's value: the file cannot change it.
fn is_run(row: &SettingRow) -> bool {
    row.layer == "-c"
}

// Keys.

impl Settings {
    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if self.page.is_some() {
            self.page_key(key, ctx);
            return Act::Stay;
        }
        let shown = self.shown(ctx);
        match &mut self.mode {
            Mode::Rows => self.rows_key(key, ctx),
            Mode::Field(_) => {
                self.field_key(key, ctx);
                Act::Stay
            }
            Mode::Choices { names, list, .. } => {
                if *key == Key::Enter {
                    let name = names.get(list.selected()).cloned();
                    return name.map_or(Act::Stay, |name| self.choose(&name, ctx));
                }
                if *key == Key::Esc {
                    // A cancel restores the entry theme and writes
                    // nothing: the preview it undoes was never saved.
                    let entry = self.entry_theme.clone();
                    self.mode = Mode::Rows;
                    return Act::Theme(entry);
                }
                if list.key(key, names.len(), shown) {
                    // Moving focus previews the focused theme live;
                    // the single-slot queue keeps only the last one.
                    if let Some(name) = names.get(list.selected()).cloned() {
                        return Act::Theme(ctx.seam.theme(ctx.workspace, &name));
                    }
                }
                Act::Stay
            }
        }
    }

    /// A key over the rows: move, change the focused value, move it to
    /// another layer, open the file, or close.
    fn rows_key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            return Act::Close;
        }
        if *key == Key::CtrlG {
            let row = self.selected();
            let file = row.and_then(|row| row.file.clone());
            if row.is_some_and(|row| crate::remote::client_owned(&row.key)) {
                return Act::OpenLocal(file);
            }
            return Act::Open(file.unwrap_or_else(|| ctx.seam.global_file()));
        }
        if *key == Key::Enter || *key == Key::Char(' ') {
            return self.change(ctx);
        }
        if *key == Key::Tab {
            return self.move_layer(ctx);
        }
        let layout = self.layout(ctx.width, ctx.height, ctx.usage);
        let mut list = self.list_over(&layout);
        let before = list.selected();
        list.key_over(key, layout.lines.len(), layout.body, &layout.takes);
        if list.selected() != before {
            self.said.clear();
        }
        self.list = list;
        self.focus_line(list.selected(), ctx);
        Act::Stay
    }

    /// Shifts the window one wheel step. The rows move three, then the
    /// focus settles on the nearest row; the theme choices move three
    /// through their own list with the same live preview. The field and
    /// the list page swallow the wheel, so it never retargets what they
    /// edit.
    pub(crate) fn wheel(&mut self, up: bool, ctx: &Ctx<'_>) -> Act {
        let shown = self.shown(ctx);
        if let Some(page) = &mut self.page {
            if page.field.is_none() {
                let rows = page.entries.len().saturating_add(1);
                page.list.wheel(up, rows, shown, 3);
            }
            return Act::Stay;
        }
        match &mut self.mode {
            Mode::Rows => {
                let layout = self.layout(ctx.width, ctx.height, ctx.usage);
                let mut list = self.list_over(&layout);
                let before = list.selected();
                list.wheel(up, layout.lines.len(), layout.body, 3);
                if list.selected() != before {
                    self.said.clear();
                }
                list.rest_on(&layout.takes, layout.lines.len(), layout.body);
                self.list = list;
                self.focus_line(list.selected(), ctx);
                Act::Stay
            }
            Mode::Choices { names, list, .. } => {
                let before = list.selected();
                list.wheel(up, names.len(), shown, 3);
                if list.selected() != before
                    && let Some(name) = names.get(list.selected()).cloned()
                {
                    return Act::Theme(ctx.seam.theme(ctx.workspace, &name));
                }
                Act::Stay
            }
            // The field takes Up/Down without scrolling the rows: the
            // wheel is swallowed, so clamping cannot retarget the key.
            Mode::Field(_) => Act::Stay,
        }
    }

    /// Handles one editing key: the field's, while one is open; else ←→
    /// move between tabs and Delete resets the focused value.
    pub(crate) fn edit_key(&mut self, edit: Edit, ctx: &Ctx<'_>) -> Act {
        if let Some(page) = &mut self.page {
            if let Some(field) = &mut page.field {
                edit_draft(&mut field.draft, edit);
            } else if edit == Edit::Delete {
                self.page_remove(ctx);
            }
            return Act::Stay;
        }
        match &mut self.mode {
            Mode::Field(field) => {
                edit_draft(&mut field.draft, edit);
                Act::Stay
            }
            Mode::Choices { .. } => Act::Stay,
            Mode::Rows => match edit {
                Edit::Left => {
                    self.step_tab(false, ctx);
                    Act::Stay
                }
                Edit::Right => {
                    self.step_tab(true, ctx);
                    Act::Stay
                }
                Edit::Delete => self.reset(ctx),
                Edit::ShiftEnter
                | Edit::CtrlJ
                | Edit::WordLeft
                | Edit::WordRight
                | Edit::DeleteWord
                | Edit::DeleteToLineStart
                | Edit::LineStart
                | Edit::LineEnd
                | Edit::Paste(_) => Act::Stay,
            },
        }
    }

    /// Moves to the next or previous tab; the first and last stop.
    fn step_tab(&mut self, forward: bool, ctx: &Ctx<'_>) {
        let at = SettingsTab::ALL
            .iter()
            .position(|tab| *tab == self.tab)
            .unwrap_or(0);
        let to = if forward {
            at.saturating_add(1)
                .min(SettingsTab::ALL.len().saturating_sub(1))
        } else {
            at.saturating_sub(1)
        };
        if let Some(tab) = SettingsTab::ALL.get(to).copied()
            && tab != self.tab
        {
            self.tab = tab;
            self.focus = None;
            self.list = List::gutter();
            self.said.clear();
            self.place(ctx);
        }
    }

    /// A click: the ✕ closes, a row is focused, and a theme choice is
    /// chosen.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        if let Some(page) = &self.page {
            if let (Spot::Row(at), None) = (spot, &page.field)
                && at <= page.entries.len()
            {
                self.page_select(at, ctx);
            }
            return Act::Stay;
        }
        match (spot, &mut self.mode) {
            (Spot::Close, _) => Act::Close,
            (Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_), _) => Act::Stay,
            (Spot::Row(at), Mode::Choices { names, .. }) => match names.get(at).cloned() {
                Some(name) => self.choose(&name, ctx),
                None => Act::Stay,
            },
            (Spot::Row(at), Mode::Rows) => {
                self.focus_line(at, ctx);
                self.said.clear();
                Act::Stay
            }
            (Spot::Row(_), Mode::Field(_)) => Act::Stay,
            // Only the model picker draws cells with targets of their
            // own; here a cell is never pushed.
            (Spot::Cell(_, _), _) => Act::Stay,
        }
    }
}

/// An editing key in a field's draft.
fn edit_draft(draft: &mut Draft, edit: Edit) {
    match edit {
        Edit::Paste(text) => text
            .chars()
            .filter(|ch| *ch != '\n' && *ch != '\t')
            .for_each(|ch| draft.insert(ch)),
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
        | Edit::Delete => draft.edit(edit),
    }
}

// Changing a value.

impl Settings {
    /// The layer a change to `row` saves to: the one its value already
    /// comes from, else the first the key allows (global first).
    fn write_layer(row: &SettingRow) -> Layer {
        let allowed = layers(row.scope);
        layer_named(&row.layer)
            .filter(|layer| allowed.contains(layer))
            .or_else(|| allowed.first().copied())
            .unwrap_or(Layer::Global)
    }

    /// Writes `text` to `key` in `layer`, then reads the rows again. A
    /// saved write says where; a refused one says why and changes nothing.
    /// Whether it saved.
    fn save(&mut self, ctx: &Ctx<'_>, layer: Layer, key: &str, text: &str) -> bool {
        match ctx.seam.set(ctx.workspace, layer, key, text) {
            Ok(saved) => {
                self.reread(ctx);
                let mut said = vec![format!("Saved to {} config.", layer_name(layer))];
                said.extend(saved.warnings);
                self.said = said;
                true
            }
            Err(error) => {
                self.said = vec![error.message];
                false
            }
        }
    }

    /// Enter or Space on the focused row, by its control.
    fn change(&mut self, ctx: &Ctx<'_>) -> Act {
        self.said.clear();
        let items = self.items();
        let Some(item) = self.focused(&items) else {
            return Act::Stay;
        };
        let at = match item {
            Item::Screen(screen) => {
                self.request = Some(Request::Screen(screen));
                return Act::Stay;
            }
            Item::Key(at) => at,
        };
        let Some(row) = self.row(at).cloned() else {
            return Act::Stay;
        };
        // A value the run's `-c` sets is read-only: no control writes
        // under it, whatever opens it.
        if is_run(&row) {
            return Act::Stay;
        }
        let layer = Self::write_layer(&row);
        match (&row.control, &row.value) {
            (_, Shown::Redacted(_) | Shown::Union { .. }) | (SettingControl::Json, _) => {
                self.said = vec![format!(
                    "{} has no control here; set it with `fiber config set`.",
                    row.key
                )];
                Act::Stay
            }
            (SettingControl::Boolean, value) => {
                let on = !matches!(value, Shown::Value(text) if text == "true");
                self.save(ctx, layer, &row.key, if on { "true" } else { "false" });
                Act::Stay
            }
            (SettingControl::Fixed(choices), value) => {
                let next = next_choice(choices, current_text(value).as_deref());
                self.step(ctx, layer, &row.key, next)
            }
            (SettingControl::Theme, value) => {
                let mut names: Vec<String> = BUILT_IN_THEMES.map(str::to_owned).to_vec();
                names.extend(
                    ctx.seam
                        .themes(ctx.workspace)
                        .into_iter()
                        .filter(|name| !BUILT_IN_THEMES.contains(&name.as_str())),
                );
                let next = next_choice(&names, current_text(value).as_deref());
                self.step(ctx, layer, &row.key, next)
            }
            (SettingControl::Model, _) => {
                self.awaiting = Some((row.key.clone(), layer));
                self.request = Some(Request::Model);
                Act::Stay
            }
            (SettingControl::Number | SettingControl::Text, value) => {
                let mut draft = Draft::default();
                draft.set(&current_text(value).unwrap_or_default());
                self.mode = Mode::Field(Field {
                    key: row.key.clone(),
                    layer,
                    number: row.control == SettingControl::Number,
                    draft,
                });
                Act::Stay
            }
            (SettingControl::StringList | SettingControl::RepositoryExtensions, value) => {
                let entries = match value {
                    Shown::Value(text) => {
                        serde_json::from_str::<Vec<Value>>(text).unwrap_or_default()
                    }
                    Shown::Unset | Shown::Union { .. } | Shown::Redacted(_) => Vec::new(),
                };
                self.page = Some(Page {
                    key: row.key.clone(),
                    label: row.presentation.label.clone(),
                    layer,
                    objects: row.control == SettingControl::RepositoryExtensions,
                    entries,
                    list: List::gutter(),
                    field: None,
                });
                Act::Stay
            }
        }
    }

    /// Writes one fixed value, and applies the theme once it saved.
    fn step(&mut self, ctx: &Ctx<'_>, layer: Layer, key: &str, name: Option<String>) -> Act {
        let Some(name) = name else {
            return Act::Stay;
        };
        if !self.save(ctx, layer, key, &quoted(&name)) {
            return Act::Stay;
        }
        if key == THEME {
            return Act::Theme(ctx.seam.theme(ctx.workspace, &name));
        }
        Act::Stay
    }

    /// Tab: moves the focused value to the next layer its key allows.
    /// The destination is written first and the source removed after, so
    /// a refused write leaves the value where it was and a refused removal
    /// leaves it in both places and says so.
    fn move_layer(&mut self, ctx: &Ctx<'_>) -> Act {
        self.said.clear();
        let Some(row) = self.selected().cloned() else {
            return Act::Stay;
        };
        if is_run(&row) {
            return Act::Stay;
        }
        let Shown::Value(original) = &row.value else {
            return Act::Stay;
        };
        let allowed = layers(row.scope);
        let from = Self::write_layer(&row);
        let Some(at) = allowed.iter().position(|layer| *layer == from) else {
            return Act::Stay;
        };
        let to = allowed
            .get(at.saturating_add(1) % allowed.len().max(1))
            .copied()
            .unwrap_or(from);
        if to == from {
            return Act::Stay;
        }
        if let Err(error) = ctx.seam.set(ctx.workspace, to, &row.key, original) {
            self.said = vec![error.message];
            return Act::Stay;
        }
        // A default has no file to take it out of.
        let from_file = layer_named(&row.layer);
        let removed = from_file.map(|layer| {
            ctx.seam
                .reset(ctx.workspace, layer, &row.key, original)
                .map(|_| layer)
        });
        self.reread(ctx);
        self.said = match removed {
            None => vec![format!("Saved to {} config.", layer_name(to))],
            Some(Ok(layer)) => vec![format!(
                "Moved from {} to {} config.",
                layer_name(layer),
                layer_name(to)
            )],
            Some(Err(error)) => vec![format!(
                "Saved to {} config, but removing it from {} failed: {}",
                layer_name(to),
                row.layer,
                error.message
            )],
        };
        self.theme_after(&row.key, ctx)
    }

    /// Delete: removes the focused value's override so the inherited value
    /// applies.
    fn reset(&mut self, ctx: &Ctx<'_>) -> Act {
        self.said.clear();
        let Some(row) = self.selected().cloned() else {
            return Act::Stay;
        };
        if is_run(&row) {
            return Act::Stay;
        }
        let (Shown::Value(expected), Some(layer)) = (&row.value, layer_named(&row.layer)) else {
            return Act::Stay;
        };
        match ctx.seam.reset(ctx.workspace, layer, &row.key, expected) {
            Ok(_) => {
                self.reread(ctx);
                self.said = vec![format!(
                    "Removed {} from {} config.",
                    row.key,
                    layer_name(layer)
                )];
                self.theme_after(&row.key, ctx)
            }
            Err(error) => {
                self.said = vec![error.message];
                Act::Stay
            }
        }
    }

    /// The theme to apply after a change to `key`: the effective one, once
    /// the row is read again.
    fn theme_after(&self, key: &str, ctx: &Ctx<'_>) -> Act {
        if key == THEME {
            Act::Theme(ctx.seam.theme(ctx.workspace, &self.theme_name()))
        } else {
            Act::Stay
        }
    }

    // The field.

    /// A key in the edit field: typing, Enter to write, Esc to close it
    /// writing nothing.
    fn field_key(&mut self, key: &Key, ctx: &Ctx<'_>) {
        if *key == Key::Enter {
            self.submit(ctx);
            return;
        }
        if *key == Key::Esc {
            self.mode = Mode::Rows;
            self.said.clear();
            return;
        }
        let Mode::Field(field) = &mut self.mode else {
            return;
        };
        if let Key::Char(ch) = key {
            field.draft.insert(*ch);
        } else if *key == Key::Backspace {
            field.draft.backspace();
        }
    }

    /// Writes the field's text. A saved write closes the field; a refused
    /// one keeps it, with the draft, and says why.
    fn submit(&mut self, ctx: &Ctx<'_>) {
        let Mode::Field(field) = &self.mode else {
            return;
        };
        let typed = field.draft.expand();
        let text = if field.number {
            typed.trim().to_owned()
        } else {
            quoted(&typed)
        };
        let (layer, key) = (field.layer, field.key.clone());
        if self.save(ctx, layer, &key, &text) {
            self.mode = Mode::Rows;
        }
    }

    // The legacy theme choices.

    /// Opens the theme choices over the `tui.theme` row. A theme the run's
    /// `-c` sets is read-only, so the choices stay shut and the row says
    /// where its value comes from.
    fn open_choices(&mut self, ctx: &Ctx<'_>) {
        if self.rows.iter().any(|row| row.key == THEME && is_run(row)) {
            return;
        }
        let current = self
            .rows
            .iter()
            .find(|row| row.key == THEME)
            .and_then(|row| match &row.value {
                Shown::Value(value) => Some(as_typed(value)),
                Shown::Unset | Shown::Union { .. } | Shown::Redacted(_) => None,
            });
        let mut names: Vec<String> = BUILT_IN_THEMES.map(str::to_owned).to_vec();
        names.extend(
            ctx.seam
                .themes(ctx.workspace)
                .into_iter()
                .filter(|name| !BUILT_IN_THEMES.contains(&name.as_str())),
        );
        let mut list = List::gutter();
        let at = names
            .iter()
            .position(|name| Some(name) == current.as_ref())
            .unwrap_or(0);
        list.select(at, names.len(), self.shown(ctx));
        self.mode = Mode::Choices {
            names,
            current,
            list,
        };
    }

    /// Writes theme `name` to the global `tui.theme`, then asks for it to
    /// apply. A failed write applies nothing.
    fn choose(&mut self, name: &str, ctx: &Ctx<'_>) -> Act {
        if self.save(ctx, Layer::Global, THEME, &quoted(name)) {
            self.mode = Mode::Rows;
            return Act::Theme(ctx.seam.theme(ctx.workspace, name));
        }
        Act::Stay
    }
}

/// A value's text as a field takes it: a string bare, anything else as
/// its JSON.
fn current_text(value: &Shown) -> Option<String> {
    match value {
        Shown::Value(text) => Some(as_typed(text)),
        Shown::Unset | Shown::Union { .. } | Shown::Redacted(_) => None,
    }
}

/// The choice after `current` in `choices`, wrapping from the last to the
/// first; the first when `current` is none of them. `None` for no choices.
fn next_choice(choices: &[String], current: Option<&str>) -> Option<String> {
    let at = current.and_then(|current| choices.iter().position(|name| name == current));
    let to = match at {
        Some(at) => at.saturating_add(1) % choices.len().max(1),
        None => 0,
    };
    choices.get(to).cloned()
}

// The list sub-page.

impl Settings {
    /// A key on the list sub-page.
    fn page_key(&mut self, key: &Key, ctx: &Ctx<'_>) {
        let shown = self.shown(ctx);
        let Some(page) = self.page.as_mut() else {
            return;
        };
        if let Some(field) = page.field.as_mut() {
            if *key == Key::Enter {
                self.page_submit(ctx);
            } else if *key == Key::Esc {
                page.field = None;
                self.said.clear();
            } else if let Key::Char(ch) = key {
                field.draft.insert(*ch);
            } else if *key == Key::Backspace {
                field.draft.backspace();
            }
            return;
        }
        let rows = page.entries.len().saturating_add(1);
        let at = page.list.selected().min(page.entries.len());
        if *key == Key::Esc {
            self.page = None;
            self.said.clear();
        } else if *key == Key::Enter {
            let entry = page.entries.get(at);
            let mut draft = Draft::default();
            draft.set(&entry.map(entry_path).unwrap_or_default());
            page.field = Some(PageField {
                at: entry.map(|_| at),
                draft,
            });
            self.said.clear();
        } else if *key == Key::Char(' ') {
            if page.objects && at < page.entries.len() {
                self.page_flip(at, ctx);
            }
        } else if *key == Key::AltUp {
            self.page_move(at, false, ctx);
        } else if *key == Key::AltDown {
            self.page_move(at, true, ctx);
        } else if page.list.key(key, rows, shown) {
            self.said.clear();
        }
    }

    /// Puts the page's focus on entry `at` (or "+ Add" past the last).
    fn page_select(&mut self, at: usize, ctx: &Ctx<'_>) {
        let shown = self.shown(ctx);
        if let Some(page) = &mut self.page {
            let rows = page.entries.len().saturating_add(1);
            page.list.select(at, rows, shown);
        }
    }

    /// Saves `candidate` as the whole list; on success it becomes the
    /// page's entries. Whether it saved.
    fn page_commit(&mut self, candidate: Vec<Value>, ctx: &Ctx<'_>) -> bool {
        let Some(page) = &self.page else {
            return false;
        };
        let (layer, key) = (page.layer, page.key.clone());
        let text = Value::Array(candidate.clone()).to_string();
        if !self.save(ctx, layer, &key, &text) {
            return false;
        }
        if let Some(page) = &mut self.page {
            page.entries = candidate;
        }
        true
    }

    /// Enter in the page's field: adds or edits the entry.
    fn page_submit(&mut self, ctx: &Ctx<'_>) {
        let Some(page) = &self.page else {
            return;
        };
        let Some(field) = &page.field else {
            return;
        };
        let typed = field.draft.expand();
        let mut candidate = page.entries.clone();
        let at = field.at;
        match at.and_then(|at| candidate.get_mut(at)) {
            Some(entry) => set_path(entry, &typed),
            None => candidate.push(new_entry(page.objects, &typed)),
        }
        let focus = at.unwrap_or(candidate.len().saturating_sub(1));
        if self.page_commit(candidate, ctx) {
            if let Some(page) = &mut self.page {
                page.field = None;
            }
            self.page_select(focus, ctx);
        }
    }

    /// Space on an object entry: flips `required`.
    fn page_flip(&mut self, at: usize, ctx: &Ctx<'_>) {
        let Some(mut candidate) = self.page.as_ref().map(|page| page.entries.clone()) else {
            return;
        };
        if let Some(Value::Object(entry)) = candidate.get_mut(at) {
            let required = entry
                .get("required")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            entry.insert("required".to_owned(), Value::Bool(!required));
        }
        self.page_commit(candidate, ctx);
    }

    /// Delete: removes the focused entry. "+ Add" has none.
    fn page_remove(&mut self, ctx: &Ctx<'_>) {
        let Some(page) = &self.page else {
            return;
        };
        let at = page.list.selected();
        if at >= page.entries.len() {
            return;
        }
        let mut candidate = page.entries.clone();
        candidate.remove(at);
        if self.page_commit(candidate, ctx) {
            self.page_select(at, ctx);
        }
    }

    /// ⌥↑ and ⌥↓: moves the focused entry one place; at either end, or on
    /// "+ Add", nothing moves.
    fn page_move(&mut self, at: usize, down: bool, ctx: &Ctx<'_>) {
        let Some(page) = &self.page else {
            return;
        };
        let to = if down {
            at.saturating_add(1)
        } else {
            at.saturating_sub(1)
        };
        if at >= page.entries.len() || to >= page.entries.len() || to == at {
            return;
        }
        let mut candidate = page.entries.clone();
        candidate.swap(at, to);
        if self.page_commit(candidate, ctx) {
            self.page_select(to, ctx);
        }
    }
}

/// An entry's text: the string, or an object's `path`.
fn entry_path(entry: &Value) -> String {
    match entry {
        Value::String(text) => text.clone(),
        Value::Object(object) => object
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => String::new(),
    }
}

/// Replaces an entry's text; an object keeps every other member.
fn set_path(entry: &mut Value, text: &str) {
    match entry {
        Value::Object(object) => {
            object.insert("path".to_owned(), Value::String(text.to_owned()));
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            *entry = Value::String(text.to_owned());
        }
    }
}

/// A new entry: a string, or an object with `required` off.
fn new_entry(objects: bool, text: &str) -> Value {
    if objects {
        serde_json::json!({"path": text, "required": false})
    } else {
        Value::String(text.to_owned())
    }
}

// Drawing.

/// A footer of `keys` (bold) and what they do, as the doc words it:
/// `("Enter", " save · ")`, ...
fn footer(parts: &[(&str, &str)]) -> (String, Vec<Span>) {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let mut plain = String::new();
    let mut spans = Vec::new();
    for (key, what) in parts {
        plain.push_str(key);
        plain.push_str(what);
        spans.push(((*key).to_owned(), bold));
        spans.push(((*what).to_owned(), Ink::Muted.style()));
    }
    (plain, spans)
}

impl Settings {
    /// The frame to draw at `width` by `height`.
    pub(crate) fn frame(&self, width: usize, height: usize, usage: Option<u64>) -> Frame {
        if let Some(page) = &self.page {
            return self.page_frame(page, width, height);
        }
        match &self.mode {
            Mode::Choices {
                names,
                current,
                list,
            } => Frame {
                joins: Vec::new(),
                title: format!("Settings › {THEME}"),
                command: "/settings".to_owned(),
                rows: names
                    .iter()
                    .map(|name| {
                        let mark = if Some(name) == current.as_ref() {
                            "  (current)"
                        } else {
                            ""
                        };
                        vec![(format!("{name}{mark}"), None, Ink::Plain)]
                    })
                    .collect(),
                list: *list,
                below: self.said.clone(),
                field: None,
                footer: "↑↓ move · Enter choose · Esc back".to_owned(),
                chrome: None,
            },
            Mode::Rows | Mode::Field(_) => self.rows_frame(width, height, usage),
        }
    }

    /// The open tab.
    fn rows_frame(&self, width: usize, height: usize, usage: Option<u64>) -> Frame {
        let layout = self.layout(width, height, usage);
        let list = if layout.takes.is_empty() {
            self.list.blur()
        } else {
            self.list_over(&layout)
        };
        let editing = match &self.mode {
            Mode::Field(field) => layout.focus.map(|line| (line, field)),
            Mode::Rows | Mode::Choices { .. } => None,
        };
        let rows = layout
            .lines
            .iter()
            .enumerate()
            .map(|(at, line)| match line {
                Line::Blank => Vec::new(),
                Line::Note(note) => vec![((*note).to_owned(), None, Ink::Muted)],
                Line::Item(item) => self.cells(
                    *item,
                    &layout,
                    width,
                    editing.is_some_and(|(line, _)| line == at),
                ),
            })
            .collect();
        let draft = editing.map(|(row, field)| Inline {
            row,
            column: layout.label_col.saturating_add(GAP),
            text: field.draft.expand(),
            caret: field.draft.position(),
        });
        let (plain, spans) = if draft.is_some() {
            footer(&[("Enter", " save · "), ("Esc", " cancel")])
        } else {
            footer(&[
                ("←→", " tab · "),
                ("/", " search · "),
                ("Enter", " change · "),
                ("Tab", " layer · "),
                ("Delete", " reset"),
            ])
        };
        Frame {
            joins: Vec::new(),
            title: "Settings".to_owned(),
            command: "/settings".to_owned(),
            rows,
            list,
            below: layout.below,
            field: None,
            footer: plain,
            chrome: Some(Chrome {
                top: layout.top,
                draft,
                footer: spans,
            }),
        }
    }

    /// One row's cells: the label, the value, then the layer it comes from
    /// when that is not global. A row being edited keeps its label only;
    /// the field draws over its value.
    fn cells(
        &self,
        item: Item,
        layout: &Layout,
        width: usize,
        editing: bool,
    ) -> Vec<(String, Option<Spot>, Ink)> {
        let label = text::cut(&self.label_of(item), layout.label_col, "…");
        let pad = layout
            .label_col
            .saturating_sub(text::width(&label))
            .saturating_add(GAP);
        let mut cells = vec![(format!("{label}{}", " ".repeat(pad)), None, Ink::Plain)];
        if editing {
            return cells;
        }
        let (value, tag) = match item {
            Item::Screen(screen) => (
                screen
                    .row()
                    .map_or_else(String::new, |row| format!("Open {} ›", row.command)),
                None,
            ),
            Item::Key(at) => self
                .row(at)
                .map_or_else(|| (String::new(), None), shown_value),
        };
        let tag_room = tag.as_ref().map_or(0, |tag| text::width(tag) + GAP);
        let room = width
            .saturating_sub(GUTTER + layout.label_col + GAP + tag_room)
            .max(1);
        cells.push((text::cut(&value, room, "…"), None, Ink::Plain));
        if let Some(tag) = tag {
            cells.push((format!("{}{tag}", " ".repeat(GAP)), None, Ink::Muted));
        }
        cells
    }

    /// The list sub-page.
    fn page_frame(&self, page: &Page, width: usize, height: usize) -> Frame {
        let editing = page.field.as_ref();
        let col = page
            .entries
            .iter()
            .map(|entry| text::width(&entry_path(entry)))
            .max()
            .unwrap_or(0)
            .min(width.saturating_sub(GUTTER + 16));
        let mut rows: Vec<Vec<(String, Option<Spot>, Ink)>> = page
            .entries
            .iter()
            .enumerate()
            .map(|(at, entry)| {
                if editing.is_some_and(|field| field.at == Some(at)) {
                    return Vec::new();
                }
                let path = text::controls_to_space(&entry_path(entry));
                if !page.objects {
                    return vec![(path, None, Ink::Plain)];
                }
                let required = entry
                    .get("required")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let path = text::cut(&path, col, "…");
                let pad = col.saturating_sub(text::width(&path)).saturating_add(GAP);
                vec![
                    (format!("{path}{}", " ".repeat(pad)), None, Ink::Plain),
                    (
                        if required {
                            "[x] required"
                        } else {
                            "[ ] required"
                        }
                        .to_owned(),
                        None,
                        Ink::Plain,
                    ),
                ]
            })
            .collect();
        let add_row = page.entries.len();
        rows.push(if editing.is_some_and(|field| field.at.is_none()) {
            Vec::new()
        } else {
            vec![("+ Add".to_owned(), None, Ink::Plain)]
        });
        let draft = editing.map(|field| Inline {
            row: field.at.unwrap_or(add_row),
            column: 0,
            text: field.draft.expand(),
            caret: field.draft.position(),
        });
        let room = height.saturating_sub(2 + MIN_BODY);
        let below: Vec<String> = self.said.iter().rev().take(room).rev().cloned().collect();
        let (plain, spans) = if draft.is_some() {
            footer(&[("Enter", " save · "), ("Esc", " cancel")])
        } else {
            footer(&[
                ("Enter", " edit · "),
                ("Delete", " remove · "),
                ("⌥↑⌥↓", " move · "),
                ("Esc", " back"),
            ])
        };
        let mut list = page.list;
        list.select(
            page.list.selected(),
            rows.len(),
            height.saturating_sub(2 + below.len()),
        );
        Frame {
            joins: Vec::new(),
            title: page.label.clone(),
            command: "/settings".to_owned(),
            rows,
            list,
            below,
            field: None,
            footer: plain,
            chrome: Some(Chrome {
                top: Vec::new(),
                draft,
                footer: spans,
            }),
        }
    }
}

/// A row's value as the tab says it, and the layer tag after it when the
/// value is set anywhere but global.
fn shown_value(row: &SettingRow) -> (String, Option<String>) {
    let list_text = |text: &str| {
        let names: Vec<String> = serde_json::from_str::<Vec<Value>>(text)
            .unwrap_or_default()
            .iter()
            .map(entry_path)
            .collect();
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    };
    let value = match (&row.control, &row.value) {
        (_, Shown::Redacted(text)) => text.clone(),
        (_, Shown::Union { names, .. }) => names
            .iter()
            .map(|(name, layer)| format!("{name} ({layer})"))
            .collect::<Vec<_>>()
            .join(", "),
        (
            SettingControl::Fixed(_) | SettingControl::Theme | SettingControl::Boolean,
            Shown::Unset,
        ) => "Default".to_owned(),
        (SettingControl::Boolean, Shown::Value(text)) => {
            if text == "true" { "[x] On" } else { "[ ] Off" }.to_owned()
        }
        (SettingControl::Model, Shown::Unset) => "Not set ›".to_owned(),
        (SettingControl::Model, Shown::Value(text)) => format!("{} ›", as_typed(text)),
        (SettingControl::StringList | SettingControl::RepositoryExtensions, Shown::Unset) => {
            "none ›".to_owned()
        }
        (SettingControl::StringList | SettingControl::RepositoryExtensions, Shown::Value(text)) => {
            format!("{} ›", list_text(text))
        }
        (_, Shown::Unset) => "Not set".to_owned(),
        (_, Shown::Value(text)) => as_typed(text),
    };
    let tag = match (&row.value, row.layer.as_str()) {
        (Shown::Unset, _) | (_, "global" | "default") => None,
        (_, layer) => Some(layer.to_owned()),
    };
    (text::controls_to_space(&value), tag)
}

#[cfg(test)]
#[path = "settings_view_tests.rs"]
mod tests;
