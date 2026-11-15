//! The layout of Fiber home (`docs/state.md`, "What each part holds"):
//! the directories extensions live in, extension data lives in, caches
//! live in, and crash reports live in. Only `config` knows these paths
//! (`docs/architecture.md`, "The call rules"); callers ask for them here
//! instead of joining the segments themselves.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{ConfigError, ProjectKey};

/// Ensures layout version 2 and lazily migrates this identity's legacy project directory.
pub fn ensure_project_layout(home: &Path, identity: &Path) -> Result<(), ConfigError> {
    ensure_project_layout_with(home, identity, support::atomic::rename_noreplace)
}

fn ensure_project_layout_with(
    home: &Path,
    identity: &Path,
    rename_noreplace: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), ConfigError> {
    let layout = home.join("layout");
    match fs::read(&layout) {
        Ok(bytes) if bytes == b"2" => {}
        Ok(_) => {
            return Err(io_error(
                &layout,
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unsupported Fiber home layout",
                ),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            support::atomic::write_atomic(&layout, b"2", 0o600)
                .map_err(|source| io_error(&layout, source))?;
        }
        Err(source) => return Err(io_error(&layout, source)),
    }

    let projects = projects_dir(home);
    match fs::symlink_metadata(&projects) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error(&projects, source)),
        Ok(_) => {}
    }
    let legacy = projects.join(support::project_key::legacy_project_slug(identity));
    let target = projects.join(support::project_key::project_key(identity));
    match fs::symlink_metadata(&legacy) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error(&legacy, source)),
    }

    let marker = legacy.join("legacy-key");
    let marker_exists = match fs::symlink_metadata(&marker) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let bytes = fs::read(&marker).map_err(|source| io_error(&marker, source))?;
            if bytes != support::project_key::legacy_project_slug(identity).as_bytes() {
                return Ok(());
            }
            true
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(source) => return Err(io_error(&marker, source)),
    };
    if plain_dir_exists(&target) {
        return Ok(());
    }
    if !marker_exists {
        support::atomic::write_atomic(
            &marker,
            support::project_key::legacy_project_slug(identity).as_bytes(),
            0o600,
        )
        .map_err(|source| io_error(&marker, source))?;
    }
    rename_project_dir(&legacy, &target, rename_noreplace)
        .map_err(|source| io_error(&legacy, source))
}

fn rename_project_dir(
    legacy: &Path,
    target: &Path,
    rename_noreplace: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match rename_noreplace(legacy, target) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::Unsupported => {
            if fs::symlink_metadata(target).is_ok() {
                return if plain_dir_exists(target) {
                    Ok(())
                } else {
                    Err(std::io::ErrorKind::AlreadyExists.into())
                };
            }
            match fs::rename(legacy, target) {
                Err(source) if rename_race(&source) => {
                    if plain_dir_exists(target) {
                        Ok(())
                    } else {
                        Err(source)
                    }
                }
                result => result,
            }
        }
        Err(source) if rename_race(&source) => {
            if plain_dir_exists(target) {
                Ok(())
            } else {
                Err(source)
            }
        }
        Err(source) => Err(source),
    }
}

fn rename_race(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::AlreadyExists
            | std::io::ErrorKind::DirectoryNotEmpty
            | std::io::ErrorKind::NotFound
    )
}

fn io_error(file: &Path, source: std::io::Error) -> ConfigError {
    ConfigError::Io {
        file: file.to_path_buf(),
        source,
    }
}

fn plain_dir_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

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

    use std::fs;
    use std::path::PathBuf;

    use super::{
        ProjectKey, ensure_project_layout, ensure_project_layout_with, project_protected_paths_dir,
        project_worktrees_dir, projects_dir,
    };

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

    #[test]
    fn layout_marker_missing_is_v1() {
        let home = fakes::TempDir::new("config-layout-v1");
        ensure_project_layout(home.path(), PathBuf::from("/none").as_path()).unwrap();
        assert_eq!(fs::read(home.path().join("layout")).unwrap(), b"2");
    }

    #[test]
    fn layout_marker_v2() {
        let home = fakes::TempDir::new("config-layout-v2");
        fs::write(home.path().join("layout"), b"2").unwrap();
        ensure_project_layout(home.path(), PathBuf::from("/none").as_path()).unwrap();
        assert_eq!(fs::read(home.path().join("layout")).unwrap(), b"2");
    }

    #[test]
    fn layout_marker_without_project_state_keeps_projects_absent() {
        let home = fakes::TempDir::new("config-layout-no-project");
        ensure_project_layout(home.path(), PathBuf::from("/none").as_path()).unwrap();
        assert_eq!(fs::read(home.path().join("layout")).unwrap(), b"2");
        assert!(!projects_dir(home.path()).exists());
    }

    #[test]
    fn layout_marker_rejects_unknown_bytes_without_rewriting_them() {
        let home = fakes::TempDir::new("config-layout-unknown-marker");
        fs::write(home.path().join("layout"), b"1").unwrap();
        let error =
            ensure_project_layout(home.path(), PathBuf::from("/a-b").as_path()).unwrap_err();
        assert!(
            matches!(error, super::ConfigError::Io { source, .. } if source.kind() == std::io::ErrorKind::InvalidData)
        );
        assert_eq!(fs::read(home.path().join("layout")).unwrap(), b"1");
    }

    #[test]
    fn projects_entry_must_be_a_directory() {
        let home = fakes::TempDir::new("config-layout-projects-file");
        fs::write(projects_dir(home.path()), b"not a directory").unwrap();
        assert!(matches!(
            ensure_project_layout(home.path(), PathBuf::from("/a-b").as_path()),
            Err(super::ConfigError::Io { .. })
        ));
    }

    #[test]
    fn legacy_project_migration_marker() {
        let home = fakes::TempDir::new("config-layout-marker");
        let identity = PathBuf::from("/a-b");
        let legacy = projects_dir(home.path()).join("-a-b");
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("config.json"), b"legacy").unwrap();
        ensure_project_layout(home.path(), &identity).unwrap();
        let target = projects_dir(home.path()).join(support::project_key::project_key(&identity));
        assert_eq!(fs::read(target.join("legacy-key")).unwrap(), b"-a-b");
        assert_eq!(fs::read(target.join("config.json")).unwrap(), b"legacy");
        assert!(!legacy.exists());
    }

    #[test]
    fn legacy_project_migration_resumes_marked_source() {
        let home = fakes::TempDir::new("config-layout-resume-marked");
        let identity = PathBuf::from("/a-b");
        let legacy = projects_dir(home.path()).join("-a-b");
        let target = projects_dir(home.path()).join(support::project_key::project_key(&identity));
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("legacy-key"), b"-a-b").unwrap();
        fs::write(legacy.join("config.json"), b"legacy").unwrap();

        ensure_project_layout(home.path(), &identity).unwrap();

        assert!(!legacy.exists());
        assert_eq!(fs::read(target.join("legacy-key")).unwrap(), b"-a-b");
        assert_eq!(fs::read(target.join("config.json")).unwrap(), b"legacy");
    }

    #[test]
    fn legacy_project_migration_refuses_malformed_marker() {
        let identity = PathBuf::from("/a-b");
        for marker in [b"wrong".as_slice(), b"".as_slice()] {
            let home = fakes::TempDir::new("config-layout-malformed-marker");
            let legacy = projects_dir(home.path()).join("-a-b");
            let target =
                projects_dir(home.path()).join(support::project_key::project_key(&identity));
            fs::create_dir_all(&legacy).unwrap();
            fs::write(legacy.join("legacy-key"), marker).unwrap();
            fs::write(legacy.join("config.json"), b"legacy").unwrap();

            ensure_project_layout(home.path(), &identity).unwrap();

            assert!(legacy.is_dir());
            assert!(!target.exists());
            assert_eq!(fs::read(legacy.join("config.json")).unwrap(), b"legacy");
        }
        let home = fakes::TempDir::new("config-layout-marker-not-file");
        let legacy = projects_dir(home.path()).join("-a-b");
        let target = projects_dir(home.path()).join(support::project_key::project_key(&identity));
        fs::create_dir_all(legacy.join("legacy-key")).unwrap();
        ensure_project_layout(home.path(), &identity).unwrap();
        assert!(legacy.is_dir());
        assert!(!target.exists());
    }

    #[test]
    fn legacy_project_migration_noreplace_unsupported() {
        let identity = PathBuf::from("/a-b");
        let home = fakes::TempDir::new("config-layout-unsupported-rename");
        let legacy = projects_dir(home.path()).join("-a-b");
        let target = projects_dir(home.path()).join(support::project_key::project_key(&identity));
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("config.json"), b"legacy").unwrap();
        ensure_project_layout_with(home.path(), &identity, |_, _| {
            Err(std::io::ErrorKind::Unsupported.into())
        })
        .unwrap();
        assert!(!legacy.exists());
        assert_eq!(fs::read(target.join("config.json")).unwrap(), b"legacy");

        let home = fakes::TempDir::new("config-layout-unsupported-target");
        let legacy = projects_dir(home.path()).join("-a-b");
        let target = projects_dir(home.path()).join(support::project_key::project_key(&identity));
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("config.json"), b"legacy").unwrap();
        ensure_project_layout_with(home.path(), &identity, |_, target| {
            fs::create_dir(target)?;
            fs::write(target.join("config.json"), b"target")?;
            Err(std::io::ErrorKind::Unsupported.into())
        })
        .unwrap();
        assert!(legacy.is_dir());
        assert_eq!(fs::read(legacy.join("config.json")).unwrap(), b"legacy");
        assert_eq!(fs::read(target.join("config.json")).unwrap(), b"target");
    }

    #[test]
    fn legacy_project_migration_handles_racing_target_and_preserves_both_trees() {
        let identity = PathBuf::from("/a-b");
        let home = fakes::TempDir::new("config-layout-racing-target");
        let legacy = projects_dir(home.path()).join("-a-b");
        let target = projects_dir(home.path()).join(support::project_key::project_key(&identity));
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("data"), b"source").unwrap();
        let result = ensure_project_layout_with(home.path(), &identity, |_, target_path| {
            fs::create_dir(target_path)?;
            fs::write(target_path.join("data"), b"target")?;
            Err(std::io::ErrorKind::AlreadyExists.into())
        });
        result.unwrap();
        assert_eq!(fs::read(legacy.join("data")).unwrap(), b"source");
        assert_eq!(fs::read(target.join("data")).unwrap(), b"target");
        assert_eq!(fs::read(legacy.join("legacy-key")).unwrap(), b"-a-b");
    }

    #[test]
    fn legacy_project_migration_keeps_existing_target() {
        let home = fakes::TempDir::new("config-layout-existing-target");
        let identity = PathBuf::from("/a-b");
        let legacy = projects_dir(home.path()).join("-a-b");
        let target = projects_dir(home.path()).join(support::project_key::project_key(&identity));
        fs::create_dir_all(&legacy).unwrap();
        fs::write(legacy.join("data"), b"source").unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("data"), b"target").unwrap();
        ensure_project_layout(home.path(), &identity).unwrap();
        assert_eq!(fs::read(legacy.join("data")).unwrap(), b"source");
        assert_eq!(fs::read(target.join("data")).unwrap(), b"target");
    }
}
