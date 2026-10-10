//! The terminal's model list: the cached discovery data as the picker
//! lists it, refreshed in the background (`docs/model-routing.md`,
//! "Model discovery"). Tests never touch the real home and never load an
//! extension except through the `load` they pass.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

use crate::test_support::{Rig, install_extension};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use config::Config;
use extensions::SessionExtensions;
use serde_json::{Value, json};

use super::{mode, read};

/// Model-list fixtures with test-specific extension helpers.
struct Setup {
    rig: Rig,
}

impl Setup {
    fn new(prefix: &str) -> Self {
        Self {
            rig: Rig::new(prefix),
        }
    }

    fn home(&self) -> PathBuf {
        self.rig.home.clone()
    }

    fn workspace(&self) -> PathBuf {
        self.rig.workspace.clone()
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

/// A fixed `now` for the list-time tests: far past the epoch, so a
/// cache file dated hours earlier is safely before it.
fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// Writes `provider`'s cached list and dates it `age` before [`now`].
/// The content never matters to a `Cached` read of an installed
/// provider; only the file's mtime does.
fn date_cache(home: &Path, provider: &str, age: Duration) {
    config::write_model_cache(
        home,
        provider,
        &json!([{"id": "m1", "protocol": "openai-responses",
                   "base_url": "http://127.0.0.1:1/v1"}]),
    )
    .unwrap();
    let file = home.join("cache/models").join(format!("{provider}.json"));
    let mtime = now().checked_sub(age).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(mtime)
        .unwrap();
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
        now(),
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
        now(),
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
        now(),
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
        now(),
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
        now(),
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
        now(),
        &unloaded(&called),
    )
    .unwrap();
    assert_eq!(catalogue, tui::Catalogue::default());
}

#[test]
fn price_maps_cache_write_and_falls_back_to_input() {
    let setup = Setup::new("fiber-model-list-price");
    setup.install_data(
        "acme-ext",
        "acme",
        &["m1"],
        &json!({"cost": {"input": 3.0, "output": 15.0}}),
    );
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
    )
    .unwrap();
    let [entry] = catalogue.models.as_slice() else {
        panic!("{:?}", catalogue.models);
    };
    // No `cache_write`: the input price stands in.
    assert_eq!(
        entry.price,
        Some(tui::Price {
            micros_per_mtok: 3_000_000,
            tiers: Vec::new(),
        })
    );
}

#[test]
fn price_maps_cache_write_over_input() {
    let setup = Setup::new("fiber-model-list-price-write");
    setup.install_data(
        "acme-ext",
        "acme",
        &["m1"],
        &json!({"cost": {"input": 3.0, "output": 15.0, "cache_write": 0.75}}),
    );
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
    )
    .unwrap();
    let [entry] = catalogue.models.as_slice() else {
        panic!("{:?}", catalogue.models);
    };
    assert_eq!(
        entry.price,
        Some(tui::Price {
            micros_per_mtok: 750_000,
            tiers: Vec::new(),
        })
    );
}

#[test]
fn price_is_none_without_a_cost_and_tiers_sort_ascending() {
    let setup = Setup::new("fiber-model-list-price-tiers");
    setup.install_data(
        "acme-ext",
        "acme",
        &["m1"],
        &json!({"cost": {"input": 2.0, "output": 10.0, "tiers": [
            {"input_tokens_above": 200000, "input": 4.0, "output": 20.0,
             "cache_read": 1.0, "cache_write": 1.5},
            {"input_tokens_above": 100000, "input": 3.0, "output": 15.0,
             "cache_read": 0.5, "cache_write": 0.0},
        ]}}),
    );
    setup.install_data("zeta-ext", "zeta", &["z1"], &json!({}));
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
    )
    .unwrap();
    let prices: Vec<(&str, Option<tui::Price>)> = catalogue
        .models
        .iter()
        .map(|entry| (entry.reference.as_str(), entry.price.clone()))
        .collect();
    // Out of install order the tiers sort ascending; a zero tier
    // `cache_write` names no price, so the tier's input stands in.
    assert_eq!(
        prices,
        [
            (
                "acme/m1",
                Some(tui::Price {
                    micros_per_mtok: 2_000_000,
                    tiers: vec![(100_000, 3_000_000), (200_000, 1_500_000)],
                })
            ),
            ("zeta/z1", None),
        ]
    );
}

#[test]
fn fresh_cache_lists_its_age() {
    let setup = Setup::new("fiber-model-list-fresh");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    date_cache(&setup.home(), "acme", Duration::from_secs(90));
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
    )
    .unwrap();
    assert_eq!(
        catalogue.lists,
        [tui::ListAge {
            provider: "acme".to_owned(),
            updated_ms: Some(contract::clock::wall_ms(now()) - 90_000),
            stale: false,
        }]
    );
}

#[test]
fn cache_at_refresh_after_is_fresh_one_second_over_is_stale() {
    // The default `refresh_after` is a day: exactly a day old reads
    // fresh, one second over reads stale.
    let setup = Setup::new("fiber-model-list-stale");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    let called = Cell::new(false);
    date_cache(&setup.home(), "acme", Duration::from_secs(86_400));
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
    )
    .unwrap();
    assert!(
        !catalogue.lists[0].stale,
        "exactly refresh_after is fresh: {:?}",
        catalogue.lists
    );
    date_cache(&setup.home(), "acme", Duration::from_secs(86_401));
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
    )
    .unwrap();
    assert!(
        catalogue.lists[0].stale,
        "one second over is stale: {:?}",
        catalogue.lists
    );
}

#[test]
fn missing_cache_has_no_time_and_is_stale() {
    let setup = Setup::new("fiber-model-list-no-cache");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
    )
    .unwrap();
    assert_eq!(
        catalogue.lists,
        [tui::ListAge {
            provider: "acme".to_owned(),
            updated_ms: None,
            stale: true,
        }]
    );
}

#[test]
fn future_cache_reads_as_updated_now() {
    let setup = Setup::new("fiber-model-list-future");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    config::write_model_cache(
        &setup.home(),
        "acme",
        &json!([{"id": "m1", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1"}]),
    )
    .unwrap();
    let file = setup.home().join("cache/models/acme.json");
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(now() + Duration::from_secs(30))
        .unwrap();
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
    )
    .unwrap();
    // A file dated after `now` reads as updated `now`: age zero.
    assert_eq!(
        catalogue.lists,
        [tui::ListAge {
            provider: "acme".to_owned(),
            updated_ms: Some(contract::clock::wall_ms(now())),
            stale: false,
        }]
    );
}
