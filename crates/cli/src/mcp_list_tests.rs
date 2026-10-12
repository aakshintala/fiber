//! `run_list`: one row per declaration, with the layer and, for a
//! repository's server, whether it is approved.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use config::{Layer, insert_entry};
use contract::events::OfferedKind;
use extensions::{Decision, Index, Store};
use serde_json::json;

use super::{Streams, has_value, run_list};
use crate::project_of;

struct Setup {
    tmp: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let tmp = fakes::TempDir::new("fiber-mcp-list");
        fs::create_dir_all(tmp.path().join("home")).unwrap();
        fs::create_dir_all(tmp.path().join("ws")).unwrap();
        Self { tmp }
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.tmp.path().join("ws")
    }

    fn declare(&self, layer: Layer, name: &str, entry: serde_json::Value) {
        let (_, project) = project_of(&self.home(), &self.workspace()).unwrap();
        insert_entry(
            &self.home(),
            &self.workspace(),
            &project,
            layer,
            "mcp.servers",
            name,
            entry,
        )
        .unwrap();
    }

    fn remote(&self, layer: Layer, name: &str) {
        self.declare(layer, name, json!({"url": "https://example.test/mcp"}));
    }
}

struct Run {
    result: Result<(), contract::shapes::Failure>,
    out: String,
    err: String,
}

fn go(setup: &Setup) -> Run {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = run_list(
        &setup.home(),
        &setup.workspace(),
        Streams {
            out: &mut out,
            err: &mut err,
        },
        &clock,
    );
    Run {
        result,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

/// Approves the repository declaration `name` as `mcp add --repo` would,
/// answering the recorded decision for its hash.
fn approve_repo(setup: &Setup, name: &str, never: bool) -> String {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_, project) = project_of(&setup.home(), &setup.workspace()).unwrap();
    let items = extensions::declared_items(&setup.workspace(), clock.as_ref()).unwrap();
    let item = items
        .iter()
        .find(|item| item.kind == OfferedKind::McpServer && item.name == name)
        .unwrap();
    let mut index = Index::load(&setup.home());
    let hash = extensions::hash(&mut index, item).unwrap();
    let store = Store::new(&setup.home(), &project, clock);
    if never {
        store.never(item, &hash).unwrap();
    } else {
        store.approve(item, &hash).unwrap();
    }
    hash
}

#[test]
fn rows_sort_by_name_then_layer_and_shadowed_names_list_twice() {
    let setup = Setup::new();
    setup.remote(Layer::Global, "b");
    setup.remote(Layer::Global, "a");
    setup.remote(Layer::Project, "a");
    setup.remote(Layer::Repository, "c");
    setup.remote(Layer::Repository, "a");
    let ran = go(&setup);
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(
        ran.out,
        "a  global\n\
         a  project\n\
         a  repository  not approved\n\
         b  global\n\
         c  repository  not approved\n"
    );
    assert!(ran.err.is_empty(), "stderr: {}", ran.err);
}

#[test]
fn an_approved_repository_row_says_so_and_a_changed_one_does_not() {
    let setup = Setup::new();
    setup.declare(
        Layer::Repository,
        "docs",
        json!({"command": "npx", "args": ["pkg"]}),
    );
    approve_repo(&setup, "docs", false);
    let ran = go(&setup);
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(ran.out, "docs  repository  approved\n", "stdout: {}", ran.out);
    // An argument added: the declaration changed since the approval.
    let (_, project) = project_of(&setup.home(), &setup.workspace()).unwrap();
    let file = setup.workspace().join(".fiber/config.json");
    let _ = &project;
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        root,
        json!({"mcp": {"servers": {"docs": {"command": "npx", "args": ["pkg"]}}}})
    );
    fs::write(
        &file,
        serde_json::to_string_pretty(&json!({"mcp": {"servers": {"docs": {
            "command": "npx", "args": ["pkg", "more"],
        }}}}))
        .unwrap(),
    )
    .unwrap();
    let ran = go(&setup);
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(ran.out, "docs  repository  not approved\n", "stdout: {}", ran.out);
}

#[test]
fn a_never_record_prints_not_approved() {
    let setup = Setup::new();
    setup.remote(Layer::Repository, "docs");
    let hash = approve_repo(&setup, "docs", true);
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_, project) = project_of(&setup.home(), &setup.workspace()).unwrap();
    assert_eq!(
        Store::new(&setup.home(), &project, clock).decision(OfferedKind::McpServer, &hash),
        Some(Decision::Never)
    );
    let ran = go(&setup);
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(ran.out, "docs  repository  not approved\n", "stdout: {}", ran.out);
}

#[test]
fn an_entry_holding_no_value_is_not_listed() {
    assert!(!has_value(&json!({})));
    assert!(!has_value(&json!({"a": {}})));
    assert!(!has_value(&json!({"a": {"b": {}}})));
    assert!(!has_value(&json!({"url": {}})));
    assert!(has_value(&json!({"url": "https://example.test/mcp"})));
    assert!(has_value(&json!(null)));
    assert!(has_value(&json!("x")));
    let setup = Setup::new();
    let (_, project) = project_of(&setup.home(), &setup.workspace()).unwrap();
    insert_entry(
        &setup.home(),
        &setup.workspace(),
        &project,
        Layer::Global,
        "mcp.servers",
        "empty",
        json!({}),
    )
    .unwrap();
    setup.remote(Layer::Global, "docs");
    let ran = go(&setup);
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(ran.out, "docs  global\n", "stdout: {}", ran.out);
}

#[test]
fn nothing_about_a_declaration_prints() {
    let setup = Setup::new();
    setup.declare(
        Layer::Global,
        "x",
        json!({"url": "https://example.test/mcp?token=zzz", "timeout_ms": 1}),
    );
    setup.declare(
        Layer::Project,
        "y",
        json!({"command": "npx", "env": {"K": "zzz"}}),
    );
    let ran = go(&setup);
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert!(!ran.out.contains("zzz"), "stdout: {}", ran.out);
    assert!(!ran.out.contains("example.test"), "stdout: {}", ran.out);
    assert!(!ran.out.contains("npx"), "stdout: {}", ran.out);
    assert_eq!(ran.out, "x  global\ny  project\n", "stdout: {}", ran.out);
}

#[test]
fn an_empty_configuration_prints_nothing() {
    let setup = Setup::new();
    let ran = go(&setup);
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert!(ran.out.is_empty(), "stdout: {}", ran.out);
    assert!(ran.err.is_empty(), "stderr: {}", ran.err);
}
