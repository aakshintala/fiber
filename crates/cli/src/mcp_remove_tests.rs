//! `run_remove`: what it removes and what it refuses, with both output
//! streams in memory.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use config::{Layer, Secret, insert_entry, store_secret};
use contract::ErrorCode;
use contract::events::OfferedKind;
use extensions::{Decision, Index, Store};
use serde_json::json;

use super::{Streams, run_remove};
use crate::project_of;

struct Setup {
    tmp: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let tmp = fakes::TempDir::new("fiber-mcp-remove");
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

    fn file(&self, layer: Layer) -> PathBuf {
        let (_, project) = project_of(&self.home(), &self.workspace()).unwrap();
        match layer {
            Layer::Global => self.home().join("config.json"),
            Layer::Project => self
                .home()
                .join("projects")
                .join(project.as_str())
                .join("config.json"),
            Layer::Repository => self.workspace().join(".fiber/config.json"),
        }
    }

    fn declare(&self, layer: Layer, name: &str) {
        let (_, project) = project_of(&self.home(), &self.workspace()).unwrap();
        insert_entry(
            &self.home(),
            &self.workspace(),
            &project,
            layer,
            "mcp.servers",
            name,
            json!({"url": "https://example.test/mcp"}),
        )
        .unwrap();
    }
}

struct Run {
    result: Result<(), contract::shapes::Failure>,
    out: String,
    err: String,
}

fn go(setup: &Setup, layer: Layer, name: &str) -> Run {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = run_remove(
        &setup.home(),
        &setup.workspace(),
        layer,
        name,
        Streams {
            out: &mut out,
            err: &mut err,
        },
    );
    Run {
        result,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

/// The approval recorded for the repository declaration `name`.
fn approve_repo(setup: &Setup, name: &str) {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_, project) = project_of(&setup.home(), &setup.workspace()).unwrap();
    let items = extensions::declared_items(&setup.workspace(), clock.as_ref()).unwrap();
    let item = items
        .iter()
        .find(|item| item.kind == OfferedKind::McpServer && item.name == name)
        .unwrap();
    let mut index = Index::load(&setup.home());
    let hash = extensions::hash(&mut index, item).unwrap();
    Store::new(&setup.home(), &project, clock).approve(item, &hash).unwrap();
}

/// The declaration's hash as startup hashes it, and the decision recorded
/// for it. The hash is captured before any removal: after it the
/// declaration is gone and there is nothing to hash.
fn hash_and_decision(setup: &Setup, name: &str) -> (String, Option<Decision>) {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_, project) = project_of(&setup.home(), &setup.workspace()).unwrap();
    let items = extensions::declared_items(&setup.workspace(), clock.as_ref()).unwrap();
    let item = items
        .iter()
        .find(|item| item.kind == OfferedKind::McpServer && item.name == name)
        .unwrap();
    let mut index = Index::load(&setup.home());
    let hash = extensions::hash(&mut index, item).unwrap();
    let decision = Store::new(&setup.home(), &project, clock).decision(OfferedKind::McpServer, &hash);
    (hash, decision)
}

fn decision_at(setup: &Setup, hash: &str) -> Option<Decision> {
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let (_, project) = project_of(&setup.home(), &setup.workspace()).unwrap();
    Store::new(&setup.home(), &project, clock).decision(OfferedKind::McpServer, hash)
}

#[test]
fn remove_deletes_only_the_chosen_layer() {
    let setup = Setup::new();
    setup.declare(Layer::Global, "docs");
    setup.declare(Layer::Project, "docs");
    let ran = go(&setup, Layer::Global, "docs");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(setup.file(Layer::Global)).unwrap()
        )
        .unwrap(),
        json!({})
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &fs::read_to_string(setup.file(Layer::Project)).unwrap()
        )
        .unwrap(),
        json!({"mcp": {"servers": {"docs": {"url": "https://example.test/mcp"}}}})
    );
    assert!(ran.out.is_empty(), "stdout: {}", ran.out);
    assert!(ran.err.is_empty(), "stderr: {}", ran.err);
}

#[test]
fn remove_of_an_absent_name_names_the_layers_holding_it() {
    let setup = Setup::new();
    setup.declare(Layer::Project, "docs");
    setup.declare(Layer::Repository, "docs");
    let ran = go(&setup, Layer::Global, "docs");
    let failure = ran.result.unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
    assert!(failure.message.contains("`docs`"), "{}", failure.message);
    assert!(failure.message.contains("global"), "{}", failure.message);
    assert!(failure.message.contains("project"), "{}", failure.message);
    assert!(failure.message.contains("repository"), "{}", failure.message);
    assert!(!setup.file(Layer::Global).exists(), "a file was created");
}

#[test]
fn remove_of_a_name_no_layer_declares_names_none() {
    let setup = Setup::new();
    setup.declare(Layer::Global, "other");
    let ran = go(&setup, Layer::Global, "missing");
    let failure = ran.result.unwrap_err();
    assert_eq!(failure.code, ErrorCode::Usage);
    assert!(failure.message.contains("`missing`"), "{}", failure.message);
    assert!(failure.message.contains("no layer declares"), "{}", failure.message);
}

#[test]
fn remove_from_the_repository_leaves_the_approval_and_the_secret() {
    let setup = Setup::new();
    setup.declare(Layer::Repository, "docs");
    approve_repo(&setup, "docs");
    store_secret(
        &setup.home(),
        "mcp.docs.TOKEN",
        &Secret::new("tok-1".into()),
    )
    .unwrap();
    let (hash, before) = hash_and_decision(&setup, "docs");
    assert_eq!(before, Some(Decision::Approve));
    let ran = go(&setup, Layer::Repository, "docs");
    assert!(ran.result.is_ok(), "{:?}", ran.result.err().map(|e| e.message));
    assert_eq!(decision_at(&setup, &hash), Some(Decision::Approve));
    assert_eq!(
        config::read_secret(&setup.home(), "mcp.docs.TOKEN")
            .unwrap()
            .unwrap()
            .expose(),
        "tok-1"
    );
}
