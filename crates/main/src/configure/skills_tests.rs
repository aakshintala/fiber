//! Tests for `/skills` through the seam: each person file's own
//! `skills.disabled` and the file each switch writes (`docs/tui.md`,
//! "Swapped views"; `docs/configuration.md`, "When Fiber writes").

use std::fs;

use serde_json::{Value, json};
use tui::{Configure, SwitchScope};

use super::Seam;
use crate::configure::tests::{Dirs, write};

/// The seam's `skills.disabled` for `workspace`.
fn disabled(seam: &Seam, workspace: &std::path::Path) -> tui::SkillsDisabled {
    seam.skills_disabled(workspace)
        .unwrap_or_else(|e| panic!("disabled: {e}"))
}

fn read(file: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap_or_else(|e| panic!("read: {e}")))
        .unwrap_or_else(|e| panic!("json: {e}"))
}

#[test]
fn skills_disabled_reads_each_layer_alone() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    write(
        &dirs.project_file(&workspace),
        r#"{"skills": {"disabled": ["b"]}}"#,
    );
    write(
        &workspace.join(".fiber/config.json"),
        r#"{"skills": {"disabled": ["c"]}}"#,
    );
    let seam = Seam::new(dirs.home());
    let found = disabled(&seam, &workspace);
    assert_eq!(found.everywhere, vec!["a".to_owned()]);
    assert_eq!(found.project, vec!["b".to_owned()]);
}

#[test]
fn no_file_reads_two_empty_lists() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    let found = disabled(&seam, &workspace);
    assert!(found.everywhere.is_empty());
    assert!(found.project.is_empty());
}

#[test]
fn skills_disabled_reads_the_session_workspaces_project() {
    let dirs = Dirs::new();
    let (one, two) = (dirs.workspace("one"), dirs.workspace("two"));
    assert_ne!(dirs.project_file(&one), dirs.project_file(&two));
    write(
        &dirs.project_file(&one),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    write(
        &dirs.project_file(&two),
        r#"{"skills": {"disabled": ["b"]}}"#,
    );
    let seam = Seam::new(dirs.home());
    assert_eq!(disabled(&seam, &one).project, vec!["a".to_owned()]);
    assert_eq!(disabled(&seam, &two).project, vec!["b".to_owned()]);
}

#[test]
fn switching_off_in_this_project_writes_only_the_project_file() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let global = dirs.home().join("config.json");
    write(&global, r#"{"skills": {"disabled": ["a"]}}"#);
    let before = fs::read(&global).unwrap_or_else(|e| panic!("read: {e}"));
    let seam = Seam::new(dirs.home());
    seam.switch_skill(&workspace, "b", SwitchScope::Project, false)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.project_file(&workspace))["skills"]["disabled"],
        json!(["b"])
    );
    assert_eq!(
        fs::read(&global).unwrap_or_else(|e| panic!("read: {e}")),
        before
    );
}

#[test]
fn switching_off_everywhere_writes_only_the_global_file() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    seam.switch_skill(&workspace, "a", SwitchScope::Everywhere, false)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.home().join("config.json"))["skills"]["disabled"],
        json!(["a"])
    );
    assert!(!dirs.project_file(&workspace).exists());
}

#[test]
fn switching_on_removes_the_name_from_that_layer_only() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    write(
        &dirs.project_file(&workspace),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.switch_skill(&workspace, "a", SwitchScope::Project, true)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.project_file(&workspace))["skills"]["disabled"],
        json!([])
    );
    assert_eq!(
        read(&dirs.home().join("config.json"))["skills"]["disabled"],
        json!(["a"])
    );
}

#[test]
fn switching_never_writes_the_repository_file() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let repository = workspace.join(".fiber/config.json");
    write(&repository, r#"{"skills": {"disabled": ["c"]}}"#);
    let before = fs::read(&repository).unwrap_or_else(|e| panic!("read: {e}"));
    let seam = Seam::new(dirs.home());
    for scope in [SwitchScope::Project, SwitchScope::Everywhere] {
        seam.switch_skill(&workspace, "a", scope, false)
            .unwrap_or_else(|e| panic!("switch: {e}"));
    }
    assert_eq!(
        fs::read(&repository).unwrap_or_else(|e| panic!("read: {e}")),
        before
    );
    let found = disabled(&seam, &workspace);
    assert_eq!(found.project, vec!["a".to_owned()]);
    assert_eq!(found.everywhere, vec!["a".to_owned()]);
}

#[test]
fn a_second_off_leaves_the_file_as_it_was() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    for _ in 0..2 {
        seam.switch_skill(&workspace, "a", SwitchScope::Project, false)
            .unwrap_or_else(|e| panic!("switch: {e}"));
    }
    assert_eq!(
        read(&dirs.project_file(&workspace))["skills"]["disabled"],
        json!(["a"])
    );
}

#[test]
fn a_file_with_no_list_starts_from_none() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"skills": {"disabled": ["a"]}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.switch_skill(&workspace, "b", SwitchScope::Project, false)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.project_file(&workspace))["skills"]["disabled"],
        json!(["b"])
    );
}
