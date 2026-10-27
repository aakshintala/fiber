//! The `/` list: built-in commands and the session's `commands` answer in
//! one filtered list (`docs/tui.md`, "Keys" › "Rules", "Slash commands").

use crate::app::commands::{TABLE, aliases_of};
use contract::events::CommandInfo;

/// How many rows a completion panel shows at most.
pub(crate) const SHOWN: usize = 8;

/// The tag of a built-in command's row.
pub(crate) const COMMAND: &str = "command";

/// One row of the `/` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    /// The name typed after `/`.
    pub(crate) name: String,
    /// Other names for the row, drawn after the name in order: `/exit`
    /// on the quit row, `/keybindings` and `/keymap` on the keys row.
    /// Empty when an extension row takes every name, so only one entry
    /// of a name shows.
    pub(crate) aliases: Vec<String>,
    /// A one-line description.
    pub(crate) description: String,
    /// An argument hint, such as `[instructions]`.
    pub(crate) hint: Option<String>,
    /// `command`, `skill`, `template`, or the name of the extension or MCP
    /// server the row comes from.
    pub(crate) tag: String,
}

impl Row {
    /// The row as drawn: `/name hint  description  tag`, each alias
    /// after the name in order.
    pub(crate) fn line(&self) -> String {
        let hint = self
            .hint
            .as_ref()
            .map_or_else(String::new, |hint| format!(" {hint}"));
        let aliases: String = self
            .aliases
            .iter()
            .map(|alias| format!(" /{alias}"))
            .collect();
        format!(
            "/{}{aliases}{hint}  {}  {}",
            self.name, self.description, self.tag
        )
    }
}

/// Whether `name` is a built-in command: the contract list, which also
/// names the command this terminal does not draw (`rewind`).
pub(crate) fn is_built_in(name: &str) -> bool {
    contract::commands::BUILT_IN_COMMANDS.contains(&name)
}

/// The list: the built-in commands in the command table's order, then
/// `commands`, the session's `commands` answer, in order, each with the
/// hint and tag it gives. An
/// extension row (`extension` set) named like a drawn built-in takes that
/// row's slot, with its own description, hint and tag, and keeps its
/// unclaimed aliases; one named like an alias takes only that alias off
/// its row, leaving the row's other aliases; any other row named like a
/// built-in is left out.
pub(crate) fn rows(commands: &[CommandInfo]) -> Vec<Row> {
    let replacement = |name: &str| {
        commands
            .iter()
            .find(|command| command.name == name && command.extension)
    };
    let built_ins = TABLE.iter().map(|entry| {
        if let Some(command) = replacement(entry.name) {
            Row {
                name: command.name.clone(),
                aliases: aliases_of(entry.name)
                    .iter()
                    .filter(|alias| replacement(alias).is_none())
                    .map(|alias| (*alias).to_owned())
                    .collect(),
                description: command.description.clone(),
                hint: command.argument_hint.clone(),
                tag: command.tag.clone(),
            }
        } else {
            Row {
                name: entry.name.to_owned(),
                // An extension row named like an alias takes that
                // alias only, so only one entry of it shows; the row
                // keeps its other aliases.
                aliases: aliases_of(entry.name)
                    .iter()
                    .filter(|alias| replacement(alias).is_none())
                    .map(|alias| (*alias).to_owned())
                    .collect(),
                description: entry.description.to_owned(),
                hint: entry.hint.map(str::to_owned),
                tag: COMMAND.to_owned(),
            }
        }
    });
    let drawn = |name: &str| TABLE.iter().any(|entry| entry.name == name);
    let session = commands
        .iter()
        .filter(|command| {
            if command.extension {
                !drawn(&command.name)
            } else {
                !is_built_in(&command.name)
            }
        })
        .map(|command| Row {
            name: command.name.clone(),
            aliases: Vec::new(),
            description: command.description.clone(),
            hint: command.argument_hint.clone(),
            tag: command.tag.clone(),
        });
    built_ins.chain(session).collect()
}

/// The names of the extension's commands in the `commands` answer, in
/// answer order.
pub(crate) fn extensions(commands: &[CommandInfo]) -> Vec<String> {
    commands
        .iter()
        .filter(|command| command.extension)
        .map(|command| command.name.clone())
        .collect()
}

/// The rows matching `query`, the text after `/`, ignoring case: those
/// whose name or alias starts with it, then those whose name or alias
/// contains it elsewhere, each group in list order.
pub(crate) fn filter<'a>(rows: &'a [Row], query: &str) -> Vec<&'a Row> {
    let query = query.to_lowercase();
    // 0 starts with, 1 contains elsewhere, 2 no match: one pass ranks
    // each row, then the stable sort keeps list order in each group.
    let rank = |row: &Row| {
        let name = row.name.to_lowercase();
        let starts = name.starts_with(&query)
            || row
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase().starts_with(&query));
        let hits = name.contains(&query)
            || row
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase().contains(&query));
        if starts {
            0
        } else if hits {
            1
        } else {
            2
        }
    };
    let mut ranked: Vec<(u8, &'a Row)> = rows
        .iter()
        .filter_map(|row| {
            let rank = rank(row);
            (rank < 2).then_some((rank, row))
        })
        .collect();
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, row)| row).collect()
}

/// The first row shown so that `selected` is in view: rows
/// scroll once the selection passes the last of [`SHOWN`].
pub(crate) fn window_start(selected: usize) -> usize {
    selected.saturating_add(1).saturating_sub(SHOWN)
}

/// The first case-folded occurrence of `query` in `name`, as byte
/// offsets on char boundaries; `None` when the query is empty or the
/// name holds none of it (`docs/tui.md`, "Rules"). The match comes
/// from the same fold the list filter uses, whole-string lowercase, so
/// the two agree on every input the filter accepts (a final sigma
/// folds contextually, which per-character folding misses). Folded
/// offsets map back by walking the name's characters with the folded
/// length of each prefix ending at a boundary: folding can expand (İ
/// lowercases to two code points) or reshape, so folded offsets are
/// never used on the original directly. Every step is bounded: the
/// edges hold one entry per boundary, and both searches stop at one.
pub(crate) fn matched(name: &str, query: &str) -> Option<std::ops::Range<usize>> {
    let folded = name.to_lowercase();
    let want = query.to_lowercase();
    if want.is_empty() {
        return None;
    }
    let at = folded.find(&want)?;
    let end = at.saturating_add(want.len());
    // Each original boundary with the folded length before it: the
    // prefix grows character by character, so every offset here is a
    // char boundary of the original string.
    let mut edges: Vec<(usize, usize)> = vec![(0, 0)];
    let mut prefix = String::new();
    for (byte, ch) in name.char_indices() {
        prefix.push(ch);
        edges.push((
            byte.saturating_add(ch.len_utf8()),
            prefix.to_lowercase().len(),
        ));
    }
    // The match covers the original characters whose folds it
    // touches: the last boundary at or before its start, and the
    // first at or past its end. Both always exist: the edges open at
    // zero and close at the whole fold's length.
    let start = edges
        .iter()
        .rev()
        .find(|(_, before)| *before <= at)
        .map_or(0, |(byte, _)| *byte);
    let stop = edges
        .iter()
        .find(|(_, before)| *before >= end)
        .map_or(name.len(), |(byte, _)| *byte);
    Some(start..stop)
}

#[cfg(test)]
#[path = "slash_tests.rs"]
mod tests;
