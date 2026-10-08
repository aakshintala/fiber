//! The session's `/name` commands (`docs/invocation.md`, "What each command does").

use std::path::Path;

use contract::ErrorCode;
use contract::events::{CommandInfo, Notice};

use crate::opening;
use crate::prompt::PromptInputs;
use crate::skills;

/// Every `/name` the session runs, and what shadowing left out: the
/// skills and prompt templates discovery keeps that `skills.disabled` does
/// not switch off, then each MCP server's prompts tagged with its name. A
/// skill wins over an MCP prompt of the same name from every source, and
/// between servers the first in name order wins; each loser is left out
/// with a `skill_shadowed` notice naming it and what won, while a name in
/// `skills.disabled` drops its prompt without one
/// (`docs/system-prompt.md`, "Skills").
pub struct Commands {
    /// The rows, skills first in discovery order, then prompts by server
    /// name.
    pub rows: Vec<CommandInfo>,
    /// One `skill_shadowed` notice per prompt a skill or an earlier
    /// server's prompt shadows.
    pub notices: Vec<Notice>,
}

/// Every `/name` the session runs: its skills and prompt templates, read
/// from the places the opening message reads, from the repository's top
/// level above `workspace`, then `prompts` after them. Reads skill places
/// only, writes nothing; the opening message raises discovery's notices.
#[must_use]
pub fn commands(inputs: &PromptInputs, workspace: &Path, prompts: &[CommandInfo]) -> Commands {
    let workspace = opening::canonical(workspace);
    let (chain, _) = opening::repo_chain(&workspace);
    let top = chain.first().unwrap_or(&workspace);
    let found = skills::discover(inputs, top);
    let mut rows = skills::commands(&found.skills, &inputs.skills_disabled);
    let mut notices = Vec::new();
    // Prompt rows sort by server name, so the first in name order wins a
    // shared name (`docs/system-prompt.md`, "Skills").
    let mut ordered: Vec<&CommandInfo> = prompts.iter().collect();
    ordered.sort_by(|left, right| left.tag.cmp(&right.tag));
    let skills_end = rows.len();
    for row in ordered {
        if inputs.skills_disabled.iter().any(|off| off == &row.name) {
            continue;
        }
        if let Some(skill) = found
            .skills
            .iter()
            .find(|found| found.listed.name == row.name)
        {
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
    Commands { rows, notices }
}

#[cfg(test)]
#[path = "commands_tests.rs"]
mod tests;
