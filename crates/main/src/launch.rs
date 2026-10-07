//! Builds the terminal's launch description: the launch directory, its
//! project key, whether it is inside a git repository, and the terminal's
//! own settings (`docs/tui.md`, "Home"). Pure: the identity path, which
//! took the process, is computed once in `main` and passed in, so no test
//! runs a child process.

use std::path::{Path, PathBuf};

use config::Config;

/// Builds the terminal's launch description from the launch directory,
/// its identity path, and the loaded configuration.
pub(crate) fn launch(workspace: PathBuf, identity: &Path, config: &Config) -> tui::Launch {
    // The project key names the identity path: git's shared directory
    // inside a repository, else the launch directory itself.
    let project = log::project_key(identity);
    // Inside a repository the identity is git's shared directory, never
    // the launch directory itself.
    let canonical = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.clone());
    let git = canonical.as_path() != identity;
    // `tui.hover`, defaulting to on (`docs/configuration.md`, "Keys").
    let hover = config
        .get("tui.hover", None)
        .and_then(|(value, _)| value.as_bool())
        .unwrap_or(true);
    // `model` and `thinking`, unset for the chips' defaults
    // (`docs/configuration.md`).
    let model = config
        .get("model", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    let thinking = config
        .get("thinking", None)
        .and_then(|(value, _)| value.as_str().map(str::to_owned));
    tui::Launch {
        workspace,
        project,
        git,
        hover,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        model,
        thinking,
        // `tui.logo_glyph`, defaulting to ⌇ (`docs/configuration.md`).
        logo_glyph: config
            .get("tui.logo_glyph", None)
            .and_then(|(value, _)| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "⌇".to_owned()),
    }
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
