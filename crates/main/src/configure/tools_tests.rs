//! Tests for `/tools` through the seam: the groups it lists and the file
//! each switch writes (`docs/tui.md`, "Swapped views";
//! `docs/configuration.md`, "When Fiber writes").

use std::fs;

use serde_json::{Value, json};
use tui::{Configure, SwitchScope, ToolGroup};

use super::Seam;
use crate::configure::tests::{Dirs, write};

const MEMORY: &str = "github.com/aakshintala/fiber/extensions/memory";

fn gh() -> ToolGroup {
    ToolGroup::Mcp("gh".to_owned())
}

fn memory() -> ToolGroup {
    ToolGroup::Extension(MEMORY.to_owned())
}

/// The switches for `workspace`, by group.
fn switches(seam: &Seam, workspace: &std::path::Path) -> Vec<tui::ToolSwitches> {
    seam.read_switches(workspace)
        .unwrap_or_else(|e| panic!("switches: {e}"))
}

fn read(file: &std::path::Path) -> Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap_or_else(|e| panic!("read: {e}")))
        .unwrap_or_else(|e| panic!("json: {e}"))
}

#[test]
fn switches_list_each_server_and_extension_that_has_tool_lists() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"mcp": {"servers": {
            "gh": {"tools": {"disabled": ["a"]}},
            "quiet": {"command": "x"}}}}"#,
    );
    write(
        &dirs.project_file(&workspace),
        r#"{"extensions": {"memory": {"tools": {"enabled": ["x"]}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    let groups: Vec<ToolGroup> = switches(&seam, &workspace)
        .into_iter()
        .map(|switches| switches.group)
        .collect();
    assert_eq!(groups, vec![gh(), memory()]);
}

#[test]
fn this_project_is_the_effective_lists_and_everywhere_the_globals() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["a"]}}}}}"#,
    );
    write(
        &dirs.project_file(&workspace),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["b"]}}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    let found = switches(&seam, &workspace)
        .into_iter()
        .find(|switches| switches.group == gh())
        .unwrap_or_else(|| panic!("no gh group"));
    assert_eq!(found.project.enabled, None);
    assert_eq!(found.project.disabled, vec!["b".to_owned()]);
    assert_eq!(found.everywhere.enabled, None);
    assert_eq!(found.everywhere.disabled, vec!["a".to_owned()]);
}

#[test]
fn switching_off_in_this_project_starts_from_the_inherited_list() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["a"]}}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.write_switch(&workspace, &gh(), "b", SwitchScope::Project, false)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.project_file(&workspace))["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["a", "b"])
    );
    assert_eq!(
        read(&dirs.home().join("config.json"))["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["a"])
    );
}

#[test]
fn the_inherited_list_is_the_repositorys_over_the_globals() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["a"]}}}}}"#,
    );
    write(
        &workspace.join(".fiber/config.json"),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["r"]}}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.write_switch(&workspace, &gh(), "b", SwitchScope::Project, false)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.project_file(&workspace))["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["r", "b"])
    );
}

#[test]
fn switching_off_everywhere_writes_only_the_global_file() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.project_file(&workspace),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["p"]}}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.write_switch(&workspace, &gh(), "b", SwitchScope::Everywhere, false)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.home().join("config.json"))["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["b"])
    );
    assert_eq!(
        read(&dirs.project_file(&workspace))["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["p"])
    );
}

#[test]
fn switching_on_removes_the_name_and_adds_it_to_an_enabled_list_that_lacks_it() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.project_file(&workspace),
        r#"{"mcp": {"servers": {"gh": {
            "tools": {"enabled": ["x"], "disabled": ["t"]}}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.write_switch(&workspace, &gh(), "t", SwitchScope::Project, true)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    let tools = &read(&dirs.project_file(&workspace))["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert_eq!(tools["enabled"], json!(["x", "t"]));
}

#[test]
fn switching_on_with_no_enabled_list_writes_no_enabled_key() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.project_file(&workspace),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["t"]}}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.write_switch(&workspace, &gh(), "t", SwitchScope::Project, true)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    let tools = &read(&dirs.project_file(&workspace))["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert!(tools.get("enabled").is_none(), "{tools}");
}

#[test]
fn switching_on_a_tool_the_enabled_list_names_leaves_that_list() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.project_file(&workspace),
        r#"{"mcp": {"servers": {"gh": {
            "tools": {"enabled": ["t"], "disabled": ["t"]}}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.write_switch(&workspace, &gh(), "t", SwitchScope::Project, true)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    let tools = &read(&dirs.project_file(&workspace))["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert_eq!(tools["enabled"], json!(["t"]));
}

#[test]
fn an_extension_switch_writes_under_the_files_spelling() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.project_file(&workspace),
        r#"{"extensions": {"memory": {"tools": {"disabled": []}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    seam.write_switch(&workspace, &memory(), "t", SwitchScope::Project, false)
        .unwrap_or_else(|e| panic!("switch: {e}"));
    let root = read(&dirs.project_file(&workspace));
    assert_eq!(
        root["extensions"]["memory"]["tools"]["disabled"],
        json!(["t"])
    );
    assert_eq!(
        root["extensions"]
            .as_object()
            .map(|extensions| extensions.len()),
        Some(1)
    );
    seam.settings(&workspace)
        .unwrap_or_else(|e| panic!("settings: {e}"));
}

#[test]
fn a_server_named_with_a_dot_is_written_under_its_whole_name() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    let seam = Seam::new(dirs.home());
    seam.write_switch(
        &workspace,
        &ToolGroup::Mcp("a.b".to_owned()),
        "t",
        SwitchScope::Project,
        false,
    )
    .unwrap_or_else(|e| panic!("switch: {e}"));
    assert_eq!(
        read(&dirs.project_file(&workspace)),
        json!({"mcp": {"servers": {"a.b": {"tools": {"disabled": ["t"]}}}}})
    );
}

#[test]
fn a_name_holding_a_quote_is_left_out() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"mcp": {"servers": {"a\"b": {"tools": {"disabled": ["x"]}}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    let groups: Vec<ToolGroup> = switches(&seam, &workspace)
        .into_iter()
        .map(|switches| switches.group)
        .collect();
    assert!(groups.is_empty(), "{groups:?}");
}
