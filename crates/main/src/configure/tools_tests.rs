//! Tests for `/tools` through the seam: the groups it lists and the file
//! a switch writes (`docs/tui.md`, "Swapped views";
//! `docs/configuration.md`, "When Fiber writes"). The switch's edit
//! rules live in `config` (`config::switch_tool`); one wiring test here
//! proves the seam calls through.

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
    seam.tool_switches(workspace)
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
fn a_name_holding_a_quote_is_left_out() {
    let dirs = Dirs::new();
    let workspace = dirs.workspace("one");
    write(
        &dirs.home().join("config.json"),
        r#"{"mcp": {"servers": {"a\"b": {"tools": {"disabled": ["x"]}}}}, "extensions": {"a\"b": {"tools": {"disabled": ["x"]}}}}"#,
    );
    let seam = Seam::new(dirs.home());
    let groups: Vec<ToolGroup> = switches(&seam, &workspace)
        .into_iter()
        .map(|switches| switches.group)
        .collect();
    assert!(groups.is_empty(), "{groups:?}");
}
