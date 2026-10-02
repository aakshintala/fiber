use std::hash::BuildHasher as _;
use std::path::PathBuf;

use std::sync::Arc;

use contract::events::ToolReplaced;
use contract::provider::ToolDefinition;
use contract::shapes::{DeclaredEffects, Effect};
use contract::tool::{Effects, EffectsError, Output, Tool};
use serde_json::{Map, Value, json};

use super::{fast_path, register};

/// A tool known only by its name and description.
struct Named(&'static str, &'static str);

impl Tool for Named {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.0.to_owned(),
            description: self.1.to_owned(),
            input_schema: json!({"type": "object"}),
            deferred: false,
        }
    }

    fn effects(&self, _: &Map<String, Value>) -> Result<Effects, EffectsError> {
        unreachable!("registration never asks for effects")
    }

    fn run(&self, _: &Map<String, Value>) -> Output {
        unreachable!("registration never runs a tool")
    }
}

fn by(registered_by: &str, tool: Named) -> (String, Arc<dyn Tool>) {
    (registered_by.to_owned(), Arc::new(tool))
}

#[test]
fn each_tool_is_registered_with_who_registered_it() {
    let (tools, replaced) = register(vec![
        by("builtin", Named("read", "Reads.")),
        by("github", Named("mcp__github__search", "Searches.")),
    ]);
    let owners: Vec<(&str, &str)> = tools
        .iter()
        .map(|(name, (by, _, _))| (name.as_str(), by.as_str()))
        .collect();
    assert_eq!(
        owners,
        [("mcp__github__search", "github"), ("read", "builtin")]
    );
    assert!(replaced.is_empty());
}

#[test]
fn a_later_tool_of_a_taken_name_replaces_the_earlier_and_is_recorded() {
    let (tools, replaced) = register(vec![
        by("builtin", Named("read", "Reads.")),
        by("lint", Named("read", "Reads, linted.")),
        by("builtin", Named("write", "Writes.")),
        by("audit", Named("read", "Reads, audited.")),
    ]);
    let (owner, _, definition) = &tools["read"];
    assert_eq!(owner, "audit");
    assert_eq!(definition.description, "Reads, audited.");
    assert_eq!(tools.len(), 2);
    let step = |from: &str, to: &str| ToolReplaced {
        name: "read".to_owned(),
        from: from.to_owned(),
        to: to.to_owned(),
    };
    assert_eq!(replaced, [step("builtin", "lint"), step("lint", "audit")]);
}

/// A fresh workspace, symlinks resolved, holding `real/` and a link `out`
/// to a directory outside it.
fn workspace() -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!(
        "fiber-calls-{:x}",
        std::collections::hash_map::RandomState::new().hash_one(())
    ));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let workspace = root.join("ws");
    std::fs::create_dir_all(workspace.join("real")).unwrap();
    std::fs::create_dir_all(root.join("elsewhere")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), workspace.join("out")).unwrap();
    (root, workspace)
}

fn declared(effects: &[Effect], paths: Option<&[&str]>) -> DeclaredEffects {
    DeclaredEffects {
        effects: effects.to_vec(),
        reversible: false,
        paths: paths.map(|p| p.iter().map(|s| (*s).to_owned()).collect()),
    }
}

#[test]
fn reads_and_no_effect_take_the_fast_path_wherever_they_point() {
    let (root, ws) = workspace();
    assert!(fast_path(&declared(&[], None), &ws));
    assert!(fast_path(
        &declared(&[Effect::Reads], Some(&["/etc/hosts"])),
        &ws
    ));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_write_takes_the_fast_path_only_inside_the_workspace_and_outside_git_and_fiber() {
    let (root, ws) = workspace();
    let writes = |paths: &[&str]| {
        fast_path(
            &declared(&[Effect::Reads, Effect::Writes], Some(paths)),
            &ws,
        )
    };
    let inside = ws.join("real/new/file.rs").display().to_string();
    assert!(writes(&["real/a.rs", "new.rs", &inside]));
    for outside in [
        "/tmp/x",
        "../x",
        "real/../../x",
        // Through a link that leaves the workspace.
        "out/x",
        "out/../x",
        // `..` past a directory that does not exist yet.
        "new/../real/x",
        ".git/config",
        "real/.git/hooks/pre-commit",
        ".fiber/config.json",
    ] {
        assert!(!writes(&[outside]), "{outside}");
    }
    assert!(!writes(&["real/a.rs", ".git/x"]));
    // No paths, or none declared, is reviewed.
    assert!(!writes(&[]));
    assert!(!fast_path(&declared(&[Effect::Writes], None), &ws));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn executes_and_network_never_take_the_fast_path() {
    let (root, ws) = workspace();
    for effect in [Effect::Executes, Effect::Network] {
        assert!(!fast_path(
            &declared(&[Effect::Reads, effect], Some(&["real/a"])),
            &ws
        ));
    }
    std::fs::remove_dir_all(root).unwrap();
}
