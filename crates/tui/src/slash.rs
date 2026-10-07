//! The `/` list: built-in commands and skills in one filtered list
//! (`docs/tui.md`, "Keys" › "Rules", "Slash commands").

use contract::events::SkillListed;

/// How many rows a completion panel shows at most.
pub(crate) const SHOWN: usize = 8;

/// The built-in commands this terminal runs, in the "Slash commands"
/// table's order: name, description, argument hint.
const BUILT_INS: &[(&str, &str, Option<&str>)] = &[
    ("home", "Goes home.", None),
    ("new", "Goes home with the cursor in the input box.", None),
    ("handoff", "Starts a handoff.", Some("[instructions]")),
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

/// The tag of a skill's row.
pub(crate) const SKILL: &str = "skill";

/// One row of the `/` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// The name typed after `/`.
    pub(crate) name: String,
    /// A one-line description.
    pub(crate) description: String,
    /// An argument hint, such as `[instructions]`.
    pub(crate) hint: Option<String>,
    /// `command`, `skill`, or the name of the extension or MCP server the
    /// row comes from.
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

/// The list: the built-in commands, then `skills` in order. A skill named
/// like a built-in is left out: the built-in wins.
pub(crate) fn rows(skills: &[SkillListed]) -> Vec<Row> {
    let built_ins = BUILT_INS.iter().map(|(name, description, hint)| Row {
        name: (*name).to_owned(),
        description: (*description).to_owned(),
        hint: hint.map(str::to_owned),
        tag: COMMAND.to_owned(),
    });
    let skills = skills
        .iter()
        .filter(|skill| !is_built_in(&skill.name))
        .map(|skill| Row {
            name: skill.name.clone(),
            description: skill.description.clone(),
            hint: None,
            tag: SKILL.to_owned(),
        });
    built_ins.chain(skills).collect()
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

/// The first row shown so that `selected` is in view: rows
/// scroll once the selection passes the last of [`SHOWN`].
pub(crate) fn window_start(selected: usize) -> usize {
    selected.saturating_add(1).saturating_sub(SHOWN)
}

#[cfg(test)]
#[path = "slash_tests.rs"]
mod tests;
