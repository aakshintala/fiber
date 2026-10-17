//! Tests for `prompt_files`: wiring to `config` and `agents_home`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::{agents_home, append, system};

fn home() -> (fakes::TempDir, config::ProjectKey) {
    let home = fakes::TempDir::new("fiber-prompt-files");
    let project = config::ProjectKey::new("proj").unwrap();
    std::fs::create_dir_all(home.path().join("projects").join("proj")).unwrap();
    (home, project)
}

fn write(path: &std::path::Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn a_project_file_reaches_system() {
    let (home, project) = home();
    write(
        &home.path().join("projects").join("proj").join("SYSTEM.md"),
        "project",
    );
    assert_eq!(system(home.path(), &project).as_deref(), Some("project"));
}

#[test]
fn a_project_file_reaches_append() {
    let (home, project) = home();
    write(
        &home
            .path()
            .join("projects")
            .join("proj")
            .join("APPEND_SYSTEM.md"),
        "appendix",
    );
    assert_eq!(append(home.path(), &project).as_deref(), Some("appendix"));
}

#[test]
fn the_agents_home_is_an_absolute_home_value() {
    assert_eq!(
        agents_home(Some("/home/alice".into())),
        Some(std::path::PathBuf::from("/home/alice"))
    );
    assert_eq!(agents_home(None), None);
    assert_eq!(agents_home(Some("".into())), None);
    assert_eq!(agents_home(Some("relative/home".into())), None);
}
