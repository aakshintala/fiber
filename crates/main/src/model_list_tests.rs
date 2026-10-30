//! The terminal's model list: the cached discovery data as the picker
//! lists it, refreshed in the background (`docs/model-routing.md`,
//! "Model discovery"). Tests never touch the real home and never load an
//! extension except through the `load` they pass.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

use crate::test_support::{Rig, install_extension};
use fakes::MUST_SUCCEED_WITHIN;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use config::Config;
use contract::clock::Clock;
use extensions::SessionExtensions;
use serde_json::{Value, json};

use super::{catalogue, mode, read, reader};

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
    /// A positive fixture is logged in: the presence gate needs a stored
    /// login for its models to list.
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
        config::store_credential(
            &self.home(),
            name,
            "default",
            &config::Secret::new("test".into()),
        )
        .unwrap();
    }

    fn write_config(&self, value: &Value) {
        std::fs::write(self.home().join("config.json"), value.to_string()).unwrap();
    }

    /// Installs a data-only extension `extension` registering provider
    /// `name` with two models on `{setting}` (one named) and that one
    /// placeholder declared, the `install_data` layout with separate
    /// extension and provider names. `credential` is merged into the
    /// provider data, so a `credential_label` reads a login the
    /// `fiber login` command cannot ask for. A positive fixture is
    /// logged in: the presence gate needs a stored login, which
    /// `credential_label` providers ignore.
    fn install_placeholder_data(
        &self,
        extension: &str,
        name: &str,
        setting: &str,
        credential: Value,
    ) {
        let template = format!("https://{{{setting}}}/v1");
        let models = json!([
            {"id": "m1", "protocol": "openai-responses",
             "base_url": template, "context_window": 1000,
             "name": "Acme One"},
            {"id": "m2", "protocol": "openai-responses",
             "base_url": template, "context_window": 1000},
        ]);
        let mut data = json!({"name": name, "models": models});
        data["placeholders"] =
            Value::Object([(setting.to_owned(), json!({}))].into_iter().collect());
        for (key, value) in credential.as_object().cloned().unwrap_or_default() {
            data[key] = value;
        }
        install_extension(
            &self.home(),
            &format!("extensions/{}", config::dir_name(extension)),
            json!({
                "name": extension,
                "version": "v1.0.0",
                "fiber": "0.1.0",
                "api": extensions::API,
            }),
            &[(name, data)],
        );
        config::store_credential(
            &self.home(),
            name,
            "default",
            &config::Secret::new("test".into()),
        )
        .unwrap();
    }

    /// Writes `value` for `setting` to the extension's global
    /// configuration file.
    fn write_setting(&self, extension: &str, setting: &str, value: &str) {
        let dir = self.home().join("config");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{}.json", config::dir_name(extension))),
            json!({setting: value}).to_string(),
        )
        .unwrap();
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
fn a_model_name_reaches_its_picker_entry() {
    let setup = Setup::new("fiber-model-list-name");
    install_extension(
        &setup.home(),
        "extensions/acme-ext",
        json!({
            "name": "acme-ext",
            "version": "v1.0.0",
            "fiber": "0.1.0",
            "api": extensions::API,
        }),
        &[(
            "acme",
            json!({"name": "acme", "models": [
                {"id": "sonnet", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
                 "name": "Claude Opus 5.5"},
                {"id": "haiku", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1", "context_window": 1000},
            ]}),
        )],
    );
    // Logged in: the presence gate needs a stored login for the models
    // to list.
    config::store_credential(
        &setup.home(),
        "acme",
        "default",
        &config::Secret::new("test".into()),
    )
    .unwrap();
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    // Neither the provider nor either id contains "opus": only the
    // display name can carry the picker's "opus 5.5" query. The picker's
    // matcher is pub(crate) to tui, so this test cannot run the query;
    // tui's own filter tests cover matching on the name, and this
    // covers the wiring that hands the name to the entry.
    let named = catalogue
        .models
        .iter()
        .find(|entry| entry.reference == "acme/sonnet")
        .unwrap();
    assert_eq!(named.name.as_deref(), Some("Claude Opus 5.5"));
    let plain = catalogue
        .models
        .iter()
        .find(|entry| entry.reference == "acme/haiku")
        .unwrap();
    assert_eq!(plain.name, None);
}

/// The terminal's reader lists the installed models on a cached read,
/// through the same runner a switch reads with: replacing it with a
/// default would list nothing.
#[test]
fn the_readers_cached_read_lists_the_installed_models() {
    let setup = Setup::new("fiber-model-list-reader");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    let clock: std::sync::Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let locks: std::sync::Arc<dyn contract::files::PathLock> = Arc::new(tools::PathLocks::new());
    let catalogue = reader(
        setup.home(),
        setup.workspace(),
        clock,
        locks,
        Arc::default(),
    )(tui::Refresh::Cached)
    .map_err(|error| format!("the cached read: {error}"))
    .unwrap();
    assert!(
        catalogue
            .models
            .iter()
            .any(|entry| entry.reference == "acme/m1"),
        "{catalogue:?}"
    );
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert!(catalogue.models.is_empty());
    assert_eq!(catalogue.notices.len(), 1, "{:?}", catalogue.notices);
    assert!(
        catalogue.notices[0].message.contains("extension API"),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert!(catalogue.models.is_empty());
    assert!(catalogue.notices.is_empty());
    assert!(catalogue.lists.is_empty());
    assert_eq!(
        catalogue.installable,
        extensions::not_installed_providers(&setup.home()).unwrap()
    );
    assert!(catalogue.installable.iter().any(|name| name == "openai"));
    assert!(!catalogue.installable.iter().any(|name| name == "scripted"));
}

#[test]
fn installed_first_party_declaration_is_not_installable() {
    let setup = Setup::new("fiber-model-list-installable");
    setup.install_data("custom", "openai", &["m"], &json!({}));
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let expected: Vec<String> = extensions::not_installed_providers(&setup.home())
        .unwrap()
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(catalogue.installable, expected);
    assert!(!catalogue.installable.iter().any(|name| name == "openai"));
    assert!(!catalogue.installable.iter().any(|name| name == "scripted"));
    assert_eq!(catalogue.models[0].reference, "openai/m");
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
            input: 3_000_000,
            output: 15_000_000,
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let [entry] = catalogue.models.as_slice() else {
        panic!("{:?}", catalogue.models);
    };
    assert_eq!(
        entry.price,
        Some(tui::Price {
            micros_per_mtok: 750_000,
            input: 3_000_000,
            output: 15_000_000,
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
                    input: 2_000_000,
                    output: 10_000_000,
                    tiers: vec![
                        tui::PriceTier {
                            input_tokens_above: 100_000,
                            micros_per_mtok: 3_000_000,
                            input: 3_000_000,
                            output: 15_000_000
                        },
                        tui::PriceTier {
                            input_tokens_above: 200_000,
                            micros_per_mtok: 1_500_000,
                            input: 4_000_000,
                            output: 20_000_000
                        }
                    ],
                })
            ),
            ("zeta/z1", None),
        ]
    );
}

#[test]
fn a_zero_cost_is_a_free_price_not_none() {
    let setup = Setup::new("fiber-model-list-price-free");
    setup.install_data(
        "acme-ext",
        "acme",
        &["m1"],
        &json!({"cost": {"input": 0.0, "output": 0.0}}),
    );
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert_eq!(
        catalogue.models[0].price,
        Some(tui::Price {
            micros_per_mtok: 0,
            input: 0,
            output: 0,
            tiers: vec![],
        })
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
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

/// An invalid configuration keeps its code through the typed read: the
/// hub answers `config_invalid` with it, naming the file.
#[test]
fn an_invalid_configuration_keeps_its_config_invalid_code() {
    let setup = Setup::new("fiber-model-list-invalid-code");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    std::fs::write(setup.home().join("config.json"), "{invalid").unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let error = catalogue(
        setup.home(),
        setup.workspace(),
        clock,
        Arc::new(tools::PathLocks::new()),
        Arc::default(),
        tui::Refresh::Cached,
    )
    .unwrap_err();
    assert_eq!(error.code(), contract::ErrorCode::ConfigInvalid);
    assert!(
        error.to_string().contains("config.json"),
        "the message names the file: {error}"
    );
}

/// An unreadable configuration file keeps its own code through the
/// typed read: a directory `config.json` is `io_failed`, not
/// `config_invalid`.
#[test]
fn an_unreadable_configuration_keeps_its_io_code() {
    let setup = Setup::new("fiber-model-list-io-code");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    std::fs::create_dir(setup.home().join("config.json")).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let error = catalogue(
        setup.home(),
        setup.workspace(),
        clock,
        Arc::new(tools::PathLocks::new()),
        Arc::default(),
        tui::Refresh::Cached,
    )
    .unwrap_err();
    assert_eq!(error.code(), contract::ErrorCode::IoFailed);
}

/// The deadline of each wait in the command-source tests.
const DEADLINE: Duration = Duration::from_secs(10);

/// A provider `acme` serving `m1` from its data file, with a Lua handle
/// registering no `credential`, a cached list dated just past the epoch,
/// and a `command` credential source writing its group id to a FIFO: the
/// first line of every test proves the command ran.
struct CommandFixture {
    setup: Setup,
    ready: fakes::children::Ready,
    reads: Arc<crate::switch::Reads>,
    clock: Arc<dyn Clock>,
}

impl CommandFixture {
    fn new(prefix: &str) -> Self {
        let setup = Setup::new(prefix);
        let source = setup.home().join("src").join("acme");
        install_extension(
            &setup.home(),
            "src/acme",
            json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": extensions::API}),
            &[(
                "acme",
                json!({"name": "acme", "models": [{"id": "m1", "protocol": "openai-responses",
                         "base_url": "http://127.0.0.1:1/v1", "context_window": 1000}]}),
            )],
        );
        // A Lua handle registering no `credential`: the refresh falls
        // through to the configured command source.
        std::fs::write(
            {
                std::fs::create_dir_all(source.join("providers")).unwrap();
                source.join("providers/acme.lua")
            },
            "fiber.provider(\"acme\", {})\n",
        )
        .unwrap();
        extensions::plan(
            &setup.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        config::write_model_cache(
            &setup.home(),
            "acme",
            &json!([{"id": "m1", "protocol": "openai-responses",
                     "base_url": "http://127.0.0.1:1/v1",
                     "context_window": 1000}]),
        )
        .unwrap();
        // Dated just past the epoch: stale under the injected clock, whose
        // wall is 1_700_000_000 s. A date taken from the test's own `now()`
        // (1_800_000_000 s) would read as future under it, age zero, and
        // the command would never run.
        let cache = setup.home().join("cache/models/acme.json");
        std::fs::File::options()
            .write(true)
            .open(&cache)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        // The FIFO lives in the home: nothing scans the home's root, and
        // its lifetime is the setup's.
        let ready = fakes::children::Ready::new(&setup.home());
        // The descendant starts before the ready line: the line then proves the
        // whole group already runs, so a cancel after it finds every member.
        // Echoing first would invite the kill to chase a fork still in flight,
        // and a child born during the kill's walk of the group can miss the
        // signal and outlive the group.
        let script = format!("sleep 600 & echo $$ > '{}'; wait", ready.path().display());
        setup.write_config(&json!({
            "model": "acme/m1",
            "providers": {"acme": {"credentials": {"default": {
                "command": ["sh", "-c", script],
            }}}},
        }));
        let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
        let reads = Arc::new(crate::switch::Reads::default());
        Self {
            setup,
            ready,
            reads,
            clock,
        }
    }

    /// The terminal's picker read over the process's shared reads.
    fn read(&self) -> tui::ReadModels {
        reader(
            self.setup.home(),
            self.setup.workspace(),
            Arc::clone(&self.clock),
            Arc::new(tools::PathLocks::new()),
            Arc::clone(&self.reads),
        )
    }
}

/// A picker read the process cancels returns and kills its group: once the
/// command's group id arrives the test cancels the shared reads, and the
/// stale read ends with the cached list kept and no refresh.
#[test]
fn a_picker_read_cancelled_by_the_process_reads_returns_and_kills_its_group() {
    let fixture = CommandFixture::new("fiber-model-list-cancel");
    // Before the read: the group is known only after the command starts,
    // so a command-line watchdog covers a spawn no later watchdog can
    // reach.
    let pre = fakes::Watchdog::matching(&fixture.ready.path().display().to_string());
    let (done, finished) = std::sync::mpsc::channel();
    let read = fixture.read();
    std::thread::spawn(move || {
        done.send(read(tui::Refresh::Stale)).unwrap_or(());
    });
    // The first line proves the command ran: without it the cancel below
    // would end nothing and the read would hang.
    let pgid = fixture.ready.wait(fakes::Deadline::after(DEADLINE))[0];
    let watchdog = fakes::Watchdog::group(pgid);
    fixture.reads.cancel();
    let catalogue = fakes::Deadline::after(DEADLINE)
        .recv(&finished)
        .unwrap()
        .unwrap();
    assert!(
        catalogue
            .models
            .iter()
            .any(|entry| entry.reference == "acme/m1"),
        "the cached list is kept: {catalogue:?}"
    );
    assert!(
        fakes::group_empties(pgid, fakes::Deadline::after(DEADLINE)),
        "the cancel killed the group"
    );
    watchdog.stand_down(fakes::Deadline::after(DEADLINE));
    pre.stand_down(fakes::Deadline::after(DEADLINE));
}

/// Two picker reads share one cancel: past the process's cancel a second
/// read starts no command and answers the cached list at once. Had the
/// reader built its own reads, the 600 s command would run and the bounded
/// receive below would fail.
#[test]
fn two_picker_reads_share_one_cancel() {
    let fixture = CommandFixture::new("fiber-model-list-shared");
    let cache = fixture.setup.home().join("cache/models/acme.json");
    let dated = std::fs::metadata(&cache).unwrap().modified().unwrap();
    fixture.reads.cancel();
    let (done, finished) = std::sync::mpsc::channel();
    let read = fixture.read();
    std::thread::spawn(move || {
        done.send(read(tui::Refresh::Stale)).unwrap_or(());
    });
    let catalogue = fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&finished)
        .unwrap()
        .unwrap();
    assert!(
        catalogue
            .models
            .iter()
            .any(|entry| entry.reference == "acme/m1"),
        "the cached list is kept: {catalogue:?}"
    );
    assert_eq!(
        std::fs::metadata(&cache).unwrap().modified().unwrap(),
        dated,
        "no refresh rewrote the cache"
    );
}

/// A placeholder with no value leaves one group and no notice: the
/// models stay out of the list, and the group carries their names, the
/// raw extension, and the file built from it.
#[test]
fn a_placeholder_with_no_value_leaves_a_group_and_no_notice() {
    let setup = Setup::new("fiber-model-list-left-out");
    setup.install_placeholder_data("acme-ext", "acme", "workspace", json!({}));
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert!(catalogue.models.is_empty(), "{:?}", catalogue.models);
    assert!(
        catalogue.notices.is_empty(),
        "no Missing notice at launch: {:?}",
        catalogue.notices
    );
    assert_eq!(catalogue.left_out.len(), 1, "{:?}", catalogue.left_out);
    let group = &catalogue.left_out[0];
    assert_eq!(group.provider, "acme");
    assert_eq!(group.extension, "acme-ext");
    assert_eq!(group.file, "config/acme-ext.json");
    assert_eq!(group.setting, "workspace");
    assert!(group.login);
    assert_eq!(
        group
            .models
            .iter()
            .map(|model| (model.id.as_str(), model.name.as_deref()))
            .collect::<Vec<_>>(),
        [("m1", Some("Acme One")), ("m2", None)]
    );
}

/// A first-party extension's group carries its full name while its file
/// is the short directory name: no path is derived from the provider.
#[test]
fn a_first_party_group_carries_its_full_name_and_short_file() {
    let setup = Setup::new("fiber-model-list-first-party");
    // A declared credential path switches the `credential_label`
    // provider on; stored logins do not count for it.
    let creds = setup.home().join("opencode-creds");
    std::fs::write(&creds, "").unwrap();
    setup.install_placeholder_data(
        "github.com/aakshintala/fiber/providers/opencode",
        "opencode",
        "workspace",
        json!({"credential_label": {"names": "profile", "default": "the environment",
            "credential_paths": [{"path": creds.to_str().unwrap()}]}}),
    );
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert!(catalogue.models.is_empty(), "{:?}", catalogue.models);
    assert!(
        catalogue.notices.is_empty(),
        "no Missing notice at launch: {:?}",
        catalogue.notices
    );
    assert_eq!(catalogue.left_out.len(), 1, "{:?}", catalogue.left_out);
    let group = &catalogue.left_out[0];
    assert_eq!(group.provider, "opencode");
    assert_eq!(
        group.extension,
        "github.com/aakshintala/fiber/providers/opencode"
    );
    assert_eq!(group.file, "config/opencode.json");
    assert_eq!(group.setting, "workspace");
    assert!(!group.login, "a credential label reads no stored login");
    assert_eq!(
        group
            .models
            .iter()
            .map(|model| (model.id.as_str(), model.name.as_deref()))
            .collect::<Vec<_>>(),
        [("m1", Some("Acme One")), ("m2", None)]
    );
}

/// A value that is not a host still reads its per-model notice at
/// launch, and leaves no group.
#[test]
fn a_value_that_is_not_a_host_keeps_its_notice_and_leaves_no_group() {
    let setup = Setup::new("fiber-model-list-not-host");
    setup.install_placeholder_data("acme-ext", "acme", "workspace", json!({}));
    setup.write_setting("acme-ext", "workspace", "x/y");
    let called = Cell::new(false);
    let catalogue = read(
        &setup.home(),
        &setup.workspace(),
        tui::Refresh::Cached,
        now(),
        &unloaded(&called),
        &fakes::children::grouped_output,
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    assert!(catalogue.models.is_empty(), "{:?}", catalogue.models);
    assert!(
        catalogue.left_out.is_empty(),
        "a not-a-host value groups nothing: {:?}",
        catalogue.left_out
    );
    assert_eq!(catalogue.notices.len(), 2, "{:?}", catalogue.notices);
    for (notice, id) in catalogue.notices.iter().zip(["acme/m1", "acme/m2"]) {
        assert!(notice.message.contains(id), "{:?}", catalogue.notices);
        assert!(
            notice.message.contains("is not a host"),
            "{:?}",
            catalogue.notices
        );
        assert_eq!(notice.code, contract::ErrorCode::ModelUnconfigured);
    }
}
