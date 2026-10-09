//! `/skills` through the seam (`docs/tui.md`, "Swapped views"): each
//! person file's own `skills.disabled`, and one locked write of a switch
//! to the project's or the global file, never a repository's
//! (`docs/configuration.md`, "When Fiber writes").

use std::io::Read;
use std::path::Path;

use contract::ErrorCode;
use tui::{ConfigureError, SkillsDisabled, SwitchScope};

use super::{Seam, from_config, from_failure};

/// The text pane reads at most this much of a `SKILL.md`.
const TEXT_LIMIT: usize = 64 * 1024;

/// A failed skill text read as the view shows it: naming the path.
fn read_error(path: &Path, error: &dyn std::fmt::Display) -> ConfigureError {
    ConfigureError {
        code: ErrorCode::IoFailed,
        message: format!("Could not read {}: {error}.", path.display()),
    }
}

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

    /// The text of the `SKILL.md` at `path`: one bounded read of at most
    /// a byte past the limit, cut back to a character boundary, with `…`
    /// appended when cut. Only a character the cap splits is trimmed;
    /// malformed or truncated input at the file's real end is an error.
    pub(super) fn read_skill_text(&self, path: &Path) -> Result<String, ConfigureError> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|error| read_error(path, &error))?
            .take(TEXT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| read_error(path, &error))?;
        let cut = bytes.len() > TEXT_LIMIT;
        if cut {
            bytes.truncate(TEXT_LIMIT);
        }
        match String::from_utf8(bytes) {
            Ok(text) => Ok(if cut { format!("{text}…") } else { text }),
            Err(error) => {
                let utf8 = error.utf8_error();
                if cut && utf8.error_len().is_none() {
                    let mut bytes = error.into_bytes();
                    bytes.truncate(utf8.valid_up_to());
                    if let Ok(text) = String::from_utf8(bytes) {
                        return Ok(format!("{text}…"));
                    }
                }
                Err(read_error(path, &utf8))
            }
        }
    }
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
