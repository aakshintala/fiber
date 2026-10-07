//! Tests for the launch description: pure, over invented identity paths
//! in a temporary directory, so no test runs a child process.

use super::launch;
use config::{Config, Sources};

/// Loads the configuration for `home` and `workspace`, with `overrides`
/// as `-c key=value` reads them.
fn config(home: &std::path::Path, workspace: &std::path::Path, overrides: Vec<String>) -> Config {
    let project = config::ProjectKey::new("-w").unwrap_or_else(|err| panic!("key: {err}"));
    Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project,
        overrides,
    })
    .unwrap_or_else(|err| panic!("config: {err}"))
}

#[test]
fn an_identity_equal_to_the_workspace_is_not_git() {
    let dir = fakes::TempDir::new("fiber-launch");
    let workspace = dir.path().to_path_buf();
    // The identity path is canonical, as `doors::project` makes it.
    let identity = workspace
        .canonicalize()
        .unwrap_or_else(|err| panic!("canonical: {err}"));
    let config = config(dir.path(), &workspace, Vec::new());
    let launch = launch(workspace.clone(), &identity, &config);
    assert_eq!(launch.workspace, workspace);
    assert_eq!(launch.project, log::project_key(&identity));
    assert!(!launch.git);
    assert!(launch.hover);
    assert_eq!(launch.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn an_identity_elsewhere_is_git_and_names_the_project() {
    let dir = fakes::TempDir::new("fiber-launch-git");
    let workspace = dir.path().join("repo");
    std::fs::create_dir_all(workspace.join(".git")).unwrap_or_else(|err| panic!("mkdir: {err}"));
    let identity = workspace.join(".git");
    let config = config(dir.path(), &workspace, Vec::new());
    let launch = launch(workspace.clone(), &identity, &config);
    assert_eq!(launch.workspace, workspace);
    assert_eq!(launch.project, log::project_key(&identity));
    assert!(launch.git);
}

#[test]
fn tui_hover_defaults_to_on_and_reads_off() {
    let dir = fakes::TempDir::new("fiber-launch-hover");
    let workspace = dir.path().to_path_buf();
    let on = config(dir.path(), &workspace, Vec::new());
    assert!(launch(workspace.clone(), &workspace, &on).hover);
    let off = config(dir.path(), &workspace, vec!["tui.hover=false".to_owned()]);
    assert!(!launch(workspace.clone(), &workspace, &off).hover);
}
