//! The session's `/name` commands and its `skills` answer
//! (`docs/invocation.md`, "What each command does").

use std::collections::BTreeMap;
use std::path::Path;

use contract::ErrorCode;
use contract::events::{CommandInfo, Notice, SkillInfo};

use crate::opening;
use crate::prompt::PromptInputs;
use crate::skill_set::runnable_commands;
use crate::skills as discovery;

/// Every `/name` the session runs, and what shadowing left out: the
/// skills and prompt templates discovery keeps that `skills.disabled` does
/// not switch off, then each MCP server's prompts tagged with its name,
/// then each extension's commands. A skill wins over an MCP prompt and
/// an extension command of the same name from every source, and an
/// extension command wins over an MCP prompt of the same name; between
/// servers the first in name order wins; each loser is left out with a
/// `skill_shadowed` notice naming it and what won, while a name in
/// `skills.disabled` drops its prompt without one
/// (`docs/system-prompt.md`, "Skills").
pub struct Commands {
    /// The rows, skills first in discovery order, then prompts by server
    /// name.
    pub rows: Vec<CommandInfo>,
    /// One `skill_shadowed` notice per prompt a skill or an earlier
    /// server's prompt shadows.
    pub notices: Vec<Notice>,
    /// The runnable discovery rows, keyed by name and whether the model
    /// may load them, for the next turn-start comparison.
    pub runnable: RunnableCommands,
}

/// Skill and prompt-template commands, by name and model invocability.
pub type RunnableCommands = BTreeMap<String, bool>;

/// Every `/name` the session runs: its skills and prompt templates, read
/// from the places the opening message reads, from the repository's top
/// level above `workspace`, then `prompts` after them, then `extensions`
/// after those. `prompts` arrives sorted by server name, so the first in
/// name order wins a shared name (`docs/system-prompt.md`, "Skills").
/// An extension row a skill or template shadows is left out with a
/// `skill_shadowed` notice; an MCP prompt a kept extension row shadows is
/// left out the same way. Reads skill places only, writes nothing; the
/// opening message raises discovery's notices.
#[must_use]
pub fn commands(
    inputs: &PromptInputs,
    workspace: &Path,
    prompts: &[CommandInfo],
    extensions: &[CommandInfo],
) -> Commands {
    let workspace = opening::canonical(workspace);
    let (chain, _) = opening::repo_chain(&workspace);
    let top = chain.first().unwrap_or(&workspace);
    let found = discovery::discover(inputs, top);
    commands_from_found(inputs, &found.skills, prompts, extensions)
}

/// Builds the complete command answer from one discovery's winners,
/// without reading skill or template files again. Used when a check or
/// opening already owns the snapshot that changed the runnable set.
pub(crate) fn commands_from_found(
    inputs: &PromptInputs,
    found: &[discovery::Found],
    prompts: &[CommandInfo],
    extensions: &[CommandInfo],
) -> Commands {
    let mut rows = discovery::commands(found, &inputs.skills_disabled);
    let mut notices = Vec::new();
    let skills_end = rows.len();
    let mut kept: Vec<CommandInfo> = Vec::new();
    for ext in extensions {
        if let Some(skill) = found
            .iter()
            .find(|found| found.listed.name == ext.name)
            .filter(|_| rows.iter().take(skills_end).any(|row| row.name == ext.name))
        {
            notices.push(Notice {
                code: ErrorCode::SkillShadowed,
                message: format!(
                    "The extension `{}`'s command `/{}` is shadowed by {}, which is used.",
                    ext.tag, ext.name, skill.listed.path
                ),
                extension: Some(ext.tag.clone()),
            });
            continue;
        }
        kept.push(ext.clone());
    }
    for row in prompts {
        if inputs.skills_disabled.iter().any(|off| off == &row.name) {
            continue;
        }
        if let Some(skill) = found.iter().find(|found| found.listed.name == row.name) {
            notices.push(Notice {
                code: ErrorCode::SkillShadowed,
                message: format!(
                    "The MCP server `{}`'s prompt `/{}` is shadowed by {}, which is used.",
                    row.tag, row.name, skill.listed.path
                ),
                extension: None,
            });
            continue;
        }
        if let Some(kept_ext) = kept.iter().find(|kept| kept.name == row.name) {
            notices.push(Notice {
                code: ErrorCode::SkillShadowed,
                message: format!(
                    "The MCP server `{}`'s prompt `/{}` is shadowed by the extension `{}`'s command, which is used.",
                    row.tag, row.name, kept_ext.tag
                ),
                extension: Some(kept_ext.tag.clone()),
            });
            continue;
        }
        if let Some(winner) = rows
            .iter()
            .skip(skills_end)
            .find(|kept| kept.name == row.name)
        {
            let winner = winner.tag.clone();
            notices.push(Notice {
                code: ErrorCode::SkillShadowed,
                message: format!(
                    "The MCP server `{}`'s prompt `/{}` is shadowed by the MCP server `{winner}`, which is used.",
                    row.tag, row.name
                ),
                extension: None,
            });
            continue;
        }
        rows.push(row.clone());
    }
    rows.extend(kept);
    Commands {
        rows,
        notices,
        runnable: runnable_commands(found, &inputs.skills_disabled),
    }
}

/// Every skill discovery read, switched-off and shadowed ones included
/// and marked: the `skills` answer's rows, from the repository's top
/// level above `workspace`, as [`commands`] reads it. Reads skill places
/// only, writes nothing; the opening message raises discovery's notices.
#[must_use]
pub fn skills(inputs: &PromptInputs, workspace: &Path) -> Vec<SkillInfo> {
    let workspace = opening::canonical(workspace);
    let (chain, _) = opening::repo_chain(&workspace);
    let top = chain.first().unwrap_or(&workspace);
    let found = discovery::discover(inputs, top);
    discovery::rows(&found, &inputs.skills_disabled)
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
