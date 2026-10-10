//! The installed models' lists: the cached discovery data as the picker
//! lists it, refreshed in the background (`docs/model-routing.md`,
//! "Model discovery"). Tests never touch the real home and never load an
//! extension except through the `load` they pass.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

use crate::test_support::write_record;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use config::Config;
use contract::clock::Clock;
use extensions::{Providers, SessionExtensions};
use serde_json::{Value, json};

use super::{ModelLists, ModelListsError, model_lists, roles_of};

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
        let dir = self.home().join("extensions").join(extension);
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
        std::fs::create_dir_all(dir.join("providers")).unwrap();
        std::fs::write(
            dir.join("extension.json"),
            json!({
                "name": extension,
                "version": "v1.0.0",
                "fiber": "0.1.0",
                "api": extensions::API,
            })
            .to_string(),
        )
        .unwrap();
        write_record(&dir);
        std::fs::write(
            dir.join("providers").join(format!("{name}.json")),
            json!({"name": name, "models": models}).to_string(),
        )
        .unwrap();
    }

    /// Installs a Lua extension with this `init.lua`, the way
    /// `fiber extension install <path>` does.
    fn install_lua(&self, extension: &str, init: &str) {
        let source = self.root.path().join("src").join(extension);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("extension.json"),
            json!({
                "name": extension,
                "version": "v1.0.0",
                "fiber": "0.1.0",
                "api": extensions::API,
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(source.join("init.lua"), init).unwrap();
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn write_config(&self, value: &Value) {
        std::fs::write(self.home().join("config.json"), value.to_string()).unwrap();
    }

    fn write_cache(&self, name: &str, ids: &[&str]) {
        let models: Vec<Value> = ids
            .iter()
            .map(|id| {
                json!({
                    "id": id,
                    "protocol": "openai-responses",
                    "base_url": "http://127.0.0.1:1/v1",
                    "context_window": 1000,
                })
            })
            .collect();
        config::write_model_cache(&self.home(), name, &Value::Array(models)).unwrap();
    }
}

/// `model_lists` never holds a lock: the loader in these tests runs
/// straight through.
struct NoLock;

impl contract::files::PathLock for NoLock {
    fn hold(&self, _path: &Path, run: &mut dyn FnMut()) {
        run();
    }

    fn hold_all(&self, _paths: &[PathBuf], run: &mut dyn FnMut()) {
        run();
    }
}

/// Loads no extension and records that nothing was loaded.
fn unloaded(called: &Cell<bool>) -> impl Fn(&Config) -> SessionExtensions + '_ {
    |_| {
        called.set(true);
        SessionExtensions::default()
    }
}

/// Loads what is installed in `home`, on `clock`.
fn installed<'a>(
    home: &'a Path,
    clock: &Arc<fakes::clock::FakeClock>,
) -> impl Fn(&Config) -> SessionExtensions + 'a {
    let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
    let clock: Arc<dyn Clock> = Arc::clone(clock) as Arc<dyn Clock>;
    move |config: &Config| {
        SessionExtensions::load(home, config, Arc::clone(&clock), Arc::clone(&locks), None)
    }
}

/// Sets the cached list's mtime `ago` before the fake clock's wall.
fn age_cache(setup: &Setup, name: &str, ago: Duration) {
    let clock = fakes::clock::FakeClock::new();
    let file = setup
        .home()
        .join("cache/models")
        .join(format!("{name}.json"));
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(clock.wall() - ago)
        .unwrap();
}

/// The listed models and notices: the text `model_lists` read, without
/// the picker's entries.
struct Listed {
    models: Vec<String>,
    notices: Vec<String>,
}

/// One `{name}/{id}` reference per model of every installed provider,
/// providers sorted by name, models in list order: the walk the picker's
/// entries make.
fn listed((providers, _config, notices): (Providers, Config, Vec<String>)) -> Listed {
    let mut models = Vec::new();
    for name in providers.names() {
        let Some(data) = providers.get(name) else {
            continue;
        };
        for model in &data.models {
            models.push(format!("{name}/{}", model.id));
        }
    }
    Listed { models, notices }
}

fn references(listed: &Listed) -> Vec<&str> {
    listed.models.iter().map(String::as_str).collect()
}

/// One named wall-clock deadline for every blocking read.
const DEADLINE: Duration = Duration::from_secs(20);

/// `model_lists` with a refresh on its own thread under [`DEADLINE`]:
/// it joins discovery threads (`docs/testing.md`, "Waits and timeouts").
fn bounded(
    setup: &Setup,
    clock: &Arc<fakes::clock::FakeClock>,
    read: ModelLists,
) -> Result<Listed, String> {
    let home = setup.home();
    let workspace = setup.workspace();
    let clock = Arc::clone(clock);
    fakes::within("the model list read", DEADLINE, move || {
        let load = installed(home.as_path(), &clock);
        match model_lists(&home, &workspace, read, &load) {
            Ok(loaded) => Ok(listed(loaded)),
            Err(error) => Err(error.to_string()),
        }
    })
}

/// A `models()` listing response with these ids.
fn listing(ids: &[&str]) -> fakes::Response {
    let data: Vec<_> = ids
        .iter()
        .map(|id| json!({ "id": id, "context_length": 1000 }))
        .collect();
    fakes::Response::status(200, json!({ "data": data }).to_string())
}

/// A Lua provider with a dummy credential and a `models()` reading the
/// fake server at `base`: a non-200 answer fails the discovery, keeping
/// the old cache (`docs/model-routing.md`, "Model discovery").
fn server_models(base: &str) -> String {
    format!(
        r#"
fiber.provider("acme", {{
  credential = {{ timeout = 60000, run = function(who)
    return {{ token = "t", expires_at = 4102444800 }}
  end }},
  models = {{ timeout = 60000, run = function()
    local reply = host.http({{
      url = "{base}/v1/models",
      headers = {{ authorization = "Bearer k" }},
    }})
    if reply.status ~= 200 then
      error("the list moved")
    end
    local list = {{}}
    for _, m in ipairs(json.decode(reply.body).data) do
      list[#list + 1] = {{
        id = m.id,
        protocol = "openai-responses",
        base_url = "{base}/v1",
        context_window = m.context_length,
      }}
    end
    return list
  end }},
}})
"#,
    )
}

#[test]
fn a_cached_read_lists_every_installed_model_sorted_by_provider() {
    let setup = Setup::new("fiber-model-list-sorted");
    setup.install_data("zeta-ext", "zeta", &["z2", "z1"], &json!({}));
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    let called = Cell::new(false);
    let catalogue = listed(
        model_lists(
            &setup.home(),
            &setup.workspace(),
            ModelLists::Cached,
            &unloaded(&called),
        )
        .unwrap(),
    );
    assert!(!called.get(), "a Cached read starts no extension");
    assert_eq!(references(&catalogue), ["acme/m1", "zeta/z2", "zeta/z1"]);
    assert!(catalogue.notices.is_empty(), "{:?}", catalogue.notices);
}

#[test]
fn a_cached_list_replaces_the_data_files_models() {
    let setup = Setup::new("fiber-model-list-cache");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    setup.write_cache("acme", &["m2"]);
    let called = Cell::new(false);
    let catalogue = listed(
        model_lists(
            &setup.home(),
            &setup.workspace(),
            ModelLists::Cached,
            &unloaded(&called),
        )
        .unwrap(),
    );
    assert_eq!(references(&catalogue), ["acme/m2"]);
}

#[test]
fn an_object_role_marks_by_its_model() {
    let setup = Setup::new("fiber-model-list-object-role");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    setup.write_config(&json!({
        "roles": {"fast": {"model": "fiber:acme/m1"}},
    }));
    let called = Cell::new(false);
    let (_providers, config, _notices) = model_lists(
        &setup.home(),
        &setup.workspace(),
        ModelLists::Cached,
        &unloaded(&called),
    )
    .unwrap();
    assert_eq!(roles_of(&config, "acme/m1"), ["fast"]);
}

#[test]
fn a_role_of_another_harness_marks_nothing() {
    let setup = Setup::new("fiber-model-list-foreign-role");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    setup.write_config(&json!({
        "roles": {"a": "other:acme/m1", "b": "acme/m1", "c": "fiber:other/m2"},
    }));
    let called = Cell::new(false);
    let (_providers, config, _notices) = model_lists(
        &setup.home(),
        &setup.workspace(),
        ModelLists::Cached,
        &unloaded(&called),
    )
    .unwrap();
    assert!(roles_of(&config, "acme/m1").is_empty());
}

#[test]
fn roles_of_reads_exact_then_stripped() {
    let setup = Setup::new("fiber-model-list-roles-of");
    setup.write_config(&json!({
        "roles": {"fast": "fiber:acme/m1", "deep": {"model": "fiber:acme/m1:low"}},
    }));
    let config = Config::load(config::Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: config::ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    assert_eq!(roles_of(&config, "acme/m1"), ["deep", "fast"]);
    assert!(roles_of(&config, "acme/m2").is_empty());
}

#[test]
fn a_cached_read_loads_no_extension() {
    let setup = Setup::new("fiber-model-list-no-load");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    let called = Cell::new(false);
    model_lists(
        &setup.home(),
        &setup.workspace(),
        ModelLists::Cached,
        &unloaded(&called),
    )
    .unwrap();
    assert!(!called.get());
}

#[test]
fn stale_refreshes_with_the_age_check_and_every_without() {
    let setup = Setup::new("fiber-model-list-stale");
    let server =
        fakes::ProviderServer::start_with_fallback([listing(&["m1"])], listing(&["m1", "m2"]))
            .unwrap();
    setup.install_lua("acme-ext", &server_models(&server.url()));
    let clock = Arc::new(fakes::clock::FakeClock::new());
    // No cached copy: the first read runs `models()` at once.
    let catalogue = bounded(&setup, &clock, ModelLists::Refresh { check_age: true }).unwrap();
    assert_eq!(references(&catalogue), ["acme/m1"]);
    assert_eq!(server.requests().len(), 1);
    // A fresh list is not stale: no refresh runs.
    let catalogue = bounded(&setup, &clock, ModelLists::Refresh { check_age: true }).unwrap();
    assert_eq!(references(&catalogue), ["acme/m1"]);
    assert_eq!(server.requests().len(), 1);
    // An old list refreshes with the age check, and the refreshed list
    // replaces the cached one.
    age_cache(&setup, "acme", Duration::from_secs(25 * 60 * 60));
    let catalogue = bounded(&setup, &clock, ModelLists::Refresh { check_age: true }).unwrap();
    assert_eq!(references(&catalogue), ["acme/m1", "acme/m2"]);
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn every_refreshes_whatever_the_cache_holds() {
    let setup = Setup::new("fiber-model-list-every");
    let server =
        fakes::ProviderServer::start_with_fallback([listing(&["m1"])], listing(&["m1", "m2"]))
            .unwrap();
    setup.install_lua("acme-ext", &server_models(&server.url()));
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let catalogue = bounded(&setup, &clock, ModelLists::Refresh { check_age: true }).unwrap();
    assert_eq!(references(&catalogue), ["acme/m1"]);
    // The cache is fresh, but `Every` refreshes anyway, and the
    // refreshed list replaces the cached one.
    let catalogue = bounded(&setup, &clock, ModelLists::Refresh { check_age: false }).unwrap();
    assert_eq!(references(&catalogue), ["acme/m1", "acme/m2"]);
}

#[test]
fn a_failed_refresh_is_a_notice_and_the_cached_list_stays() {
    let setup = Setup::new("fiber-model-list-failed");
    // The script holds one answer: the refresh past it fails.
    let server = fakes::ProviderServer::start([listing(&["m1"])]).unwrap();
    setup.install_lua("acme-ext", &server_models(&server.url()));
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let catalogue = bounded(&setup, &clock, ModelLists::Refresh { check_age: true }).unwrap();
    assert_eq!(references(&catalogue), ["acme/m1"]);
    age_cache(&setup, "acme", Duration::from_secs(25 * 60 * 60));
    let catalogue = bounded(&setup, &clock, ModelLists::Refresh { check_age: true }).unwrap();
    assert_eq!(references(&catalogue), ["acme/m1"]);
    assert!(
        catalogue
            .notices
            .iter()
            .any(|notice| notice.contains("Could not refresh acme's model list")),
        "{:?}",
        catalogue.notices
    );
}

#[test]
fn a_cached_lua_only_provider_lists_without_loading() {
    let setup = Setup::new("fiber-model-list-lua-only");
    let server = fakes::ProviderServer::start([listing(&["m1"])]).unwrap();
    setup.install_lua("acme-ext", &server_models(&server.url()));
    let clock = Arc::new(fakes::clock::FakeClock::new());
    let catalogue = bounded(&setup, &clock, ModelLists::Refresh { check_age: true }).unwrap();
    assert_eq!(references(&catalogue), ["acme/m1"]);
    // The list is cached now: a `Cached` read serves it with no extension.
    let called = Cell::new(false);
    let catalogue = listed(
        model_lists(
            &setup.home(),
            &setup.workspace(),
            ModelLists::Cached,
            &unloaded(&called),
        )
        .unwrap(),
    );
    assert!(!called.get());
    assert_eq!(references(&catalogue), ["acme/m1"]);
}

#[test]
fn a_provider_with_a_data_file_is_not_listed_twice() {
    let setup = Setup::new("fiber-model-list-twice");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    setup.write_cache("acme", &["m2"]);
    let called = Cell::new(false);
    let catalogue = listed(
        model_lists(
            &setup.home(),
            &setup.workspace(),
            ModelLists::Cached,
            &unloaded(&called),
        )
        .unwrap(),
    );
    assert_eq!(references(&catalogue), ["acme/m2"]);
}

#[test]
fn a_held_providers_cached_invalid_model_is_not_reported_twice() {
    let setup = Setup::new("fiber-model-list-cached-invalid");
    setup.install_data("acme-ext", "acme", &["m1"], &json!({}));
    config::write_model_cache(
        &setup.home(),
        "acme",
        &json!([{
            "id": "m1",
            "protocol": "openai-responses",
            "base_url": "http://127.0.0.1:1/v1",
            "context_window": 0,
        }]),
    )
    .unwrap();
    let called = Cell::new(false);
    let catalogue = listed(
        model_lists(
            &setup.home(),
            &setup.workspace(),
            ModelLists::Cached,
            &unloaded(&called),
        )
        .unwrap(),
    );
    assert!(catalogue.models.is_empty());
    assert_eq!(catalogue.notices.len(), 1, "{:?}", catalogue.notices);
    assert!(catalogue.notices[0].contains("acme/m1"));
    assert!(catalogue.notices[0].contains("context_window"));
}

#[test]
fn the_scripted_provider_is_never_listed() {
    let setup = Setup::new("fiber-model-list-scripted");
    setup.write_cache("scripted", &["m1"]);
    let called = Cell::new(false);
    let catalogue = listed(
        model_lists(
            &setup.home(),
            &setup.workspace(),
            ModelLists::Cached,
            &unloaded(&called),
        )
        .unwrap(),
    );
    assert!(catalogue.models.is_empty(), "{:?}", catalogue.models);
}

#[test]
fn load_notices_reach_the_lists() {
    let setup = Setup::new("fiber-model-list-notices");
    let dir = setup.home().join("extensions").join("old-ext");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("extension.json"),
        json!({
            "name": "old-ext",
            "version": "v1.0.0",
            "fiber": "0.1.0",
            "api": extensions::API + 1,
        })
        .to_string(),
    )
    .unwrap();
    write_record(&dir);
    let called = Cell::new(false);
    let catalogue = listed(
        model_lists(
            &setup.home(),
            &setup.workspace(),
            ModelLists::Cached,
            &unloaded(&called),
        )
        .unwrap(),
    );
    assert!(catalogue.models.is_empty());
    assert_eq!(catalogue.notices.len(), 1, "{:?}", catalogue.notices);
    assert!(
        catalogue.notices[0].contains("extension API"),
        "{:?}",
        catalogue.notices
    );
}

#[test]
fn a_load_failure_names_what_todays_read_named() {
    let setup = Setup::new("fiber-model-list-load-error");
    std::fs::write(setup.home().join("config.json"), "not json").unwrap();
    let called = Cell::new(false);
    let error = model_lists(
        &setup.home(),
        &setup.workspace(),
        ModelLists::Cached,
        &unloaded(&called),
    )
    .expect_err("an unreadable configuration reads no list");
    assert!(matches!(error, ModelListsError::Load(_)), "{error:?}");
    assert!(error.to_string().contains("is not valid JSON"), "{error}",);
    assert!(!error.to_string().contains("not json"), "{error}");
    assert_eq!(error.code(), contract::ErrorCode::ConfigInvalid);
}

#[test]
fn an_unreadable_cached_list_fails_the_read() {
    let setup = Setup::new("fiber-model-list-cache-error");
    // A file where the cached lists live: no directory reads as one.
    std::fs::create_dir_all(setup.home().join("cache")).unwrap();
    std::fs::write(setup.home().join("cache/models"), "not a directory").unwrap();
    let called = Cell::new(false);
    let error = model_lists(
        &setup.home(),
        &setup.workspace(),
        ModelLists::Cached,
        &unloaded(&called),
    )
    .expect_err("an unreadable cached list reads no list");
    assert!(matches!(error, ModelListsError::Config(_)), "{error:?}");
    assert!(error.to_string().contains("cache/models"), "{error}",);
    assert_eq!(error.code(), contract::ErrorCode::IoFailed);
}

#[test]
fn a_placeholder_failure_keeps_its_code_and_text() {
    let error = ModelListsError::Placeholders(extensions::Error::BadVersion {
        text: "1.x".to_owned(),
    });
    assert!(!matches!(error, ModelListsError::Load(_)), "{error:?}");
    assert!(!matches!(error, ModelListsError::Config(_)), "{error:?}");
    assert_eq!(error.to_string(), "`1.x` is not a version such as `0.3.0`.",);
    assert_eq!(error.code(), contract::ErrorCode::ConfigInvalid);
}
