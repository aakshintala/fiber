//! The terminal's model list: the cached discovery data as the picker
//! lists it, refreshed in the background (`docs/model-routing.md`,
//! "Model discovery"). Tests never touch the real home and never load an
//! extension except through the `load` they pass.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

use crate::test_support::install_extension;
use std::cell::Cell;
use std::path::PathBuf;

use config::Config;
use extensions::SessionExtensions;
use serde_json::{Value, json};

use super::{mode, read};

/// Fiber home and a workspace in a temporary directory, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new(prefix: &str) -> Self {
        let setup = Self {
            root: fakes::TempDir::new(prefix),
        };
        std::fs::create_dir_all(setup.home()).unwrap();
        std::fs::create_dir_all(setup.workspace()).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("workspace")
    }

    /// Installs a data-only extension registering `name` with these model
    /// ids, each on `openai-responses` with `extra` merged into each model.
    fn install_data(&self, extension: &str, name: &str, ids: &[&str], extra: &Value) {
        let models: Vec<Value> = ids
            .iter()
            .map(|id| {
                let mut model = json!({
                    "id": id,
                    "protocol": "openai-responses",
                    "base_url": "http://127.0.0.1:1/v1",
                    "context_window": 1000,
                });
                for (key, value) in extra.as_object().cloned().unwrap_or_default() {
                    model[key] = value;
                }
                model
            })
            .collect();
        install_extension(
            &self.home(),
            &format!("extensions/{extension}"),
            json!({
                "name": extension,
                "version": "v1.0.0",
                "fiber": "0.1.0",
                "api": extensions::API,
            }),
            &[(name, json!({"name": name, "models": models}))],
        );
    }

    fn write_config(&self, value: &Value) {
        std::fs::write(self.home().join("config.json"), value.to_string()).unwrap();
    }
}

/// Loads no extension and records that nothing was loaded.
fn unloaded(called: &Cell<bool>) -> impl Fn(&Config) -> SessionExtensions + '_ {
    |_| {
        called.set(true);
        SessionExtensions::default()
    }
}

#[test]
fn each_refresh_maps_to_its_read_mode() {
    assert_eq!(mode(tui::Refresh::Cached), cli::ModelLists::Cached);
    assert_eq!(
        mode(tui::Refresh::Stale),
        cli::ModelLists::Refresh { check_age: true }
    );
    assert_eq!(
        mode(tui::Refresh::Every),
        cli::ModelLists::Refresh { check_age: false }
    );
}

#[test]
fn levels_and_default_come_from_the_model() {
    let setup = Setup::new("fiber-model-list-levels");
    setup.install_data(
        "acme-ext",
        "acme",
        &["m1"],
        &json!({"thinking_levels": ["low", "high"], "thinking_default": "high"}),
    );
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        &unloaded(&called),
    )
    .unwrap();
    let [entry] = catalogue.models.as_slice() else {
        panic!("{:?}", catalogue.models);
    };
    assert_eq!(entry.levels, ["low", "high"]);
    assert_eq!(entry.default_level.as_deref(), Some("high"));
    assert_eq!(entry.configured, None);
    assert_eq!(entry.provider, "acme");
    assert_eq!(entry.id, "m1");
}

#[test]
fn configured_is_the_per_model_level_then_the_top_level() {
    let setup = Setup::new("fiber-model-list-configured");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    setup.write_config(&json!({
        "thinking": "low",
        "models": {"acme/m1": {"thinking": "high"}},
    }));
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        &unloaded(&called),
    )
    .unwrap();
    assert_eq!(
        catalogue
            .models
            .first()
            .and_then(|entry| entry.configured.clone()),
        Some("high".to_owned())
    );
    setup.write_config(&json!({"thinking": "low"}));
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        &unloaded(&called),
    )
    .unwrap();
    assert_eq!(
        catalogue
            .models
            .first()
            .and_then(|entry| entry.configured.clone()),
        Some("low".to_owned())
    );
}

#[test]
fn a_fiber_role_marks_its_model_with_or_without_a_level() {
    let setup = Setup::new("fiber-model-list-roles");
    setup.install_data("acme-ext", "acme", &["m1", "m2"], &json!({}));
    setup.write_config(&json!({
        "roles": {"fast": "fiber:acme/m1", "deep": "fiber:acme/m1:high"},
    }));
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        &unloaded(&called),
    )
    .unwrap();
    let marked: Vec<(&str, Vec<&str>)> = catalogue
        .models
        .iter()
        .map(|entry| {
            (
                entry.reference.as_str(),
                entry.roles.iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        marked,
        [("acme/m1", vec!["deep", "fast"]), ("acme/m2", vec![])],
    );
}

#[test]
fn load_notices_reach_the_catalogue() {
    let setup = Setup::new("fiber-model-list-notices");
    install_extension(
        &setup.home(),
        "extensions/old-ext",
        json!({
            "name": "old-ext",
            "version": "v1.0.0",
            "fiber": "0.1.0",
            "api": extensions::API + 1,
        }),
        &[],
    );
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        &unloaded(&called),
    )
    .unwrap();
    assert!(catalogue.models.is_empty());
    assert_eq!(catalogue.notices.len(), 1, "{:?}", catalogue.notices);
    assert!(
        catalogue.notices[0].contains("extension API"),
        "{:?}",
        catalogue.notices
    );
}

#[test]
fn no_provider_is_an_empty_catalogue() {
    let setup = Setup::new("fiber-model-list-empty");
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        &unloaded(&called),
    )
    .unwrap();
    assert_eq!(catalogue, tui::Catalogue::default());
}
