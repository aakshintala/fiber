//! `/settings` (`docs/tui.md`, "Swapped views", "Themes"): every
//! configuration key with its effective value and layer, an edit field
//! that writes through the same path as `fiber config set`, the choice of
//! theme, and when a written key applies (`docs/configuration.md`, "When
//! Fiber reads configuration").

use std::path::{Path, PathBuf};

use std::fmt;

use serde_json::Value;

use crate::ThemeSetting;
use crate::configure::{Configure, Layer, SettingRow, Shown, WriteScope};
use crate::input::Draft;
use crate::keys::{Edit, Key};
use crate::swapped::{Frame, GUTTER, Ink, List, Spot, rows_height};
use crate::text;

/// The key the theme choice edits.
pub(crate) const THEME: &str = "tui.theme";

/// The themes offered before the theme files: following the terminal,
/// then the built-ins.
pub(crate) const BUILT_IN_THEMES: [&str; 3] = ["auto", "dark", "light"];

/// The widest key column; a longer key pushes its value right.
const KEY_COLUMN: usize = 36;

/// The widest layer word, `repository`: the room the layer column keeps
/// at the end of a row.
const LAYER_COLUMN: usize = 10;

/// The gap between two columns.
const GAP: usize = 2;

/// The columns the two gaps of a row take.
const GAPS: usize = 2 * GAP;

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
pub(crate) fn reload_text(key: &str, usage: Option<u64>) -> String {
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

/// A layer's name, as the edit field says it.
fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Global => "global",
        Layer::Project => "project",
        Layer::Repository => "repository",
    }
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

/// The edit field: the layers a write may choose, the one chosen, the
/// typed text, and whether the row's value is hidden.
struct Field {
    layers: Vec<Layer>,
    at: usize,
    draft: Draft,
    redacted: bool,
}

/// The field may hold a typed secret replacing a redacted value, so it
/// prints its draft redacted (`docs/code-quality.md`, "Errors").
impl fmt::Debug for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Field")
            .field("layers", &self.layers)
            .field("at", &self.at)
            .field("draft", &"redacted")
            .field("redacted", &self.redacted)
            .finish()
    }
}

impl Field {
    /// The layer chosen.
    fn layer(&self) -> Layer {
        self.layers.get(self.at).copied().unwrap_or(Layer::Global)
    }
}

/// What the view shows below its header.
#[derive(Debug)]
enum Mode {
    /// The keys.
    Rows,
    /// The edit field over the selected key.
    Field(Field),
    /// The theme choices: their names, the current one, the selection.
    Choices {
        names: Vec<String>,
        current: Option<String>,
        list: List,
    },
}

/// The `/settings` view's state.
#[derive(Debug)]
pub(crate) struct Settings {
    rows: Vec<SettingRow>,
    list: List,
    mode: Mode,
    /// The entry `tui.theme`, resolved once when the view opened: a
    /// cancel restores this, whatever a preview queued since.
    entry_theme: ThemeSetting,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
}

impl Settings {
    /// The view over `ctx`'s workspace, its rows read.
    pub(crate) fn open(ctx: &Ctx<'_>) -> Self {
        let mut settings = Self {
            rows: Vec::new(),
            list: List::gutter(),
            mode: Mode::Rows,
            entry_theme: ThemeSetting::Follow,
            said: Vec::new(),
        };
        settings.reread(ctx);
        settings.entry_theme = settings.entry_setting(ctx);
        settings
    }

    /// The lines the list holds: the header row, then one per key; none
    /// with no keys.
    fn list_len(&self) -> usize {
        if self.rows.is_empty() {
            0
        } else {
            self.rows.len() + 1
        }
    }

    /// The view opened directly in the theme choices, as `/theme`
    /// opens it: the `tui.theme` row focused with its choices open.
    /// With no theme row the view opens on its rows, as `open` does.
    pub(crate) fn open_theme(ctx: &Ctx<'_>) -> Self {
        let mut settings = Self::open(ctx);
        if let Some(at) = settings.rows.iter().position(|row| row.key == THEME) {
            settings
                .list
                .select(at + 1, settings.list_len(), settings.shown(ctx));
            settings.edit(ctx);
        }
        settings
    }

    /// The entry `tui.theme`, resolved against `ctx`'s seam: the theme
    /// the choices restore on a cancel.
    fn entry_setting(&self, ctx: &Ctx<'_>) -> ThemeSetting {
        let name = self
            .rows
            .iter()
            .find(|row| row.key == THEME)
            .map(|row| match &row.value {
                Shown::Value(value) => as_typed(value),
                Shown::Unset | Shown::Union { .. } | Shown::Redacted(_) => "auto".to_owned(),
            })
            .unwrap_or_else(|| "auto".to_owned());
        ctx.seam.theme(ctx.workspace, &name)
    }

    /// The captured entry theme, while the theme choices are open: what
    /// a cancel restores. `None` on the rows and in the field.
    pub(crate) fn choices_entry(&self) -> Option<ThemeSetting> {
        match &self.mode {
            Mode::Choices { .. } => Some(self.entry_theme.clone()),
            Mode::Rows | Mode::Field(_) => None,
        }
    }

    /// Reads the rows again, keeping the selection where it was; a failed
    /// read says why.
    pub(crate) fn reread(&mut self, ctx: &Ctx<'_>) {
        match ctx.seam.settings(ctx.workspace) {
            Ok(rows) => self.rows = rows,
            Err(error) => {
                self.rows = Vec::new();
                self.said = vec![error.message];
            }
        }
        let shown = self.shown(ctx);
        // The header takes no selection: a list that holds keys rests on
        // one.
        let at = self.list.selected().max(1);
        self.list.select(at, self.list_len(), shown);
    }

    /// The rows the view shows at `ctx`'s height.
    fn shown(&self, ctx: &Ctx<'_>) -> usize {
        rows_height(&self.frame(ctx.width, ctx.usage), ctx.height)
    }

    /// The selected key's row; the header row is none.
    fn selected(&self) -> Option<&SettingRow> {
        self.rows.get(self.list.selected().checked_sub(1)?)
    }

    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        let shown = self.shown(ctx);
        match &mut self.mode {
            Mode::Rows => self.rows_key(key, ctx, shown),
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

    /// A key in the edit field: typing, Tab to the next layer (a union's
    /// field then holds that layer's own list), Enter to write, Esc to
    /// close it writing nothing.
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
        let union = self.selected().and_then(|row| match &row.value {
            Shown::Union { own, .. } => Some(own.clone()),
            Shown::Unset | Shown::Value(_) | Shown::Redacted(_) => None,
        });
        let Mode::Field(field) = &mut self.mode else {
            return;
        };
        if let Key::Char(ch) = key {
            field.draft.insert(*ch);
        } else if *key == Key::Backspace {
            field.draft.backspace();
        } else if *key == Key::Tab {
            field.at = (field.at + 1) % field.layers.len().max(1);
            if let Some(own) = &union {
                field.draft.set(&own_list(own, field.layer()));
            }
        }
    }

    /// A key over the rows: move, open the field or the theme choices,
    /// open the selected row's file, or close.
    fn rows_key(&mut self, key: &Key, ctx: &Ctx<'_>, shown: usize) -> Act {
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
        if *key == Key::Enter {
            self.edit(ctx);
            return Act::Stay;
        }
        let before = self.list.selected();
        self.list.key(key, self.list_len(), shown);
        // The header takes no selection.
        let at = self.list.selected().max(1);
        self.list.select(at, self.list_len(), shown);
        if self.list.selected() != before {
            self.said.clear();
        }
        Act::Stay
    }

    /// Opens the selected row's edit field, or the theme choices on the
    /// `tui.theme` row.
    fn edit(&mut self, ctx: &Ctx<'_>) {
        let Some(row) = self.selected().cloned() else {
            return;
        };
        self.said.clear();
        if row.key == THEME {
            let current = match &row.value {
                Shown::Value(value) => Some(as_typed(value)),
                Shown::Unset | Shown::Union { .. } | Shown::Redacted(_) => None,
            };
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
            return;
        }
        let layers = layers(row.scope);
        let first = layers.first().copied().unwrap_or(Layer::Global);
        let (text, redacted) = match &row.value {
            Shown::Unset => (String::new(), false),
            Shown::Value(value) => (as_typed(value), false),
            Shown::Union { own, .. } => (own_list(own, first), false),
            Shown::Redacted(_) => (String::new(), true),
        };
        let mut draft = Draft::default();
        draft.set(&text);
        self.mode = Mode::Field(Field {
            layers,
            at: 0,
            draft,
            redacted,
        });
    }

    /// Writes the field's text to its layer. A saved write reads the rows
    /// again and says the file and when the key applies; a failed one
    /// keeps the field and says why.
    fn submit(&mut self, ctx: &Ctx<'_>) {
        let (Some(row), Mode::Field(field)) = (self.selected(), &self.mode) else {
            return;
        };
        let text = field.draft.expand();
        if field.redacted && text.is_empty() {
            self.said = vec!["Nothing was typed; nothing was written.".to_owned()];
            return;
        }
        let key = row.key.clone();
        match ctx.seam.set(ctx.workspace, field.layer(), &key, &text) {
            Ok(saved) => {
                self.mode = Mode::Rows;
                self.reread(ctx);
                let mut said = vec![
                    format!("Saved to {}.", saved.file.display()),
                    applies_text(&key, ctx.usage),
                ];
                said.extend(saved.warnings);
                self.said = said;
            }
            Err(error) => self.said = vec![error.message],
        }
    }

    /// Writes theme `name` to the global `tui.theme`, then asks for it to
    /// apply. A failed write applies nothing.
    fn choose(&mut self, name: &str, ctx: &Ctx<'_>) -> Act {
        let text = Value::String(name.to_owned()).to_string();
        match ctx.seam.set(ctx.workspace, Layer::Global, THEME, &text) {
            Ok(saved) => {
                self.mode = Mode::Rows;
                self.reread(ctx);
                let mut said = vec![
                    format!("Saved to {}.", saved.file.display()),
                    applies_text(THEME, ctx.usage),
                ];
                said.extend(saved.warnings);
                self.said = said;
                Act::Theme(ctx.seam.theme(ctx.workspace, name))
            }
            Err(error) => {
                self.said = vec![error.message];
                Act::Stay
            }
        }
    }

    /// Handles one editing key: the field's, while it is open.
    pub(crate) fn edit_key(&mut self, edit: Edit) {
        if let Mode::Field(field) = &mut self.mode {
            match edit {
                Edit::Paste(text) => text.chars().for_each(|ch| field.draft.insert(ch)),
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
                | Edit::Delete => field.draft.edit(edit),
            }
        }
    }

    /// A click: the ✕ closes, a row is selected, and a theme choice is
    /// chosen.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        let shown = self.shown(ctx);
        match (spot, &mut self.mode) {
            (Spot::Close, _) => Act::Close,
            (Spot::Switch { .. } | Spot::Revoke(_) | Spot::Item(_), _) => Act::Stay,
            (Spot::Row(at), Mode::Choices { names, .. }) => match names.get(at).cloned() {
                Some(name) => self.choose(&name, ctx),
                None => Act::Stay,
            },
            (Spot::Row(at), Mode::Rows) => {
                // The header row is not a key.
                if (1..=self.rows.len()).contains(&at) {
                    self.list.select(at, self.list_len(), shown);
                    self.said.clear();
                }
                Act::Stay
            }
            (Spot::Row(_), Mode::Field(_)) => Act::Stay,
            // Only the model picker draws cells with targets of their
            // own; here a cell is never pushed.
            (Spot::Cell(_, _), _) => Act::Stay,
        }
    }

    /// The frame to draw at `width` columns.
    pub(crate) fn frame(&self, width: usize, usage: Option<u64>) -> Frame {
        match &self.mode {
            Mode::Choices {
                names,
                current,
                list,
            } => Frame {
                title: format!("Settings › {THEME}"),
                command: "/settings",
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
            },
            Mode::Rows => {
                let below = if self.said.is_empty() {
                    self.selected()
                        .map(|row| vec![applies_text(&row.key, usage)])
                        .unwrap_or_default()
                } else {
                    self.said.clone()
                };
                Frame {
                    title: "Settings".to_owned(),
                    command: "/settings",
                    rows: self.lines(width),
                    list: self.rows_list(),
                    below,
                    field: None,
                    footer: "Ctrl+G opens the file".to_owned(),
                }
            }
            Mode::Field(field) => {
                let key = self.selected().map_or("", |row| row.key.as_str());
                let mut below = self.said.clone();
                if field.redacted {
                    below.push("The stored value is hidden; what you type replaces it.".to_owned());
                }
                below.push(format!(
                    "{key}, written to the {} file:",
                    layer_name(field.layer())
                ));
                Frame {
                    title: "Settings".to_owned(),
                    command: "/settings",
                    rows: self.lines(width),
                    list: self.rows_list(),
                    below,
                    field: Some((field.draft.expand(), field.draft.position())),
                    footer: "Tab layer · Enter save · Esc cancel".to_owned(),
                }
            }
        }
    }

    /// The list the rows draw with: with no key to focus, none is marked.
    fn rows_list(&self) -> List {
        if self.rows.is_empty() {
            self.list.blur()
        } else {
            self.list
        }
    }

    /// The table: a dim header row, then one line per key as aligned
    /// columns of key, value and layer. The value column is cut with `…`
    /// to the room the key and layer columns leave, so the layer column
    /// stays where it is; a key wider than its column pushes its value.
    fn lines(&self, width: usize) -> Vec<Vec<(String, Option<Spot>, Ink)>> {
        if self.rows.is_empty() {
            return Vec::new();
        }
        let key_col = self
            .rows
            .iter()
            .map(|row| text::width(&row.key))
            .chain(["key".len()])
            .max()
            .unwrap_or(0)
            .min(KEY_COLUMN);
        let values: Vec<String> = self.rows.iter().map(shown_value).collect();
        let room = width.saturating_sub(key_col + LAYER_COLUMN + GAPS + GUTTER);
        let value_col = values
            .iter()
            .map(|value| text::width(value))
            .chain(["value".len()])
            .max()
            .unwrap_or(0)
            .min(room);
        let line = |key: &str, value: &str, layer: &str, ink: Ink| {
            let key_pad = key_col.saturating_sub(text::width(key)) + GAP;
            let value = text::cut(value, value_col, "…");
            let value_pad = value_col.saturating_sub(text::width(&value)) + GAP;
            vec![
                (format!("{key}{}", " ".repeat(key_pad)), None, ink),
                (format!("{value}{}", " ".repeat(value_pad)), None, ink),
                (layer.to_owned(), None, ink),
            ]
        };
        let mut lines = vec![line("key", "value", "layer", Ink::Muted)];
        lines.extend(
            self.rows
                .iter()
                .zip(&values)
                .map(|(row, value)| line(&row.key, value, &row.layer, Ink::Plain)),
        );
        lines
    }
}

/// A key's effective value as the table says it: a string bare, as
/// `fiber config set` takes it, anything else as its JSON, a union as
/// its names and layers.
fn shown_value(row: &SettingRow) -> String {
    let value = match &row.value {
        Shown::Unset => "unset".to_owned(),
        Shown::Value(value) => as_typed(value),
        Shown::Redacted(value) => value.clone(),
        Shown::Union { names, .. } => names
            .iter()
            .map(|(name, layer)| format!("{name} ({layer})"))
            .collect::<Vec<_>>()
            .join(", "),
    };
    text::controls_to_space(&value)
}

/// `layer`'s own list in a union, as the field takes it; `[]` when the
/// layer sets none.
fn own_list(own: &[(Layer, String)], layer: Layer) -> String {
    own.iter()
        .find(|(at, _)| *at == layer)
        .map_or_else(|| "[]".to_owned(), |(_, list)| list.clone())
}

#[cfg(test)]
#[path = "settings_view_tests.rs"]
mod tests;
