//! Tests for the `/tools` and `/skills` switch edits
//! (`docs/configuration.md`, "When Fiber writes"): the key each owner
//! writes and the file each switch writes.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{SwitchLayer, ToolOwner, ToolSwitch, switch_skill, switch_tool};
use crate::home::ProjectKey;
use crate::{Config, Sources};

const MEMORY_FULL: &str = "github.com/aakshintala/fiber/extensions/memory";

/// Fiber home and a workspace in one temporary directory.
struct Dirs {
    root: fakes::TempDir,
    project: ProjectKey,
}

impl Dirs {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-switches");
        for dir in ["home", "workspace"] {
            fs::create_dir_all(root.path().join(dir)).unwrap_or_else(|e| panic!("mkdir: {e}"));
        }
        Self {
            root,
            project: ProjectKey::new("test-proj").unwrap_or_else(|e| panic!("key: {e}")),
        }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    fn global_file(&self) -> PathBuf {
        self.home().join("config.json")
    }

    fn repository_file(&self) -> PathBuf {
        self.workspace().join(".fiber/config.json")
    }

    fn project_file(&self) -> PathBuf {
        self.home()
            .join("projects")
            .join("test-proj")
            .join("config.json")
    }

    fn write(&self, file: &Path, text: &str) {
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).unwrap_or_else(|e| panic!("mkdir: {e}"));
        }
        fs::write(file, text).unwrap_or_else(|e| panic!("write: {e}"));
    }

    fn load(&self) -> Config {
        Config::load(Sources {
            home: self.home(),
            workspace: self.workspace(),
            project: self.project.clone(),
            overrides: Vec::new(),
        })
        .unwrap_or_else(|e| panic!("load: {e}"))
    }
}

fn read(file: &Path) -> Value {
    serde_json::from_str(&fs::read_to_string(file).unwrap_or_else(|e| panic!("read: {e}")))
        .unwrap_or_else(|e| panic!("json: {e}"))
}

/// Switches `tool` of `owner` through `switch_tool`, failing on error.
fn flip(
    config: &Config,
    dirs: &Dirs,
    owner: ToolOwner<'_>,
    tool: &str,
    layer: SwitchLayer,
    on: bool,
) -> bool {
    switch_tool(
        config,
        ToolSwitch {
            home: &dirs.home(),
            workspace: &dirs.workspace(),
            project: &dirs.project,
            owner,
            tool,
            layer,
            on,
        },
    )
    .unwrap_or_else(|e| panic!("switch: {e}"))
}

#[test]
fn an_mcp_owner_keys_its_servers_tools() {
    assert_eq!(
        ToolOwner::Mcp("gh").key(),
        "mcp.servers.\"gh\".tools".to_owned()
    );
}

#[test]
fn an_extension_owner_grows_a_short_name_to_its_full_name() {
    assert_eq!(
        ToolOwner::Extension("memory").key(),
        format!("extensions.\"{MEMORY_FULL}\".tools")
    );
}

#[test]
fn an_extension_owner_leaves_a_full_name_alone() {
    assert_eq!(
        ToolOwner::Extension(MEMORY_FULL).key(),
        format!("extensions.\"{MEMORY_FULL}\".tools")
    );
}

#[test]
fn switching_off_in_this_project_starts_from_the_inherited_list() {
    let dirs = Dirs::new();
    dirs.write(
        &dirs.global_file(),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["a"]}}}}}"#,
    );
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "b",
        SwitchLayer::Project,
        false
    ));
    assert_eq!(
        read(&dirs.project_file())["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["a", "b"])
    );
    assert_eq!(
        read(&dirs.global_file())["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["a"])
    );
}

#[test]
fn the_inherited_list_is_the_repositorys_over_the_globals() {
    let dirs = Dirs::new();
    dirs.write(
        &dirs.global_file(),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["a"]}}}}}"#,
    );
    dirs.write(
        &dirs.repository_file(),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["r"]}}}}}"#,
    );
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "b",
        SwitchLayer::Project,
        false
    ));
    assert_eq!(
        read(&dirs.project_file())["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["r", "b"])
    );
}

#[test]
fn switching_off_everywhere_writes_only_the_global_file() {
    let dirs = Dirs::new();
    dirs.write(
        &dirs.project_file(),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["p"]}}}}}"#,
    );
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "b",
        SwitchLayer::Global,
        false
    ));
    assert_eq!(
        read(&dirs.global_file())["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["b"])
    );
    assert_eq!(
        read(&dirs.project_file())["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["p"])
    );
}

#[test]
fn switching_on_removes_the_name_and_adds_it_to_an_enabled_list_that_lacks_it() {
    let dirs = Dirs::new();
    dirs.write(
        &dirs.project_file(),
        r#"{"mcp": {"servers": {"gh": {
            "tools": {"enabled": ["x"], "disabled": ["t"]}}}}}"#,
    );
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "t",
        SwitchLayer::Project,
        true
    ));
    let tools = &read(&dirs.project_file())["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert_eq!(tools["enabled"], json!(["x", "t"]));
}

#[test]
fn switching_on_everywhere_writes_only_the_global_file() {
    let dirs = Dirs::new();
    dirs.write(
        &dirs.global_file(),
        r#"{"mcp": {"servers": {"gh": {
            "tools": {"enabled": ["x"], "disabled": ["t"]}}}}}"#,
    );
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "t",
        SwitchLayer::Global,
        true
    ));
    let tools = &read(&dirs.global_file())["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert_eq!(tools["enabled"], json!(["x", "t"]));
    assert!(!dirs.project_file().exists());
}

#[test]
fn switching_on_with_no_enabled_list_writes_no_enabled_key() {
    let dirs = Dirs::new();
    dirs.write(
        &dirs.project_file(),
        r#"{"mcp": {"servers": {"gh": {"tools": {"disabled": ["t"]}}}}}"#,
    );
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "t",
        SwitchLayer::Project,
        true
    ));
    let tools = &read(&dirs.project_file())["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert!(tools.get("enabled").is_none(), "{tools}");
}

#[test]
fn switching_on_a_tool_the_enabled_list_names_leaves_that_list() {
    let dirs = Dirs::new();
    dirs.write(
        &dirs.project_file(),
        r#"{"mcp": {"servers": {"gh": {
            "tools": {"enabled": ["t"], "disabled": ["t"]}}}}}"#,
    );
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "t",
        SwitchLayer::Project,
        true
    ));
    let tools = &read(&dirs.project_file())["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert_eq!(tools["enabled"], json!(["t"]));
}

#[test]
fn a_second_off_writes_nothing() {
    let dirs = Dirs::new();
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "a",
        SwitchLayer::Project,
        false
    ));
    assert!(!flip(
        &config,
        &dirs,
        ToolOwner::Mcp("gh"),
        "a",
        SwitchLayer::Project,
        false
    ));
    assert_eq!(
        read(&dirs.project_file())["mcp"]["servers"]["gh"]["tools"]["disabled"],
        json!(["a"])
    );
}

#[test]
fn an_extension_switch_writes_under_the_files_spelling() {
    let dirs = Dirs::new();
    dirs.write(
        &dirs.project_file(),
        r#"{"extensions": {"memory": {"tools": {"disabled": []}}}}"#,
    );
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Extension(MEMORY_FULL),
        "t",
        SwitchLayer::Project,
        false
    ));
    let root = read(&dirs.project_file());
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
    dirs.load();
}

#[test]
fn a_server_named_with_a_dot_is_written_under_its_whole_name() {
    let dirs = Dirs::new();
    let config = dirs.load();
    assert!(flip(
        &config,
        &dirs,
        ToolOwner::Mcp("a.b"),
        "t",
        SwitchLayer::Project,
        false
    ));
    assert_eq!(
        read(&dirs.project_file()),
        json!({"mcp": {"servers": {"a.b": {"tools": {"disabled": ["t"]}}}}})
    );
    dirs.load();
}

/// Switches `name` for skills through `switch_skill`, failing on error.
fn flip_skill(dirs: &Dirs, name: &str, layer: SwitchLayer, on: bool) -> bool {
    switch_skill(
        &dirs.home(),
        &dirs.workspace(),
        &dirs.project,
        layer,
        name,
        on,
    )
    .unwrap_or_else(|e| panic!("switch: {e}"))
}

#[test]
fn switching_off_in_this_project_writes_only_the_project_file() {
    let dirs = Dirs::new();
    let global = dirs.global_file();
    dirs.write(&global, r#"{"skills": {"disabled": ["a"]}}"#);
    let before = fs::read(&global).unwrap_or_else(|e| panic!("read: {e}"));
    assert!(flip_skill(&dirs, "b", SwitchLayer::Project, false));
    assert_eq!(
        read(&dirs.project_file())["skills"]["disabled"],
        json!(["b"])
    );
    assert_eq!(
        fs::read(&global).unwrap_or_else(|e| panic!("read: {e}")),
        before
    );
}

#[test]
fn switching_on_removes_the_name_from_that_layer_only() {
    let dirs = Dirs::new();
    dirs.write(&dirs.global_file(), r#"{"skills": {"disabled": ["a"]}}"#);
    dirs.write(&dirs.project_file(), r#"{"skills": {"disabled": ["a"]}}"#);
    assert!(flip_skill(&dirs, "a", SwitchLayer::Project, true));
    assert_eq!(read(&dirs.project_file())["skills"]["disabled"], json!([]));
    assert_eq!(
        read(&dirs.global_file())["skills"]["disabled"],
        json!(["a"])
    );
}

#[test]
fn switching_never_writes_the_repository_file() {
    let dirs = Dirs::new();
    let repository = dirs.repository_file();
    dirs.write(&repository, r#"{"skills": {"disabled": ["c"]}}"#);
    let before = fs::read(&repository).unwrap_or_else(|e| panic!("read: {e}"));
    for layer in [SwitchLayer::Project, SwitchLayer::Global] {
        assert!(flip_skill(&dirs, "a", layer, false));
    }
    assert_eq!(
        fs::read(&repository).unwrap_or_else(|e| panic!("read: {e}")),
        before
    );
    let config = dirs.load();
    assert_eq!(
        config.in_layer("skills.disabled", crate::Layer::Project),
        Some(json!(["a"]))
    );
    assert_eq!(
        config.in_layer("skills.disabled", crate::Layer::Global),
        Some(json!(["a"]))
    );
}

#[test]
fn a_second_off_leaves_the_file_as_it_was() {
    let dirs = Dirs::new();
    assert!(flip_skill(&dirs, "a", SwitchLayer::Project, false));
    assert!(!flip_skill(&dirs, "a", SwitchLayer::Project, false));
    assert_eq!(
        read(&dirs.project_file())["skills"]["disabled"],
        json!(["a"])
    );
}

#[test]
fn a_file_with_no_list_starts_from_none() {
    let dirs = Dirs::new();
    dirs.write(&dirs.global_file(), r#"{"skills": {"disabled": ["a"]}}"#);
    assert!(flip_skill(&dirs, "b", SwitchLayer::Project, false));
    assert_eq!(
        read(&dirs.project_file())["skills"]["disabled"],
        json!(["b"])
    );
}
