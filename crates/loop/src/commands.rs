//! The session's `/name` commands (`docs/invocation.md`, "What each command does").

use std::path::Path;

use contract::events::CommandInfo;

use crate::opening;
use crate::prompt::PromptInputs;
use crate::skills;

/// Every `/name` the session runs: its skills and prompt templates, read
/// from the places the opening message reads, from the repository's top
/// level above `workspace`. Reads skill places only, writes nothing and
/// raises no notice: the opening message raises discovery's.
#[must_use]
pub fn commands(inputs: &PromptInputs, workspace: &Path) -> Vec<CommandInfo> {
    let workspace = opening::canonical(workspace);
    let (chain, _) = opening::repo_chain(&workspace);
    let top = chain.first().unwrap_or(&workspace);
    skills::commands(
        &skills::discover(inputs, top).skills,
        &inputs.skills_disabled,
    )
}
