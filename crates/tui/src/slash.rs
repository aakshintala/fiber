//! The `/` list: built-in commands and the session's `commands` answer in
//! one filtered list (`docs/tui.md`, "Keys" › "Rules", "Slash commands").

use contract::events::CommandInfo;

/// How many rows a completion panel shows at most.
pub(crate) const SHOWN: usize = 8;

/// The built-in commands this terminal runs, in the "Slash commands"
/// table's order: name, description, argument hint.
const BUILT_INS: &[(&str, &str, Option<&str>)] = &[
    ("home", "Goes home.", None),
    ("new", "Goes home with the cursor in the input box.", None),
    ("resume", "Opens home at the session list.", None),
    ("model", "Opens the model picker.", None),
    (
        "thinking",
        "Sets the thinking level, or opens the model picker on it.",
        Some("[<level>]"),
    ),
    (
        "scoped-models",
        "Chooses which models the model picker shows.",
        None,
    ),
    ("tools", "Opens the tools view.", None),
    ("context", "Opens the context breakdown.", None),
    ("usage", "Opens the usage view.", None),
    ("panel", "Shows or hides the panel.", None),
    ("rules", "Opens the standing rules.", None),
    ("settings", "Opens the configuration keys.", None),
    ("keys", "Opens the rebinding screen.", None),
    ("skills", "Opens the skills.", None),
    ("handoff", "Starts a handoff.", Some("[instructions]")),
    ("name", "Names the session.", Some("<text>")),
    ("login", "Logs in.", None),
    (
        "reload",
        "Reloads configuration, MCP servers and extensions.",
        None,
    ),
    ("close", "Stops the session on screen.", None),
    ("quit", "Quits.", None),
    (
        "approvals",
        "Reopens the waiting approvals and questions.",
        None,
    ),
    ("?", "Opens the key map.", None),
    ("help", "Opens the key map.", None),
];

/// The tag of a built-in command's row.
pub(crate) const COMMAND: &str = "command";

/// One row of the `/` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// The name typed after `/`.
    pub(crate) name: String,
    /// A one-line description.
    pub(crate) description: String,
    /// An argument hint, such as `[instructions]`.
    pub(crate) hint: Option<String>,
    /// `command`, `skill`, `template`, or the name of the extension or MCP
    /// server the row comes from.
    pub(crate) tag: String,
}

impl Row {
    /// The row as drawn: `/name hint  description  tag`.
    pub(crate) fn line(&self) -> String {
        let hint = self
            .hint
            .as_ref()
            .map_or_else(String::new, |hint| format!(" {hint}"));
        format!("/{}{hint}  {}  {}", self.name, self.description, self.tag)
    }
}

/// Whether `name` is a built-in command.
pub(crate) fn is_built_in(name: &str) -> bool {
    BUILT_INS.iter().any(|(built_in, _, _)| *built_in == name)
}

/// The list: the built-in commands, then `commands`, the session's
/// `commands` answer, in order, each with the hint and tag it gives. A row
/// named like a built-in is left out: the built-in wins.
pub(crate) fn rows(commands: &[CommandInfo]) -> Vec<Row> {
    let built_ins = BUILT_INS.iter().map(|(name, description, hint)| Row {
        name: (*name).to_owned(),
        description: (*description).to_owned(),
        hint: hint.map(str::to_owned),
        tag: COMMAND.to_owned(),
    });
    let session = commands
        .iter()
        .filter(|command| !is_built_in(&command.name))
        .map(|command| Row {
            name: command.name.clone(),
            description: command.description.clone(),
            hint: command.argument_hint.clone(),
            tag: command.tag.clone(),
        });
    built_ins.chain(session).collect()
}

/// The rows matching `query`, the text after `/`, ignoring case: those
/// whose name starts with it, then those whose name contains it elsewhere,
/// each group in list order.
pub(crate) fn filter<'a>(rows: &'a [Row], query: &str) -> Vec<&'a Row> {
    let query = query.to_lowercase();
    let names: Vec<String> = rows.iter().map(|row| row.name.to_lowercase()).collect();
    let starts = rows
        .iter()
        .zip(&names)
        .filter(|(_, name)| name.starts_with(&query));
    let contains = rows
        .iter()
        .zip(&names)
        .filter(|(_, name)| !name.starts_with(&query) && name.contains(&query));
    starts.chain(contains).map(|(row, _)| row).collect()
}

/// The rows matching `query`, the text after `/`, in `filter` order,
/// owned for the panel's window.
pub(crate) fn filtered(rows: &[Row], query: &str) -> Vec<Row> {
    filter(rows, query).into_iter().cloned().collect()
}

/// The first row shown so that `selected` is in view: rows
/// scroll once the selection passes the last of [`SHOWN`].
pub(crate) fn window_start(selected: usize) -> usize {
    selected.saturating_add(1).saturating_sub(SHOWN)
}

/// The first case-folded occurrence of `query` in `name`, as byte
/// offsets on char boundaries; `None` when the query is empty or the
/// name holds none of it (`docs/tui.md`, "Rules").
pub(crate) fn matched(name: &str, query: &str) -> Option<std::ops::Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let at = name.to_lowercase().find(&query.to_lowercase())?;
    // The fold can move byte offsets across char boundaries, so each
    // end widens to one it can cut at.
    let mut start = at;
    while !name.is_char_boundary(start) {
        start = start.saturating_add(1);
    }
    let mut end = at.saturating_add(query.len()).min(name.len());
    while !name.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    (start < end).then_some(start..end)
}

#[cfg(test)]
#[path = "slash_tests.rs"]
mod tests;
