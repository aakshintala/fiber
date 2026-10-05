//! Tests for `prompt_files`: precedence of the person's system prompt files.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::{append, system};

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
fn project_file_wins_and_files_are_not_combined() {
    let (home, project) = home();
    write(&home.path().join("SYSTEM.md"), "global");
    write(
        &home.path().join("projects").join("proj").join("SYSTEM.md"),
        "project",
    );
    assert_eq!(system(home.path(), &project).as_deref(), Some("project"));
}

#[test]
fn global_file_applies_without_a_project_file() {
    let (home, project) = home();
    write(&home.path().join("APPEND_SYSTEM.md"), "global appendix");
    assert_eq!(
        append(home.path(), &project).as_deref(),
        Some("global appendix")
    );
}

#[test]
fn empty_file_is_absent() {
    let (home, project) = home();
    write(&home.path().join("SYSTEM.md"), "   \n");
    assert_eq!(system(home.path(), &project), None);
}

#[test]
fn empty_project_file_falls_back_to_the_global_one() {
    let (home, project) = home();
    write(&home.path().join("SYSTEM.md"), "global");
    write(
        &home.path().join("projects").join("proj").join("SYSTEM.md"),
        "  \n",
    );
    assert_eq!(system(home.path(), &project).as_deref(), Some("global"));
    write(&home.path().join("APPEND_SYSTEM.md"), "global appendix");
    write(
        &home
            .path()
            .join("projects")
            .join("proj")
            .join("APPEND_SYSTEM.md"),
        "\n",
    );
    assert_eq!(
        append(home.path(), &project).as_deref(),
        Some("global appendix")
    );
}

#[test]
fn missing_files_are_absent() {
    let (home, project) = home();
    assert_eq!(system(home.path(), &project), None);
    assert_eq!(append(home.path(), &project), None);
}
