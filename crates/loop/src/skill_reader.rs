//! Reads one listed skill's body (`docs/tools.md`, "Skills"): the file
//! discovery found for its name now, and the body's revalidated read.
//! A free-standing reader over [`PromptInputs`] and the workspace, with
//! no loop state and no events.

use std::path::{Path, PathBuf};

use contract::skills::{SkillRead, Skills};

use crate::opening;
use crate::prompt::PromptInputs;
use crate::skill_header;
use crate::skills;

/// Reads one listed skill's body (`docs/tools.md`, "Skills").
pub struct SkillReader {
    inputs: PromptInputs,
    workspace: PathBuf,
}

impl SkillReader {
    /// Reads skills listed for `inputs` above `workspace`. The workspace
    /// is canonicalised, as [`crate::commands::skills`] reads it.
    pub fn new(inputs: PromptInputs, workspace: &Path) -> Self {
        Self {
            inputs,
            workspace: opening::canonical(workspace),
        }
    }

    fn top(&self) -> PathBuf {
        let (chain, _) = opening::repo_chain(&self.workspace);
        chain
            .first()
            .cloned()
            .unwrap_or_else(|| self.workspace.clone())
    }
}

impl Skills for SkillReader {
    fn file(&self, name: &str) -> Option<PathBuf> {
        let top = self.top();
        let found = skills::discover(&self.inputs, &top);
        let listed = skills::listing(&found.skills, &self.inputs.skills_disabled);
        listed
            .into_iter()
            .find(|found| found.listed.name == name)
            .map(|found| found.file.clone())
    }

    fn body(&self, name: &str, file: &Path) -> Result<String, SkillRead> {
        let bytes = std::fs::read(file).map_err(|error| {
            SkillRead::Io(format!("Could not read skill {}: {error}.", file.display()))
        })?;
        let text = String::from_utf8_lossy(&bytes);
        let header = skill_header::parse(&text).map_err(|_| SkillRead::Invalid)?;
        if header.name != name || !header.model_invocable {
            return Err(SkillRead::Invalid);
        }
        skill_header::body(&text).ok_or(SkillRead::Invalid)
    }
}

#[cfg(test)]
#[path = "skill_reader_tests.rs"]
mod tests;
