//! `/tools` (`docs/tui.md`, "Swapped views"; `docs/tools.md`, "Seeing the
//! tools"): every declared tool by source with its state and size, and each
//! MCP or extension tool's two switches, this project and everywhere, that
//! write that source's `tools.enabled` and `tools.disabled`
//! (`docs/configuration.md`).

use contract::events::{ToolInfo, ToolSource, ToolState};

use crate::configure::{SwitchScope, ToolGroup, ToolLists, ToolSwitches};
use crate::format::tokens;
use crate::keys::{Edit, Key};
use crate::settings_view::{Act, Ctx};
use crate::swapped::{Frame, GUTTER, Ink, List, Spot, about, rows_height};
use crate::text;

/// A switch at its widest, as the name's room counts it.
const WIDEST_SWITCH: &str = "[off]";

/// Whether a tool is declared under `lists` (`mcp::start::kept`'s rule).
pub(crate) fn on(lists: &ToolLists, tool: &str) -> bool {
    lists
        .enabled
        .as_ref()
        .is_none_or(|names| names.iter().any(|name| name == tool))
        && !lists.disabled.iter().any(|name| name == tool)
}

/// A switch's mark: on or off, with `›` when it is the selection's focus.
pub(crate) fn mark(on: bool, focused: bool) -> String {
    let mark = if on { "[x]" } else { "[ ]" };
    if focused {
        format!("›{mark}")
    } else {
        mark.to_owned()
    }
}

/// The group a tool row's switches write.
fn group_of(source: &ToolSource) -> Option<ToolGroup> {
    match source {
        ToolSource::Builtin => None,
        ToolSource::Extension { extension } => Some(ToolGroup::Extension(extension.clone())),
        ToolSource::Mcp { server, .. } => Some(ToolGroup::Mcp(server.clone())),
    }
}

/// The name a row shows and a switch writes: an MCP tool's server-side
/// name, else the declared name (`docs/tools.md`, "Seeing the tools").
fn own_name(info: &ToolInfo) -> String {
    match &info.source {
        ToolSource::Mcp { tool, .. } => tool.clone(),
        ToolSource::Builtin | ToolSource::Extension { .. } => info.name.clone(),
    }
}

/// A state as the rows show it.
fn state_word(state: ToolState) -> &'static str {
    match state {
        ToolState::Full => "full",
        ToolState::Deferred => "deferred",
        ToolState::Loaded => "loaded",
    }
}

/// A size as the rows show it: tokens once a request gave a rate, as
/// `~1.2k` or `~800`, else bytes (`docs/tools.md`, "Seeing the tools").
fn size_text(info: &ToolInfo) -> String {
    match info.tokens {
        Some(tokens) => format!("~{}", token_figure(tokens)),
        None => format!("{} bytes", about(info.bytes)),
    }
}

/// A token count as the rows say it: the figure of [`tokens`] without
/// its unit and without a zero decimal, so whole thousands read "12k".
fn token_figure(count: u64) -> String {
    let text = tokens(count);
    let text = text
        .strip_suffix(" tokens")
        .or_else(|| text.strip_suffix(" token"))
        .unwrap_or(&text);
    text.strip_suffix(".0k")
        .map(|kept| format!("{kept}k"))
        .or_else(|| text.strip_suffix(".0M").map(|kept| format!("{kept}M")))
        .unwrap_or_else(|| text.to_owned())
}

/// A group heading's name.
fn heading_of(group: &Option<ToolGroup>) -> String {
    match group {
        None => "Built-in".to_owned(),
        Some(ToolGroup::Extension(name)) => format!("Extension · {name}"),
        Some(ToolGroup::Mcp(name)) => format!("MCP · {name}"),
    }
}

/// One row of the view.
#[derive(Debug)]
enum Entry {
    /// A group's heading.
    Heading { group: Option<ToolGroup> },
    /// The blank row between two groups.
    Blank,
    /// A tool.
    Tool(ToolRow),
}

/// One tool's row.
#[derive(Debug)]
struct ToolRow {
    /// Whose lists its switches write; none is built in, with no switch.
    group: Option<ToolGroup>,
    /// The server's own name for an MCP tool, else the declared name.
    name: String,
    /// `full`, `deferred`, `loaded`, or `off`.
    state: &'static str,
    /// Tokens or bytes, empty for an `off` row.
    size: String,
}

/// The `/tools` view's state.
#[derive(Debug)]
pub(crate) struct Tools {
    /// The `tools` command's id until its answer arrives.
    awaiting: Option<String>,
    /// The answer's tools.
    infos: Vec<ToolInfo>,
    /// Every group's lists, re-read after each switch.
    switches: Vec<ToolSwitches>,
    /// Off rows shown so far: kept until the view closes, so switching one
    /// on does not make it vanish.
    unlisted: Vec<(ToolGroup, String)>,
    /// Headings and tool rows.
    rows: Vec<Entry>,
    list: List,
    /// The switch Space flips: this project at open.
    focus: SwitchScope,
    /// What the last failed action said, shown below the rows.
    said: Vec<String>,
}

impl Tools {
    /// The view opening with the `tools` command `id` sent: its switches
    /// read; a failed read says why.
    pub(crate) fn open(id: String, ctx: &Ctx<'_>) -> Self {
        let (switches, said) = match ctx.seam.tool_switches(ctx.workspace) {
            Ok(switches) => (switches, Vec::new()),
            Err(error) => (Vec::new(), vec![error.message]),
        };
        let mut tools = Self {
            awaiting: Some(id),
            infos: Vec::new(),
            switches,
            unlisted: Vec::new(),
            rows: Vec::new(),
            list: List::gutter(),
            focus: SwitchScope::Project,
            said,
        };
        tools.rebuild(ctx);
        tools
    }

    /// The rows the view shows at `ctx`'s height.
    fn shown(&self, ctx: &Ctx<'_>) -> usize {
        rows_height(&self.frame(ctx.width, None), ctx.height)
    }

    /// The rows that take a switch: a tool with a group. Up and Down
    /// move between them only.
    fn takes(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches!(entry, Entry::Tool(row) if row.group.is_some()))
            .map(|(at, _)| at)
            .collect()
    }

    /// Handles one key.
    pub(crate) fn key(&mut self, key: &Key, ctx: &Ctx<'_>) -> Act {
        if *key == Key::Esc {
            return Act::Close;
        }
        if *key == Key::Char(' ') {
            return self.switch(ctx);
        }
        let shown = self.shown(ctx);
        if self
            .list
            .key_over(key, self.rows.len(), shown, &self.takes())
        {
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
            | Edit::DeleteToLineStart
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
            Spot::Revoke(_) | Spot::Cell(_, _) | Spot::Item(_) => Act::Stay,
            Spot::Row(at) => {
                if self.takes().contains(&at) {
                    let shown = self.shown(ctx);
                    self.list.select(at, self.rows.len(), shown);
                }
                Act::Stay
            }
            Spot::Switch { row, at } => {
                let shown = self.shown(ctx);
                self.list.select(row, self.rows.len(), shown);
                self.focus = if at == 0 {
                    SwitchScope::Project
                } else {
                    SwitchScope::Everywhere
                };
                self.switch(ctx)
            }
        }
    }

    /// Flips the focused switch of the selected row. A heading or a
    /// built-in row has nothing to flip.
    fn switch(&mut self, ctx: &Ctx<'_>) -> Act {
        let Some(Entry::Tool(row)) = self.rows.get(self.list.selected()) else {
            return Act::Stay;
        };
        let Some(group) = row.group.clone() else {
            return Act::Stay;
        };
        let name = row.name.clone();
        let on = !self.on_in_scope(&group, &name, self.focus);
        match ctx
            .seam
            .switch_tool(ctx.workspace, &group, &name, self.focus, on)
        {
            Ok(()) => {
                self.reread(ctx);
                self.said.clear();
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

    /// Whether `tool` of `group` is on in `scope`'s lists.
    fn on_in_scope(&self, group: &ToolGroup, tool: &str, scope: SwitchScope) -> bool {
        let lists = self
            .switches
            .iter()
            .find(|known| known.group == *group)
            .map(|known| match scope {
                SwitchScope::Project => &known.project,
                SwitchScope::Everywhere => &known.everywhere,
            });
        lists.is_none_or(|lists| on(lists, tool))
    }

    /// Reads the switches again, keeping the `off` rows shown so far; a
    /// failed re-read keeps the last lists.
    fn reread(&mut self, ctx: &Ctx<'_>) {
        if let Ok(switches) = ctx.seam.tool_switches(ctx.workspace) {
            self.switches = switches;
        }
        self.rebuild(ctx);
    }

    /// The answer to the sent command: any other answer, and an answer to
    /// an older id, is ignored.
    pub(crate) fn answered(&mut self, id: &str, tools: &[ToolInfo], ctx: &Ctx<'_>) {
        if self.awaiting.as_deref() != Some(id) {
            return;
        }
        self.infos = tools.to_vec();
        self.awaiting = None;
        self.rebuild(ctx);
    }

    /// The sent command's rejection: only for its id.
    pub(crate) fn rejected(&mut self, id: &str, message: &str) {
        if self.awaiting.as_deref() != Some(id) {
            return;
        }
        self.awaiting = None;
        self.said = vec![message.to_owned()];
    }

    /// Rebuilds the rows from the answer and the switches: tool rows from
    /// the answer, plus an `off` row for every name either switch's
    /// `disabled` list holds that the answer lacks. Rows sort by group,
    /// built-in first, then by name, each group under its heading.
    fn rebuild(&mut self, ctx: &Ctx<'_>) {
        let mut tools: Vec<ToolRow> = self
            .infos
            .iter()
            .map(|info| ToolRow {
                group: group_of(&info.source),
                name: own_name(info),
                state: state_word(info.state),
                size: size_text(info),
            })
            .collect();
        // Off rows join only once the answer is in: before it every
        // name is lacking, and the view shows no rows while it waits.
        if self.awaiting.is_none() {
            for switches in &self.switches {
                for name in switches
                    .project
                    .disabled
                    .iter()
                    .chain(switches.everywhere.disabled.iter())
                {
                    let declared = tools.iter().any(|row| {
                        row.group.as_ref() == Some(&switches.group) && row.name == *name
                    });
                    let shown = self
                        .unlisted
                        .iter()
                        .any(|(group, shown)| group == &switches.group && shown == name);
                    if !declared && !shown {
                        self.unlisted.push((switches.group.clone(), name.clone()));
                    }
                }
            }
        }
        for (group, name) in &self.unlisted {
            tools.push(ToolRow {
                group: Some(group.clone()),
                name: name.clone(),
                state: "off",
                size: String::new(),
            });
        }
        tools.sort_by(|a, b| a.group.cmp(&b.group).then(a.name.cmp(&b.name)));
        let mut rows = Vec::new();
        let mut last: Option<Option<ToolGroup>> = None;
        for row in tools {
            if last.as_ref() != Some(&row.group) {
                if last.is_some() {
                    rows.push(Entry::Blank);
                }
                last = Some(row.group.clone());
                rows.push(Entry::Heading {
                    group: row.group.clone(),
                });
            }
            rows.push(Entry::Tool(row));
        }
        self.rows = rows;
        let shown = rows_height(&self.frame(ctx.width, None), ctx.height);
        // The selection rests on a row that takes a switch.
        self.list.rest_on(&self.takes(), self.rows.len(), shown);
    }

    /// The frame to draw at `width` columns. `cost` is what a reload's
    /// cache rebuild costs, as the footer says it, when known.
    pub(crate) fn frame(&self, width: usize, cost: Option<&str>) -> Frame {
        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, entry)| match entry {
                Entry::Heading { group } => vec![(heading_of(group), None, Ink::Heading)],
                Entry::Blank => Vec::new(),
                Entry::Tool(row) => self.tool_cells(index, row, width),
            })
            .collect();
        let below = if self.awaiting.is_some() {
            let mut below = vec!["Reading the tools…".to_owned()];
            below.extend(self.said.clone());
            below
        } else {
            self.said.clone()
        };
        let footer = match cost {
            Some(cost) => format!("a change rebuilds the cache ({cost}) on reload"),
            None => "a change rebuilds the cache on reload".to_owned(),
        };
        // With no row that takes a switch there is nothing to focus.
        let list = if self.takes().is_empty() {
            self.list.blur()
        } else {
            self.list
        };
        Frame {
            title: "Tools".to_owned(),
            command: "/tools".to_owned(),
            rows,
            list,
            below,
            field: None,
            footer,
        }
    }

    /// One tool's cells: `<name> · <state> · <size>`, then for a tool
    /// with switches ` · this project ` and its switch and ` · everywhere `
    /// and its switch. The name is cut to the room the rest of the row
    /// leaves, so the switches stay inside `width`.
    fn tool_cells(
        &self,
        index: usize,
        row: &ToolRow,
        width: usize,
    ) -> Vec<(String, Option<Spot>, Ink)> {
        let size = if row.size.is_empty() {
            String::new()
        } else {
            format!(" · {}", row.size)
        };
        let scoped = row.group.is_some();
        let project = " · this project";
        let everywhere = " · everywhere";
        // Each switch follows a column that holds the focus mark, in the
        // place of the space before it.
        let widest = if scoped {
            format!("{project} {WIDEST_SWITCH}{everywhere} {WIDEST_SWITCH}")
        } else {
            String::new()
        };
        let rest = format!(" · {}{size}", row.state);
        let room = width.saturating_sub(GUTTER + text::width(&rest) + text::width(&widest));
        let head = format!("{}{rest}", text::cut(&row.name, room, "…"));
        let Some(group) = &row.group else {
            return vec![(head, None, Ink::Plain)];
        };
        let selected = index == self.list.selected();
        let switch = |scope: SwitchScope, at: usize| {
            let mark = if selected && self.focus == scope {
                ("›".to_owned(), None, Ink::Heading)
            } else {
                (" ".to_owned(), None, Ink::Plain)
            };
            let (word, ink) = if self.on_in_scope(group, &row.name, scope) {
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
        ]
    }
}

#[cfg(test)]
#[path = "tools_view_tests.rs"]
mod tests;
