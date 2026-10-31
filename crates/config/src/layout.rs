//! The layout of Fiber home (`docs/state.md`, "What each part holds"):
//! the directories extensions live in, extension data lives in, caches
//! live in, and crash reports live in. Only `config` knows these paths
//! (`docs/architecture.md`, "The call rules"); callers ask for them here
//! instead of joining the segments themselves.

use std::path::{Path, PathBuf};

use crate::ProjectKey;

/// `<home>/extensions`: where extensions are installed.
pub fn extensions_dir(home: &Path) -> PathBuf {
    home.join("extensions")
}

/// `<home>/pinned`: the approved copies of code a repository ships,
/// one `pinned/<hash>/` directory each.
pub fn pinned_dir(home: &Path) -> PathBuf {
    home.join("pinned")
}

/// `<home>/approvals`: the per-machine approvals of code a repository
/// ships, one file per MCP server decision.
pub fn machine_approvals_dir(home: &Path) -> PathBuf {
    home.join("approvals")
}

/// `<layer>/data/<slug>`: one extension's data directory. `layer` is Fiber
/// home for machine-wide data or `projects/<key>/` for per-project data;
/// `slug` is already `crate::dir_name(..)`.
pub fn extension_data_dir(layer: &Path, slug: &str) -> PathBuf {
    layer.join("data").join(slug)
}

/// `<home>/projects`: every project's directory, one `<key>/` each.
pub fn projects_dir(home: &Path) -> PathBuf {
    home.join("projects")
}

/// `<home>/projects/<key>/approvals`: one project's approvals of code a
/// repository ships, one file per extension or hook decision.
pub fn project_approvals_dir(home: &Path, project: &ProjectKey) -> PathBuf {
    home.join("projects")
        .join(project.as_str())
        .join("approvals")
}

/// `<home>/projects/<key>/servers`: one project's records of the MCP
/// server copies it approved, one file per content hash, mirroring the
/// per-machine approval.
pub fn project_servers_dir(home: &Path, project: &ProjectKey) -> PathBuf {
    home.join("projects").join(project.as_str()).join("servers")
}

/// `<home>/projects/<key>/data/<slug>`: one extension's per-project data
/// directory.
pub fn project_data_dir(home: &Path, project: &ProjectKey, slug: &str) -> PathBuf {
    extension_data_dir(&home.join("projects").join(project.as_str()), slug)
}

/// `<home>/projects/<key>/worktrees`: one project's checked-out worktrees.
pub fn project_worktrees_dir(home: &Path, project: &ProjectKey) -> PathBuf {
    projects_dir(home).join(project.as_str()).join("worktrees")
}

/// `<home>/projects/<key>/protected-paths`: one project's baseline and locks.
pub fn project_protected_paths_dir(home: &Path, project: &ProjectKey) -> PathBuf {
    projects_dir(home)
        .join(project.as_str())
        .join("protected-paths")
}

/// `<home>/cache/mcp`: where MCP server output is cached.
pub fn mcp_cache_dir(home: &Path) -> PathBuf {
    home.join("cache").join("mcp")
}

/// `<home>/cache/images`: where image attachments are cached.
pub fn images_cache_dir(home: &Path) -> PathBuf {
    home.join("cache").join("images")
}

/// `<home>/crashes`: where crash reports are written.
pub fn crashes_dir(home: &Path) -> PathBuf {
    home.join("crashes")
}

/// `<home>/config.json`: the global configuration file.
pub fn global_config_file(home: &Path) -> PathBuf {
    home.join("config.json")
}

/// `<home>/projects/<key>/config.json`: one project's configuration file.
pub fn project_config_file(home: &Path, project: &ProjectKey) -> PathBuf {
    home.join("projects")
        .join(project.as_str())
        .join("config.json")
}

/// `<workspace>/.fiber/config.json`: the repository configuration file.
pub fn repository_config_file(workspace: &Path) -> PathBuf {
    workspace.join(".fiber").join("config.json")
}

/// `<home>/logs`: where diagnostic logs are written. Never creates it.
pub fn logs_dir(home: &Path) -> PathBuf {
    home.join("logs")
}

/// `<home>/docs`: where the user guide is installed. Never creates it.
pub fn docs_dir(home: &Path) -> PathBuf {
    home.join("docs")
}

/// `<home>/run`: where running sessions listen. Never creates it.
pub fn run_dir(home: &Path) -> PathBuf {
    home.join("run")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "a failure is the test's")]

    use std::path::PathBuf;

    use super::{ProjectKey, project_protected_paths_dir, project_worktrees_dir, projects_dir};

    #[test]
    fn projects_dir_is_projects_under_home() {
        assert_eq!(
            projects_dir(&PathBuf::from("/h")),
            PathBuf::from("/h/projects")
        );
    }

    #[test]
    fn project_worktrees_dir_is_worktrees_under_the_project() {
        let project = ProjectKey::new("proj").unwrap();
        assert_eq!(
            project_worktrees_dir(&PathBuf::from("/h"), &project),
            PathBuf::from("/h/projects/proj/worktrees")
        );
    }

    #[test]
    fn project_protected_paths_dir_is_under_the_project() {
        let project = ProjectKey::new("proj").unwrap();
        assert_eq!(
            project_protected_paths_dir(&PathBuf::from("/h"), &project),
            PathBuf::from("/h/projects/proj/protected-paths")
        );
    }
}
