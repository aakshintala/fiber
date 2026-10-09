//! `/skills` (`docs/tui.md`, "Swapped views"; `docs/system-prompt.md`,
//! "Skills"): every skill the session's `skills` answer found, in
//! discovery order, with where it comes from, whether the model can see
//! it and the skills it shadows, and two switches per row, this project
//! and everywhere, that write `skills.disabled`
//! (`docs/configuration.md`, "Layers").

use std::path::{Path, PathBuf};

use contract::events::{SkillInfo, SkillSource};

use crate::configure::{SkillsDisabled, SwitchScope};
use crate::format::{cut, width, wrap};
use crate::keys::{Edit, Key};
use crate::settings_view::{Act, Ctx, applies_text};
use crate::swapped::{Frame, List, Spot, rows_height};
use crate::tools_view::{fit, mark};

/// A skill name's column.
const NAME: usize = 18;

/// The `this project` switch's column.
const PROJECT: usize = 12;

/// The `everywhere` switch's column.
const EVERYWHERE: usize = 10;

/// The visibility word's column.
const VISIBILITY: usize = 13;

/// A source's column.
const SOURCE: usize = 14;

/// The details lines below the rows at most.
const DETAILS: usize = 2;

/// Whether the model can load a skill (`docs/system-prompt.md`,
/// "Skills").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Visibility {
    /// The model can load it.
    Model,
    /// Only the person can run it.
    PersonOnly,
    /// `skills.disabled` names it in either person file.
    Off,
    /// Another skill shadows it.
    Shadowed,
}

/// A skill's visibility: `off` when either `skills.disabled` list names
/// it, else `shadowed` when another skill shadows it (the model cannot
/// load a shadowed skill), else whether its header and place let the
/// model load it. The answer's `disabled` is fixed at session start, so
/// the view never reads it.
pub(crate) fn visibility(info: &SkillInfo, off: &SkillsDisabled) -> Visibility {
    if off.project.iter().any(|name| name == &info.name)
        || off.everywhere.iter().any(|name| name == &info.name)
    {
        Visibility::Off
    } else if info.shadowed_by.is_some() {
        Visibility::Shadowed
    } else if info.model_invocable {
        Visibility::Model
    } else {
        Visibility::PersonOnly
    }
}

/// A visibility as the rows show it.
fn visibility_word(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Model => "model sees it",
        Visibility::PersonOnly => "person only",
        Visibility::Off => "off",
        Visibility::Shadowed => "shadowed",
    }
}

/// Where a skill comes from, as the rows show it
/// (`docs/tui.md`, "Swapped views").
fn source_word(info: &SkillInfo) -> String {
    match info.source {
        SkillSource::Repository => "repository".to_owned(),
        SkillSource::Personal => "personal".to_owned(),
        SkillSource::Extension => match &info.extension {
            Some(name) => format!("extension {name}"),
            None => "extension".to_owned(),
        },
        SkillSource::Builtin => "built in".to_owned(),
    }
}

/// A source in at least `SOURCE` columns: padded, never cut, so an
/// extension's name stays visible.
fn source_cell(info: &SkillInfo) -> String {
    let mut out = source_word(info);
    out.push_str(&" ".repeat(SOURCE.saturating_sub(width(&out))));
    out
}

/// A skill's text, open in place of the rows.
#[derive(Debug)]
struct Text {
    /// The skill's name.
    name: String,
    /// Its `SKILL.md`.
    path: PathBuf,
    /// The shadow block, a blank line, and the skill's text, unwrapped.
    raw: String,
    /// `raw` wrapped at `width`.
    lines: Vec<String>,
    /// The width `lines` wrap at.
    width: usize,
    /// The scrolled line.
    list: List,
}

/// The `/skills` view's state.
#[derive(Debug)]
pub(crate) struct Skills {
    /// The `skills` command's id until its answer arrives.
    awaiting: Option<String>,
    /// The answer's skills, in discovery order.
    infos: Vec<SkillInfo>,
    /// Each person file's own `skills.disabled`, re-read after each
    /// switch.
    off: SkillsDisabled,
    /// The header's row plus one per skill.
    list: List,
    /// The switch Space flips: this project at open.
    focus: SwitchScope,
    /// The width the details cut and the pane wraps at.
    width: usize,
    /// The text pane while open.
    text: Option<Text>,
    /// What the last action said, shown below the rows.
    said: Vec<String>,
}

impl Skills {
    /// The view opening with the `skills` command `id` sent: the
    /// `skills.disabled` lists read; a failed read says why.
    pub(crate) fn open(id: String, ctx: &Ctx<'_>) -> Self {
        let (off, said) = match ctx.seam.skills_disabled(ctx.workspace) {
            Ok(off) => (off, Vec::new()),
            Err(error) => (SkillsDisabled::default(), vec![error.message]),
        };
        let mut skills = Self {
            awaiting: Some(id),
            infos: Vec::new(),
            off,
            list: List::default(),
            focus: SwitchScope::Project,
            width: ctx.width,
            text: None,
            said,
        };
        skills.clamp(ctx.height);
        skills
    }

    /// The pane's lines while open, else one past the header plus one
    /// row per skill.
    fn rows_len(&self) -> usize {
        match &self.text {
            Some(text) => text.lines.len(),
            None => self.infos.len().saturating_add(1),
        }
    }

    /// The rows the view shows at `height`.
    fn shown(&self, height: usize) -> usize {
        rows_height(&self.frame(height), height)
    }

    /// The selected skill: none on the header row or with no answer.
    fn selected_info(&self) -> Option<&SkillInfo> {
        self.infos.get(self.list.selected().checked_sub(1)?)
    }

    /// Handles one key: in the text pane Esc returns to the rows, every
    /// other key scrolls it, and Ctrl+G opens the skill's file.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        self.width = ctx.width;
        if self.text.is_some() {
            return self.text_key(key, ctx);
        }
        if *key == Key::Esc {
            return Act::Close;
        }
        if *key == Key::CtrlG {
            return self.open_file();
        }
        if *key == Key::Enter {
            return self.enter(ctx);
        }
        if *key == Key::Char(' ') {
            return self.switch(ctx);
        }
        let shown = self.shown(ctx.height);
        if self.list.key(key, self.rows_len(), shown) {
            self.said.clear();
        }
        Act::Stay
    }

    /// A key with the text pane open.
    fn text_key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            self.text = None;
            return Act::Stay;
        }
        if *key == Key::CtrlG {
            return self.open_file();
        }
        // The view draws at the width it is given, so the pane wraps
        // again when a key arrives at a new width.
        if self
            .text
            .as_ref()
            .is_some_and(|text| ctx.width != text.width)
        {
            let width = ctx.width;
            if let Some(text) = &mut self.text {
                text.width = width;
                text.lines = wrap(&text.raw, width);
            }
        }
        let shown = self.shown(ctx.height);
        if let Some(text) = &mut self.text {
            text.list.key(key, text.lines.len(), shown);
        }
        Act::Stay
    }

    /// Handles one editing key: ← and → choose the row's switch.
    pub(crate) fn edit_key(&mut self, edit: &Edit) {
        match edit {
            Edit::Left => self.focus = SwitchScope::Project,
            Edit::Right => self.focus = SwitchScope::Everywhere,
            Edit::ShiftEnter
            | Edit::CtrlJ
            | Edit::WordLeft
            | Edit::WordRight
            | Edit::DeleteWord
            | Edit::LineStart
            | Edit::LineEnd
            | Edit::Delete
            | Edit::Paste(_) => {}
        }
    }

    /// A click: the ✕ closes, a row is selected, and a switch is selected
    /// and flipped. In the text pane a row scrolls to the line.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        self.width = ctx.width;
        match spot {
            Spot::Close => Act::Close,
            Spot::Revoke(_) => Act::Stay,
            Spot::Row(at) => {
                let shown = self.shown(ctx.height);
                if let Some(text) = &mut self.text {
                    text.list.select(at, text.lines.len(), shown);
                } else {
                    self.list.select(at, self.rows_len(), shown);
                }
                Act::Stay
            }
            Spot::Switch { row, at } => {
                if self.text.is_some() {
                    return Act::Stay;
                }
                let shown = self.shown(ctx.height);
                self.list.select(row, self.rows_len(), shown);
                self.focus = if at == 0 {
                    SwitchScope::Project
                } else {
                    SwitchScope::Everywhere
                };
                self.switch(ctx)
            }
        }
    }

    /// Opens the selected skill's text in place of the rows, with every
    /// shadow path first so each stays reachable. A read error stays on
    /// the rows and shows the message.
    fn enter(&mut self, ctx: &Ctx<'_>) -> Act {
        self.width = ctx.width;
        let Some(info) = self.selected_info().cloned() else {
            return Act::Stay;
        };
        match ctx.seam.skill_text(Path::new(&info.path)) {
            Ok(raw) => {
                let body = pane_text(&info, &raw);
                let lines = wrap(&body, ctx.width);
                self.text = Some(Text {
                    name: info.name.clone(),
                    path: PathBuf::from(&info.path),
                    raw: body,
                    lines,
                    width: ctx.width,
                    list: List::default(),
                });
                self.said.clear();
            }
            Err(error) => self.said = vec![error.message],
        }
        Act::Stay
    }

    /// Opens the selected skill's file: the pane's while open, else the
    /// selected row's. With no row selected nothing opens.
    fn open_file(&self) -> Act {
        if let Some(text) = &self.text {
            return Act::Open(text.path.clone());
        }
        match self.selected_info() {
            Some(info) => Act::Open(PathBuf::from(&info.path)),
            None => Act::Stay,
        }
    }

    /// Flips the focused switch of the selected row. The switch is by
    /// name, so a winner and the rows it shadows flip together. The
    /// header row has nothing to flip.
    fn switch(&mut self, ctx: &Ctx<'_>) -> Act {
        let Some(info) = self.selected_info().cloned() else {
            return Act::Stay;
        };
        let on = !self.on_in_scope(&info.name, self.focus);
        match ctx
            .seam
            .switch_skill(ctx.workspace, &info.name, self.focus, on)
        {
            Ok(()) => {
                self.reread(ctx);
                self.said = vec![applies_text("skills.disabled", None)];
            }
            Err(error) => {
                // A write may have landed before the error, so the rows
                // show what the files hold before the message does.
                self.reread(ctx);
                self.said = vec![error.message];
            }
        }
        Act::Stay
    }

    /// Whether `name` is on in `scope`'s own list: a file with no list
    /// starts from none, never an inherited list, since both lists apply
    /// (`docs/configuration.md`, "Layers").
    fn on_in_scope(&self, name: &str, scope: SwitchScope) -> bool {
        let listed = match scope {
            SwitchScope::Project => &self.off.project,
            SwitchScope::Everywhere => &self.off.everywhere,
        };
        !listed.iter().any(|listed| listed == name)
    }

    /// Reads the lists again, and the text pane's file while open,
    /// keeping the selection where it was; a failed re-read keeps the
    /// last lists and text.
    pub(crate) fn reread(&mut self, ctx: &Ctx<'_>) {
        self.width = ctx.width;
        if let Ok(off) = ctx.seam.skills_disabled(ctx.workspace) {
            self.off = off;
        }
        let path = self.text.as_ref().map(|text| text.path.clone());
        if let Some(path) = path {
            match ctx.seam.skill_text(&path) {
                Ok(raw) => {
                    if let Some(info) = self.infos.iter().find(|info| Path::new(&info.path) == path)
                    {
                        let body = pane_text(info, &raw);
                        let lines = wrap(&body, ctx.width);
                        if let Some(text) = &mut self.text {
                            text.raw = body;
                            text.lines = lines;
                            text.width = ctx.width;
                        }
                    }
                }
                Err(error) => self.said = vec![error.message],
            }
        }
        self.clamp(ctx.height);
    }

    /// Keeps the selection on a row at `height`.
    fn clamp(&mut self, height: usize) {
        let shown = rows_height(&self.frame(height), height);
        self.list
            .select(self.list.selected(), self.rows_len(), shown);
    }

    /// The answer to the sent command: any other answer, and an answer to
    /// an older id, is ignored.
    pub(crate) fn answered(&mut self, id: &str, skills: &[SkillInfo], ctx: &Ctx<'_>) {
        if self.awaiting.as_deref() != Some(id) {
            return;
        }
        self.width = ctx.width;
        self.infos = skills.to_vec();
        self.awaiting = None;
        self.clamp(ctx.height);
    }

    /// The sent command's rejection: only for its id.
    pub(crate) fn rejected(&mut self, id: &str, message: &str) {
        if self.awaiting.as_deref() != Some(id) {
            return;
        }
        self.awaiting = None;
        self.said = vec![message.to_owned()];
    }

    /// The frame to draw at `height`: the text pane while open, else the
    /// rows, then at most two details lines for the selected skill and
    /// the last action's line, budgeted so one row always stays.
    pub(crate) fn frame(&self, height: usize) -> Frame {
        if let Some(text) = &self.text {
            return Frame {
                title: format!("Skill {}", text.name),
                rows: text
                    .lines
                    .iter()
                    .map(|line| vec![(line.clone(), None)])
                    .collect(),
                list: text.list,
                below: self.said.clone(),
                field: None,
                footer: "↑↓ scroll · Ctrl+G open the file · Esc back".to_owned(),
            };
        }
        let mut rows = vec![heading_cells()];
        rows.extend(
            self.infos
                .iter()
                .enumerate()
                .map(|(index, info)| self.skill_cells(index.saturating_add(1), info)),
        );
        let room = height.saturating_sub(3);
        let message = usize::from(!self.said.is_empty()).min(room);
        let budget = DETAILS.min(room.saturating_sub(message));
        let mut below: Vec<String> = self.details().into_iter().take(budget).collect();
        below.extend(self.said.iter().take(message).cloned());
        if self.awaiting.is_some() {
            below.insert(0, "Reading the skills…".to_owned());
        }
        Frame {
            title: "Skills".to_owned(),
            rows,
            list: self.list,
            below,
            field: None,
            footer: "↑↓ move · ←→ choose a switch · Space switch · Enter text · Ctrl+G open the file · Esc close"
                .to_owned(),
        }
    }

    /// The selected skill's details: its description, then one shadow
    /// summary line, whatever the number of shadows.
    fn details(&self) -> Vec<String> {
        let Some(info) = self.selected_info() else {
            return Vec::new();
        };
        // The description is cut to the width the view draws at; the
        // pane's lines wrap instead, staying readable at 80 columns.
        let mut details = vec![cut(&info.description, self.width)];
        if info.shadows.len() == 1
            && let Some(path) = info.shadows.first()
        {
            details.push(format!("shadows {path}"));
        } else if info.shadows.len() > 1 {
            details.push(format!(
                "shadows {} skills; Enter lists them",
                info.shadows.len()
            ));
        } else if let Some(path) = &info.shadowed_by {
            details.push(format!("shadowed by {path}"));
        }
        details
    }

    /// One skill's cells: its name, its two switches, its visibility,
    /// its source and its description.
    fn skill_cells(&self, index: usize, info: &SkillInfo) -> Vec<(String, Option<Spot>)> {
        let selected = index == self.list.selected();
        let project = (
            format!(
                "{:^PROJECT$}",
                mark(
                    self.on_in_scope(&info.name, SwitchScope::Project),
                    selected && self.focus == SwitchScope::Project
                )
            ),
            Some(Spot::Switch { row: index, at: 0 }),
        );
        let everywhere = (
            format!(
                "{:^EVERYWHERE$}",
                mark(
                    self.on_in_scope(&info.name, SwitchScope::Everywhere),
                    selected && self.focus == SwitchScope::Everywhere
                )
            ),
            Some(Spot::Switch { row: index, at: 1 }),
        );
        debug_assert_eq!(width(&project.0), PROJECT);
        debug_assert_eq!(width(&everywhere.0), EVERYWHERE);
        vec![
            (format!("  {}", fit(&info.name, NAME)), None),
            project,
            ("  ".to_owned(), None),
            everywhere,
            (
                format!(
                    "  {:<VISIBILITY$}  {}  {}",
                    visibility_word(visibility(info, &self.off)),
                    source_cell(info),
                    info.description
                ),
                None,
            ),
        ]
    }
}

/// The skill's text with its shadow block first: each `shadows <path>`
/// and the `shadowed by <path>` line, then a blank line, then the text,
/// so every shadow path stays reachable.
fn pane_text(info: &SkillInfo, text: &str) -> String {
    let mut block: Vec<String> = info
        .shadows
        .iter()
        .map(|path| format!("shadows {path}"))
        .collect();
    if let Some(path) = &info.shadowed_by {
        block.push(format!("shadowed by {path}"));
    }
    if block.is_empty() {
        text.to_owned()
    } else {
        block.join("\n") + "\n\n" + text
    }
}

/// The header row: each column's name over its column.
fn heading_cells() -> Vec<(String, Option<Spot>)> {
    vec![(
        format!(
            "{}{:^PROJECT$}  {:^EVERYWHERE$}  {:<VISIBILITY$}  {:<SOURCE$}  description",
            fit("skill", NAME.saturating_add(2)),
            "this project",
            "everywhere",
            "visibility",
            "source",
        ),
        None,
    )]
}

#[cfg(test)]
#[path = "skills_view_tests.rs"]
mod tests;
