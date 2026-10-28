//! `/skills` (`docs/tui.md`, "Swapped views"; `docs/system-prompt.md`,
//! "Skills"): every skill the session's `skills` answer found, in
//! discovery order, with where it comes from, whether the model can see
//! it and the skills it shadows, and two switches per row, this project
//! and everywhere, that write `skills.disabled`
//! (`docs/configuration.md`, "Layers").

use std::path::{Path, PathBuf};

use contract::events::{SkillInfo, SkillSource};

use crate::configure::{SkillsDisabled, SwitchScope};
use crate::format::{cut, wrap};
use crate::keys::{Edit, Key};
use crate::scope_switches::ScopeSwitches;
use crate::settings_view::{Act, Ctx, applies_text};
use crate::swapped::{Frame, GUTTER, Ink, List, Spot, rows_height};
use crate::text;

/// A switch at its widest, as the room the row leaves for the switches
/// counts it, as on `/tools`.
const WIDEST_SWITCH: &str = "[off]";

/// The shadows clause at its shortest: the row keeps room for this much
/// before it leaves the clause out.
const SHADOWS_MIN: &str = " · shadows …";

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

/// A visibility as the rows show it: only `seen` and `hidden`. Switches
/// communicate disabled state and the `shadows` clause communicates
/// shadowing, so no third word is needed.
fn visibility_word(visibility: Visibility) -> &'static str {
    match visibility {
        Visibility::Model => "seen",
        Visibility::PersonOnly | Visibility::Off | Visibility::Shadowed => "hidden",
    }
}

/// Where a skill comes from, as the rows show it
/// (`docs/tui.md`, "Swapped views"): the bare extension name, unless that
/// name is taken by another source, when the kind keeps them apart.
fn source_word(info: &SkillInfo) -> String {
    match info.source {
        SkillSource::Repository => "repo".to_owned(),
        SkillSource::Personal => "personal".to_owned(),
        SkillSource::Extension => match &info.extension {
            Some(name) if name == "repo" || name == "personal" || name == "built in" => {
                format!("extension {name}")
            }
            Some(name) => name.clone(),
            None => "extension".to_owned(),
        },
        SkillSource::Builtin => "built in".to_owned(),
    }
}

/// The shadowed sources a row names, in precedence order: repo, personal,
/// extensions by extension name, then built in (`docs/tui.md`, "Swapped
/// views"). Each source appears once, however many shadow paths map to it:
/// two entries sharing a display word but not a source stay separate.
/// `None` when no path maps, or when any path fails to: a path with no
/// matching skill entry, or an extension entry with no extension name, is a
/// data inconsistency, so the row leaves out the whole `shadows` clause
/// rather than show an invented word or a partial list.
fn shadow_sources(shadows: &[String], infos: &[SkillInfo]) -> Option<Vec<String>> {
    if shadows.is_empty() {
        return None;
    }
    let mut seen: Vec<(u8, String, String)> = Vec::new();
    for path in shadows {
        let info = infos.iter().find(|info| info.path == *path)?;
        let (rank, key) = match info.source {
            SkillSource::Repository => (0, String::new()),
            SkillSource::Personal => (1, String::new()),
            SkillSource::Extension => (2, info.extension.clone()?),
            SkillSource::Builtin => (3, String::new()),
        };
        if !seen
            .iter()
            .any(|(ranked, keyed, _)| (*ranked, keyed) == (rank, &key))
        {
            seen.push((rank, key, source_word(info)));
        }
    }
    seen.sort();
    Some(seen.into_iter().map(|(_, _, word)| word).collect())
}

/// A skill's text, open in place of the rows.
#[derive(Debug)]
struct Text {
    /// The skill's name.
    name: String,
    /// Its `SKILL.md`.
    path: PathBuf,
    /// The shadow block, a blank line, and the skill's text, unwrapped.
    /// The frame wraps it at the width it draws at on each redraw.
    raw: String,
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
    /// One per skill: the first skill is selected at open, and the
    /// focused row's mark never sits on a non-skill row, since there is
    /// no header row.
    list: List,
    /// The two switches' shared state: the switch Space flips.
    focus: ScopeSwitches,
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
            list: List::gutter(),
            focus: ScopeSwitches {
                focus: SwitchScope::Project,
            },
            text: None,
            said,
        };
        skills.clamp(ctx);
        skills
    }

    /// The pane's lines while open, else one row per skill: no header
    /// row, so the count is the skills'.
    fn rows_len(&self, width: usize) -> usize {
        match &self.text {
            Some(text) => wrap(&text.raw, width).len(),
            None => self.infos.len(),
        }
    }

    /// The rows the view shows at `height` and `width`.
    fn shown(&self, ctx: &Ctx<'_>) -> usize {
        rows_height(&self.frame(ctx.width, ctx.height), ctx.height)
    }

    /// The selected skill: none with no answer.
    fn selected_info(&self) -> Option<&SkillInfo> {
        self.infos.get(self.list.selected())
    }

    /// Handles one key: in the text pane Esc returns to the rows, every
    /// other key scrolls it, and Ctrl+G opens the skill's file.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
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
        let shown = self.shown(ctx);
        if self.list.key(key, self.rows_len(ctx.width), shown) {
            self.said.clear();
            // The new selection can add two details lines, and
            // clearing the message frees one, so the frame's available
            // rows change: clamp to the final frame.
            self.clamp(ctx);
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
        // The frame wraps at the width it draws at on each redraw, so
        // scrolling counts the lines at this key's width.
        let shown = self.shown(ctx);
        if let Some(text) = &mut self.text {
            text.list.key(key, wrap(&text.raw, ctx.width).len(), shown);
        }
        Act::Stay
    }

    /// Shifts the window one wheel step: three rows, clamping to the
    /// frame's final rows as the keys do. The open text pane takes
    /// Up/Down without scrolling the rows, so the wheel is swallowed
    /// there and never retargets the skill.
    pub(crate) fn wheel(&mut self, up: bool, ctx: &Ctx<'_>) -> Act {
        if self.text.is_some() {
            return Act::Stay;
        }
        let shown = self.shown(ctx);
        let before = (self.list.top(), self.list.selected());
        self.list.wheel(up, self.rows_len(ctx.width), shown, 3);
        if (self.list.top(), self.list.selected()) != before {
            self.said.clear();
            // The new selection can add two details lines, and
            // clearing the message frees one, so the frame's available
            // rows change: clamp to the final frame.
            self.clamp(ctx);
        }
        Act::Stay
    }

    /// Handles one editing key: ← and → choose the row's switch.
    pub(crate) fn edit_key(&mut self, edit: &Edit) {
        self.focus.edit_key(edit);
    }

    /// A click: the ✕ closes, a row is selected, and a switch is selected
    /// and flipped. In the text pane a row scrolls to the line.
    pub(crate) fn click(&mut self, spot: Spot, ctx: &Ctx<'_>) -> Act {
        match spot {
            Spot::Close => Act::Close,
            Spot::Revoke(_) | Spot::Cell(_, _) | Spot::Item(_) => Act::Stay,
            Spot::Row(at) => {
                let shown = self.shown(ctx);
                if let Some(text) = &mut self.text {
                    text.list
                        .select(at, wrap(&text.raw, ctx.width).len(), shown);
                } else {
                    self.list.select(at, self.rows_len(ctx.width), shown);
                }
                // The new selection's details lines change the frame's
                // available rows: clamp to the final frame.
                self.clamp(ctx);
                Act::Stay
            }
            Spot::Switch { row, at } => {
                if self.text.is_some() {
                    return Act::Stay;
                }
                let shown = self.shown(ctx);
                self.list.select(row, self.rows_len(ctx.width), shown);
                self.focus.focus = ScopeSwitches::scope_for_at(at);
                self.switch(ctx)
            }
        }
    }

    /// Opens the selected skill's text in place of the rows, with every
    /// shadow path first so each stays reachable. A read error stays on
    /// the rows and shows the message.
    fn enter(&mut self, ctx: &Ctx<'_>) -> Act {
        let Some(info) = self.selected_info().cloned() else {
            return Act::Stay;
        };
        match ctx.seam.skill_text(Path::new(&info.path)) {
            Ok(raw) => {
                // The frame wraps the text at the width it draws at, so
                // the pane needs only the unwrapped body here.
                self.text = Some(Text {
                    name: info.name.clone(),
                    path: PathBuf::from(&info.path),
                    raw: pane_text(&info, &self.infos, &raw),
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
    /// name, so a winner and the rows it shadows flip together.
    fn switch(&mut self, ctx: &Ctx<'_>) -> Act {
        let Some(info) = self.selected_info().cloned() else {
            return Act::Stay;
        };
        let name = info.name.clone();
        let scope = self.focus.focused();
        ScopeSwitches::write_then_reread(
            scope,
            self,
            |view, scope| view.on_in_scope(&name, scope),
            |_view, scope, on| ctx.seam.switch_skill(ctx.workspace, &name, scope, on),
            |view| view.reread(ctx),
            |view| view.said = vec![applies_text("skills.disabled", None)],
            // A write may have landed before the error, so the rows
            // show what the files hold before the message does.
            |view, error| view.said = vec![error],
        );
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
        if let Ok(off) = ctx.seam.skills_disabled(ctx.workspace) {
            self.off = off;
        }
        let path = self.text.as_ref().map(|text| text.path.clone());
        if let Some(path) = path {
            match ctx.seam.skill_text(&path) {
                Ok(raw) => {
                    if let Some(info) = self.infos.iter().find(|info| Path::new(&info.path) == path)
                    {
                        // The frame wraps at the width it draws at, so
                        // the re-read keeps only the unwrapped body.
                        if let Some(text) = &mut self.text {
                            text.raw = pane_text(info, &self.infos, &raw);
                        }
                    }
                }
                Err(error) => self.said = vec![error.message],
            }
        }
        self.clamp(ctx);
    }

    /// Keeps the selection on a row at `ctx`'s width and height: the
    /// pane's lines while open, else the skill rows'.
    fn clamp(&mut self, ctx: &Ctx<'_>) {
        let shown = rows_height(&self.frame(ctx.width, ctx.height), ctx.height);
        if let Some(text) = &mut self.text {
            let selected = text.list.selected();
            text.list
                .select(selected, wrap(&text.raw, ctx.width).len(), shown);
        } else {
            let selected = self.list.selected();
            self.list.select(selected, self.rows_len(ctx.width), shown);
        }
    }

    /// The answer to the sent command: any other answer, and an answer to
    /// an older id, is ignored.
    pub(crate) fn answered(&mut self, id: &str, skills: &[SkillInfo], ctx: &Ctx<'_>) {
        if self.awaiting.as_deref() != Some(id) {
            return;
        }
        self.infos = skills.to_vec();
        self.awaiting = None;
        self.clamp(ctx);
    }

    /// The sent command's rejection: only for its id.
    pub(crate) fn rejected(&mut self, id: &str, message: &str) {
        if self.awaiting.as_deref() != Some(id) {
            return;
        }
        self.awaiting = None;
        self.said = vec![message.to_owned()];
    }

    /// The frame to draw at `width` columns and `height` rows: the text
    /// pane while open, else the rows, then at most two details lines
    /// for the selected skill and the last action's line, budgeted so
    /// one row always stays. Rows, details and pane lines all render at
    /// the passed width on each redraw, including a resize with no
    /// intervening key.
    pub(crate) fn frame(&self, width: usize, height: usize) -> Frame {
        if let Some(text) = &self.text {
            return Frame {
                title: format!("Skill {}", text.name),
                command: "/skills".to_owned(),
                rows: wrap(&text.raw, width)
                    .iter()
                    .map(|line| vec![(line.clone(), None, Ink::Plain)])
                    .collect(),
                list: text.list,
                below: self.said.clone(),
                field: None,
                footer: "↑↓ scroll · Ctrl+G open the file · Esc back".to_owned(),
            };
        }
        let rows = self
            .infos
            .iter()
            .enumerate()
            .map(|(index, info)| self.skill_cells(index, info, width))
            .collect();
        let room = height.saturating_sub(3);
        let message = usize::from(!self.said.is_empty()).min(room);
        let budget = DETAILS.min(room.saturating_sub(message));
        let mut below: Vec<String> = self.details(width).into_iter().take(budget).collect();
        below.extend(self.said.iter().take(message).cloned());
        if self.awaiting.is_some() {
            below.insert(0, "Reading the skills…".to_owned());
        }
        Frame {
            title: "Skills".to_owned(),
            command: "/skills".to_owned(),
            rows,
            list: self.list,
            below,
            field: None,
            footer: "Enter shows the text · Ctrl+G opens its file".to_owned(),
        }
    }

    /// The selected skill's details: its description, then one shadow
    /// summary line, whatever the number of shadows.
    fn details(&self, width: usize) -> Vec<String> {
        let Some(info) = self.selected_info() else {
            return Vec::new();
        };
        // The description is cut to the width the view draws at; the
        // pane's lines wrap instead, staying readable at 80 columns.
        let mut details = vec![cut(&info.description, width)];
        match info.shadows.as_slice() {
            [path] => details.push(format!("shadows {path}")),
            [] => {
                if let Some(path) = &info.shadowed_by {
                    details.push(format!("shadowed by {path}"));
                }
            }
            many => details.push(format!("shadows {} skills; Enter lists them", many.len())),
        }
        details
    }

    /// One skill's cells: `<name> · <description> · <source> ·
    /// <visibility> · this project ` and its switch, then ` · everywhere `
    /// and its switch, then the shadows clause. The row stays one line:
    /// both switches stay whole, and the clause past them is cut from its
    /// tail with `…`. The description is cut first, then the name, then
    /// the skill's own source, keeping room for at least ` · shadows …`;
    /// below that minimum the clause is left out.
    fn skill_cells(
        &self,
        index: usize,
        info: &SkillInfo,
        width: usize,
    ) -> Vec<(String, Option<Spot>, Ink)> {
        let selected = index == self.list.selected();
        let visibility = visibility_word(visibility(info, &self.off));
        let full = match shadow_sources(&info.shadows, &self.infos) {
            Some(sources) => format!(" · shadows {}", sources.join(", ")),
            None => String::new(),
        };
        let project = " · this project";
        let everywhere = " · everywhere";
        // Each switch follows a column holding the focus mark, in the
        // place of the space before it, as on `/tools`.
        let widest = format!("{project} {WIDEST_SWITCH}{everywhere} {WIDEST_SWITCH}");
        // The separators around the variable fields, the visibility, the
        // gutter and both switches at their widest never give.
        let fixed =
            GUTTER + 3 * text::width(" · ") + text::width(visibility) + text::width(&widest);
        // The clause stays when the fixed fields and both switches leave
        // at least its shortest form; else the fields take that room.
        let keep = !full.is_empty() && width.saturating_sub(fixed) >= text::width(SHADOWS_MIN);
        let room = if keep {
            width.saturating_sub(fixed + text::width(SHADOWS_MIN))
        } else {
            width.saturating_sub(fixed)
        };
        let source = source_word(info);
        let description_room = room.saturating_sub(text::width(&info.name) + text::width(&source));
        let description = text::cut(&info.description, description_room, "…");
        let name_room = room.saturating_sub(text::width(&description) + text::width(&source));
        let name = text::cut(&info.name, name_room, "…");
        let source_room = room.saturating_sub(text::width(&description) + text::width(&name));
        let source = text::cut(&source, source_room, "…");
        let head = format!("{name} · {description} · {source} · {visibility}");
        // Past the switches the clause shows what fits, tail-cut.
        let shadows = if keep {
            let leftover = width.saturating_sub(GUTTER + text::width(&head) + text::width(&widest));
            text::cut(&full, leftover, "…")
        } else {
            String::new()
        };
        let switch = |scope: SwitchScope, at: usize| {
            let (text, ink) = ScopeSwitches::mark(selected, self.focus.focused() == scope);
            let mark = (text, None, ink);
            let (word, ink) = if self.on_in_scope(&info.name, scope) {
                ("[on]", Ink::Heading)
            } else {
                ("[off]", Ink::Muted)
            };
            [
                mark,
                (word.to_owned(), Some(Spot::Switch { row: index, at }), ink),
            ]
        };
        let [project_mark, project_switch] = switch(SwitchScope::Project, 0);
        let [everywhere_mark, everywhere_switch] = switch(SwitchScope::Everywhere, 1);
        vec![
            (format!("{head}{project}"), None, Ink::Plain),
            project_mark,
            project_switch,
            (everywhere.to_owned(), None, Ink::Plain),
            everywhere_mark,
            everywhere_switch,
            (shadows, None, Ink::Plain),
        ]
    }
}

/// The skill's text with its shadow block first: one `shadows <source> ·
/// <path>` line per shadow path, in path order, then any `shadowed by
/// <path>` line, a blank line, and the text, so every shadow path stays
/// reachable. A path with no matching skill entry, or an extension entry
/// with no extension name, keeps its path-only line: no source word can
/// be derived from inconsistent data.
fn pane_text(info: &SkillInfo, infos: &[SkillInfo], text: &str) -> String {
    let mut block: Vec<String> = info
        .shadows
        .iter()
        .map(|path| match infos.iter().find(|entry| entry.path == *path) {
            Some(entry)
                if !matches!(entry.source, SkillSource::Extension if entry.extension.is_none()) =>
            {
                format!("shadows {} · {path}", source_word(entry))
            }
            _ => format!("shadows {path}"),
        })
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

#[cfg(test)]
#[path = "skills_view_tests.rs"]
mod tests;
