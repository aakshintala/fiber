//! `/skills` (`docs/tui.md`, "Swapped views"; `docs/system-prompt.md`,
//! "Skills"): every skill the session's `skills` answer found, in
//! discovery order, with where it comes from, whether the model can see
//! it and the skills it shadows, and two switches per row, this project
//! and everywhere, that write `skills.disabled`
//! (`docs/configuration.md`, "Layers").

use contract::events::{SkillInfo, SkillSource};

use crate::configure::{SkillsDisabled, SwitchScope};
use crate::format::width;
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
            said,
        };
        skills.clamp(ctx.height);
        skills
    }

    /// One past the header plus one row per skill.
    fn rows_len(&self) -> usize {
        self.infos.len().saturating_add(1)
    }

    /// The rows the view shows at `height`.
    fn shown(&self, height: usize) -> usize {
        rows_height(&self.frame(height), height)
    }

    /// The selected skill: none on the header row or with no answer.
    fn selected_info(&self) -> Option<&SkillInfo> {
        self.infos.get(self.list.selected().checked_sub(1)?)
    }

    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            return Act::Close;
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
    /// and flipped.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        match spot {
            Spot::Close => Act::Close,
            Spot::Revoke(_) => Act::Stay,
            Spot::Row(at) => {
                let shown = self.shown(ctx.height);
                self.list.select(at, self.rows_len(), shown);
                Act::Stay
            }
            Spot::Switch { row, at } => {
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

    /// Reads the lists again, keeping the selection where it was; a
    /// failed re-read keeps the last lists.
    pub(crate) fn reread(&mut self, ctx: &Ctx<'_>) {
        if let Ok(off) = ctx.seam.skills_disabled(ctx.workspace) {
            self.off = off;
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

    /// The frame to draw at `height`: the rows, then at most two details
    /// lines for the selected skill and the last action's line, budgeted
    /// so one row always stays.
    pub(crate) fn frame(&self, height: usize) -> Frame {
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
            footer: "↑↓ move · ←→ choose a switch · Space switch · Esc close".to_owned(),
        }
    }

    /// The selected skill's details: its description, then one shadow
    /// summary line, whatever the number of shadows.
    fn details(&self) -> Vec<String> {
        let Some(info) = self.selected_info() else {
            return Vec::new();
        };
        let mut details = vec![info.description.clone()];
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
