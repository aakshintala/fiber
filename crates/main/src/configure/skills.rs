//! `/skills` through the seam (`docs/tui.md`, "Swapped views"): each
//! person file's own `skills.disabled`, and one locked write of a switch
//! to the project's or the global file, never a repository's
//! (`docs/configuration.md`, "When Fiber writes").

use std::path::Path;

use tui::{ConfigureError, SkillsDisabled, SwitchScope};

use super::{Seam, from_config, from_failure};

/// `layer`'s own `skills.disabled`: what a write to that file starts
/// from. Both lists apply, so a file with no list starts from none, never
/// an inherited list.
fn layer_list(config: &config::Config, layer: config::Layer) -> Vec<String> {
    config
        .in_layer("skills.disabled", layer)
        .map(|value| crate::mcp_servers::strings(&value))
        .unwrap_or_default()
}

impl Seam {
    /// Each person file's own `skills.disabled` for `workspace`'s
    /// project. The repository layer is never read.
    pub(super) fn read_skills_disabled(
        &self,
        workspace: &Path,
    ) -> Result<SkillsDisabled, ConfigureError> {
        let config = self.load(workspace)?;
        Ok(SkillsDisabled {
            project: layer_list(&config, config::Layer::Project),
            everywhere: layer_list(&config, config::Layer::Global),
        })
    }

    /// Switches `name` off or on in `scope`'s file: one locked
    /// read-modify-write of one file. `on` false adds the name, `on`
    /// true removes it; the other file is untouched.
    pub(super) fn write_skill_switch(
        &self,
        workspace: &Path,
        name: &str,
        scope: SwitchScope,
        on: bool,
    ) -> Result<(), ConfigureError> {
        let (_, project) = ::cli::project_of(&self.home, workspace).map_err(from_failure)?;
        let layer = match scope {
            SwitchScope::Project => config::Layer::Project,
            SwitchScope::Everywhere => config::Layer::Global,
        };
        let change = if on {
            config::ListChange::Remove
        } else {
            config::ListChange::Add
        };
        config::edit_list(
            &self.home,
            workspace,
            &project,
            layer,
            &[config::ListEdit {
                key: "skills.disabled",
                name,
                change,
                inherited: None,
            }],
        )
        .map_err(from_config)?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
