//! The hub's `models` operation: the hub's own catalogue, read the way
//! the local picker reads it, through the real seam.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

use std::path::Path;
use std::time::{Duration, SystemTime};

use contract::ErrorCode;
use contract::clock::Clock;
use hub::Machine;
use serde_json::{Value, json};

use crate::configure::Seam;
use crate::test_support::{Rig, install_extension};
use fakes::Deadline;

/// One named wall-clock deadline for every blocking read: a refresh
/// joins its discovery threads before answering (`docs/testing.md`,
/// "Waits and timeouts").
const DEADLINE: Duration = Duration::from_secs(20);

/// Installs a data-only extension registering `name` with these model
/// ids, each on `openai-responses` with `extra` merged into each model.
/// A positive fixture is logged in: the presence gate needs a stored
/// login for its models to list.
fn install_data(home: &Path, extension: &str, name: &str, ids: &[&str], extra: &Value) {
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
        home,
        &format!("extensions/{extension}"),
        json!({
            "name": extension,
            "version": "v1.0.0",
            "fiber": "0.1.0",
            "api": extensions::API,
        }),
        &[(name, json!({"name": name, "models": models}))],
    );
    config::store_credential(home, name, "default", &config::Secret::new("test".into())).unwrap();
}

/// Installs a data-only extension `extension` registering provider
/// `name` with `models` on `{setting}` and that one placeholder
/// declared, the `install_data` layout with separate extension and
/// provider names. `credential` is merged into the provider data, so a
/// `credential_label` reads a login no `fiber login` asks for. The
/// directory is the extension's short name, as on disk. A positive
/// fixture is logged in: the presence gate needs a stored login for
/// discovery to run, which `credential_label` providers ignore.
fn install_placeholder(
    home: &Path,
    extension: &str,
    name: &str,
    setting: &str,
    models: Value,
    credential: &Value,
) {
    let mut data = json!({"name": name, "models": models});
    data["placeholders"] = Value::Object([(setting.to_owned(), json!({}))].into_iter().collect());
    for (key, value) in credential.as_object().cloned().unwrap_or_default() {
        data[key] = value;
    }
    install_extension(
        home,
        &format!("extensions/{}", config::dir_name(extension)),
        json!({
            "name": extension,
            "version": "v1.0.0",
            "fiber": "0.1.0",
            "api": extensions::API,
        }),
        &[(name, data)],
    );
    config::store_credential(home, name, "default", &config::Secret::new("test".into())).unwrap();
}

/// Installs a Lua extension with this `init.lua`, the way
/// `fiber extension install <path>` does. A positive fixture is logged
/// in: the presence gate needs a stored login for discovery to run.
fn install_lua(root: &Path, home: &Path, extension: &str, init: &str, providers: &Value) {
    let source = root.join("src").join(extension);
    std::fs::create_dir_all(&source).unwrap();
    let mut manifest = json!({
        "name": extension,
        "version": "v1.0.0",
        "fiber": "0.1.0",
        "api": extensions::API,
    });
    manifest["providers"] = providers.clone();
    std::fs::write(source.join("extension.json"), manifest.to_string()).unwrap();
    std::fs::create_dir_all(source.join("providers")).unwrap();
    std::fs::write(source.join("providers/acme.lua"), init).unwrap();
    extensions::plan(
        home,
        &extensions::Request::Path(source),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    config::store_credential(home, "acme", "default", &config::Secret::new("test".into())).unwrap();
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
/// fake server at `base`.
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

/// Writes `name`'s cached list with these ids, served from `base`: a
/// cached model whose `base_url` no installed extension declares is
/// left out as unlisted, so the cache must name the Lua provider's
/// own server.
fn write_cache(home: &Path, name: &str, base: &str, ids: &[&str]) {
    let models: Vec<Value> = ids
        .iter()
        .map(|id| {
            json!({
                "id": id,
                "protocol": "openai-responses",
                "base_url": format!("{base}/v1"),
                "context_window": 1000,
            })
        })
        .collect();
    config::write_model_cache(home, name, &Value::Array(models)).unwrap();
}

/// Dates `name`'s cached list `age` before the injected clock's `wall`:
/// the wall is the fake clock's, never the copied epoch, so the age
/// check measures against the clock the read uses.
fn date_cache(home: &Path, name: &str, age: Duration, wall: SystemTime) {
    let file = home.join("cache/models").join(format!("{name}.json"));
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(wall.checked_sub(age).unwrap())
        .unwrap();
}

fn call(
    seam: &Seam,
    operation: &str,
    args: Value,
) -> Result<Option<Value>, contract::shapes::Failure> {
    seam.configure(
        operation,
        args.as_object().unwrap(),
        fakes::clock::FakeClock::new(),
    )
}

fn result(seam: &Seam, operation: &str, args: Value) -> Value {
    call(seam, operation, args).unwrap().unwrap()
}

#[test]
fn hub_models_carries_installable_from_its_home() {
    let hub = Rig::new("fiber-hub-models-installable");
    install_data(&hub.home, "custom", "openai", &["m"], &json!({}));
    let seam = Seam::new(hub.home.clone());
    let catalogue = result(&seam, "models", json!({"workspace": hub.workspace}));
    assert_eq!(
        catalogue["installable"],
        json!(extensions::not_installed_providers(&hub.home).unwrap())
    );
    let offered = catalogue["installable"].as_array().unwrap();
    assert!(offered.contains(&json!("anthropic")));
    assert!(!offered.contains(&json!("openai")));
    assert!(!offered.contains(&json!("scripted")));
}

fn references(catalogue: &Value) -> Vec<&str> {
    catalogue["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["reference"].as_str().unwrap())
        .collect()
}

/// The hub's `models` lists the hub's own catalogue: the hub home's
/// models, and none of the other home's.
#[test]
fn hub_models_lists_the_hub_catalogue_and_none_of_the_clients() {
    let hub = Rig::new("fiber-hub-models-catalogue");
    let client = Rig::new("fiber-hub-models-client");
    install_data(&hub.home, "acme-ext", "acme", &["m1", "m2"], &json!({}));
    install_data(&client.home, "zed-ext", "zed", &["z1"], &json!({}));
    // An extension written for another API loads no provider: its
    // notice rides the answer as a `code` and `message` object, with
    // `extension` naming it.
    install_extension(
        &hub.home,
        "extensions/old-ext",
        json!({
            "name": "old-ext",
            "version": "v1.0.0",
            "fiber": "0.1.0",
            "api": extensions::API + 1,
        }),
        &[],
    );
    let seam = Seam::new(hub.home.clone());
    let catalogue = result(&seam, "models", json!({"workspace": hub.workspace}));
    assert_eq!(references(&catalogue), ["acme/m1", "acme/m2"]);
    assert_eq!(
        catalogue["lists"],
        json!([{"provider": "acme", "stale": true}]),
        "no cached copy: no time, and stale"
    );
    assert_eq!(
        catalogue["notices"],
        json!([{
            "code": "extension_incompatible",
            "message": format!(
                "`old-ext` was written for extension API {}, and this Fiber speaks API {}.",
                extensions::API + 1,
                extensions::API,
            ),
            "extension": "old-ext",
        }]),
        "one notice object with its extension: {catalogue:?}"
    );
}

/// A model with a cost carries its price field for field; a model with
/// no cost carries no `price` key, and a model with no name no `name`
/// key. Absent, never `null`.
#[test]
fn hub_models_encodes_price_name_and_absent_fields() {
    let rig = Rig::new("fiber-hub-models-shape");
    install_extension(
        &rig.home,
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
                {"id": "m1", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1", "context_window": 1000,
                 "name": "Acme One",
                 "cost": {"input": 3.0, "output": 15.0, "tiers": [
                    {"input_tokens_above": 100000, "input": 4.0, "output": 20.0,
                     "cache_read": 1.0, "cache_write": 1.5},
                 ]}},
                {"id": "m2", "protocol": "openai-responses",
                 "base_url": "http://127.0.0.1:1/v1", "context_window": 1000},
            ]}),
        )],
    );
    // Logged in: the presence gate needs a stored login for the models
    // to list.
    config::store_credential(
        &rig.home,
        "acme",
        "default",
        &config::Secret::new("test".into()),
    )
    .unwrap();
    let seam = Seam::new(rig.home.clone());
    let catalogue = result(&seam, "models", json!({"workspace": rig.workspace}));
    let models = catalogue["models"].as_array().unwrap();
    assert_eq!(models.len(), 2);
    let priced = &models[0];
    assert_eq!(priced["reference"], json!("acme/m1"));
    assert_eq!(priced["name"], json!("Acme One"));
    // No `cache_write`: the input price stands in; the tier's own
    // `cache_write` names its rebuild price.
    assert_eq!(
        priced["price"],
        json!({"micros_per_mtok": 3_000_000, "input": 3_000_000,
               "output": 15_000_000,
               "tiers": [{"input_tokens_above": 100_000,
                          "micros_per_mtok": 1_500_000,
                          "input": 4_000_000, "output": 20_000_000}]}),
    );
    let plain = &models[1];
    assert_eq!(plain["reference"], json!("acme/m2"));
    for key in ["name", "price", "default_level", "configured"] {
        assert!(
            plain.get(key).is_none(),
            "{key} is absent, never null: {plain}"
        );
        assert!(
            priced.get(key).is_none() || key == "name" || key == "price",
            "{key} is absent, never null: {priced}"
        );
    }
}

/// The default refresh is `cached`: with a Lua provider that would
/// serve a different list, the answer equals the cached list and the
/// provider is never read.
#[test]
fn hub_models_defaults_to_the_cached_list() {
    let rig = Rig::new("fiber-hub-models-cached");
    let server = fakes::ProviderServer::start([listing(&["served"])]).unwrap();
    install_lua(
        rig.root.path(),
        &rig.home,
        "acme-ext",
        &server_models(&server.url()),
        &json!({"acme": [format!("{}/v1", server.url())]}),
    );
    write_cache(&rig.home, "acme", &server.url(), &["cached"]);
    // A fresh cache, dated against the injected clock's wall: an age
    // check against the real clock would read it stale and refresh.
    let wall = fakes::clock::FakeClock::new().wall();
    date_cache(&rig.home, "acme", Duration::from_secs(90), wall);
    let seam = Seam::new(rig.home.clone());
    let catalogue = result(&seam, "models", json!({"workspace": rig.workspace}));
    assert_eq!(references(&catalogue), ["acme/cached"]);
    assert!(
        server.requests().is_empty(),
        "a Cached read starts no extension"
    );
}

/// An explicit `refresh: "cached"` is the same read as the default: the
/// cached list is served and the Lua provider is never read.
#[test]
fn hub_models_explicit_cached_serves_the_cached_list() {
    let rig = Rig::new("fiber-hub-models-explicit-cached");
    let server = fakes::ProviderServer::start([listing(&["served"])]).unwrap();
    install_lua(
        rig.root.path(),
        &rig.home,
        "acme-ext",
        &server_models(&server.url()),
        &json!({"acme": [format!("{}/v1", server.url())]}),
    );
    write_cache(&rig.home, "acme", &server.url(), &["cached"]);
    let wall = fakes::clock::FakeClock::new().wall();
    date_cache(&rig.home, "acme", Duration::from_secs(90), wall);
    let seam = Seam::new(rig.home.clone());
    let catalogue = result(
        &seam,
        "models",
        json!({"workspace": rig.workspace, "refresh": "cached"}),
    );
    assert_eq!(references(&catalogue), ["acme/cached"]);
    assert!(
        server.requests().is_empty(),
        "a Cached read starts no extension"
    );
}

/// `refresh: "every"` runs the refresh: the answer is the Lua
/// provider's list, and its list time is the injected clock's wall, so
/// the refresh ran and the model came from the hub's home.
#[test]
fn hub_models_refresh_every_runs_the_refresh() {
    let rig = Rig::new("fiber-hub-models-every");
    let server = fakes::ProviderServer::start([listing(&["served"])]).unwrap();
    install_lua(
        rig.root.path(),
        &rig.home,
        "acme-ext",
        &server_models(&server.url()),
        &json!({"acme": [format!("{}/v1", server.url())]}),
    );
    write_cache(&rig.home, "acme", &server.url(), &["cached"]);
    let home = rig.home.clone();
    let workspace = rig.workspace.clone();
    let clock = fakes::clock::FakeClock::new();
    let wall_ms = contract::clock::wall_ms(clock.wall());
    let catalogue = fakes::within("the every refresh", Deadline::after(DEADLINE), move || {
        let seam = Seam::new(home);
        seam.configure(
            "models",
            json!({"workspace": workspace, "refresh": "every"})
                .as_object()
                .unwrap(),
            clock,
        )
        .unwrap()
        .unwrap()
    });
    assert_eq!(references(&catalogue), ["acme/served"]);
    assert_eq!(
        catalogue["lists"],
        json!([{"provider": "acme", "updated_ms": wall_ms, "stale": false}]),
    );
    assert_eq!(server.requests().len(), 1);
}

/// `refresh: "stale"` keeps a fresh cached list and refreshes a stale
/// one: the fresh case reads no provider, the stale case answers the
/// Lua provider's list.
#[test]
fn hub_models_refresh_stale_checks_the_age() {
    let rig = Rig::new("fiber-hub-models-stale");
    let server =
        fakes::ProviderServer::start_with_fallback([listing(&["m1"])], listing(&["m1", "m2"]))
            .unwrap();
    install_lua(
        rig.root.path(),
        &rig.home,
        "acme-ext",
        &server_models(&server.url()),
        &json!({"acme": [format!("{}/v1", server.url())]}),
    );
    write_cache(&rig.home, "acme", &server.url(), &["cached"]);
    let wall = fakes::clock::FakeClock::new().wall();
    // A fresh list stays: no refresh runs.
    date_cache(&rig.home, "acme", Duration::from_secs(90), wall);
    let home = rig.home.clone();
    let workspace = rig.workspace.clone();
    let catalogue = fakes::within(
        "the fresh stale refresh",
        Deadline::after(DEADLINE),
        move || {
            let seam = Seam::new(home);
            seam.configure(
                "models",
                json!({"workspace": workspace, "refresh": "stale"})
                    .as_object()
                    .unwrap(),
                fakes::clock::FakeClock::new(),
            )
            .unwrap()
            .unwrap()
        },
    );
    assert_eq!(references(&catalogue), ["acme/cached"]);
    assert!(
        server.requests().is_empty(),
        "a fresh list is not refreshed"
    );
    // A stale list refreshes with the age check.
    date_cache(&rig.home, "acme", Duration::from_secs(25 * 60 * 60), wall);
    let home = rig.home.clone();
    let workspace = rig.workspace.clone();
    let catalogue = fakes::within("the stale refresh", Deadline::after(DEADLINE), move || {
        let seam = Seam::new(home);
        seam.configure(
            "models",
            json!({"workspace": workspace, "refresh": "stale"})
                .as_object()
                .unwrap(),
            fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .unwrap()
    });
    assert_eq!(references(&catalogue), ["acme/m1"]);
    assert_eq!(server.requests().len(), 1);
}

/// A bad shape is rejected `invalid_arguments`: a relative workspace, a
/// missing one, a `refresh` outside `cached`/`stale`/`every`, a `null`
/// or mistyped `refresh`, and an unknown key.
#[test]
fn hub_models_rejects_a_bad_shape() {
    let rig = Rig::new("fiber-hub-models-shape-reject");
    install_data(&rig.home, "acme-ext", "acme", &["m1"], &json!({}));
    let seam = Seam::new(rig.home.clone());
    let workspace = json!(rig.workspace);
    for args in [
        json!({"workspace": "relative"}),
        json!({}),
        json!({"workspace": workspace, "refresh": "fast"}),
        json!({"workspace": workspace, "refresh": null}),
        json!({"workspace": workspace, "refresh": 3}),
        json!({"workspace": workspace, "extra": true}),
    ] {
        assert_eq!(
            call(&seam, "models", args.clone()).unwrap_err().code,
            ErrorCode::InvalidArguments,
            "{args}"
        );
    }
}

/// An invalid hub configuration is rejected `config_invalid`, with the
/// file named.
#[test]
fn hub_models_reports_invalid_configuration() {
    let rig = Rig::new("fiber-hub-models-invalid");
    install_data(&rig.home, "acme-ext", "acme", &["m1"], &json!({}));
    std::fs::write(rig.home.join("config.json"), "{invalid").unwrap();
    let seam = Seam::new(rig.home.clone());
    let error = call(&seam, "models", json!({"workspace": rig.workspace})).unwrap_err();
    assert_eq!(error.code, ErrorCode::ConfigInvalid);
    assert!(
        error.message.contains("config.json"),
        "the message names the file: {}",
        error.message
    );
}

/// A configuration file that cannot be read keeps its own code: a
/// directory `config.json` is `io_failed`, not `config_invalid`.
#[test]
fn hub_models_keeps_an_io_failure_code() {
    let rig = Rig::new("fiber-hub-models-io");
    install_data(&rig.home, "acme-ext", "acme", &["m1"], &json!({}));
    std::fs::create_dir(rig.home.join("config.json")).unwrap();
    let seam = Seam::new(rig.home.clone());
    let error = call(&seam, "models", json!({"workspace": rig.workspace})).unwrap_err();
    assert_eq!(error.code, ErrorCode::IoFailed);
    assert_ne!(error.code, ErrorCode::ConfigInvalid);
}

/// A placeholder with no value rides the answer as one `left_out`
/// element with all six fields: the raw extension, the file built from
/// it, and each model's name only when the provider data names one.
#[test]
fn hub_models_encodes_a_group_with_no_value() {
    let rig = Rig::new("fiber-hub-models-left-out");
    // A declared credential path switches the `credential_label`
    // provider on; stored logins do not count for it.
    let creds = rig.home.join("acme-creds");
    std::fs::write(&creds, "").unwrap();
    let credential = json!({"credential_label": {"names": "key", "default": "the environment",
        "credential_paths": [{"path": creds.to_str().unwrap()}]}});
    install_placeholder(
        &rig.home,
        "acme-ext",
        "acme",
        "api_key",
        json!([
            {"id": "m1", "protocol": "openai-responses",
             "base_url": "https://{api_key}/v1", "context_window": 1000,
             "name": "Acme One"},
            {"id": "m2", "protocol": "openai-responses",
             "base_url": "https://{api_key}/v1", "context_window": 1000},
        ]),
        &credential,
    );
    let seam = Seam::new(rig.home.clone());
    let catalogue = result(&seam, "models", json!({"workspace": rig.workspace}));
    assert!(references(&catalogue).is_empty(), "{catalogue:?}");
    assert_eq!(
        catalogue["notices"],
        json!([]),
        "no Missing notice at launch: {catalogue:?}"
    );
    assert_eq!(
        catalogue["left_out"],
        json!([{
            "provider": "acme",
            "extension": "acme-ext",
            "file": "config/acme-ext.json",
            "setting": "api_key",
            "login": false,
            "models": [{"id": "m1", "name": "Acme One"}, {"id": "m2"}],
        }]),
    );
}

/// A first-party extension's `left_out` element carries its full name
/// while its file is the short directory name.
#[test]
fn hub_models_encodes_a_first_party_group() {
    let rig = Rig::new("fiber-hub-models-first-party");
    install_placeholder(
        &rig.home,
        "github.com/aakshintala/fiber/providers/opencode",
        "opencode",
        "workspace",
        json!([
            {"id": "m1", "protocol": "openai-responses",
             "base_url": "https://{workspace}/v1", "context_window": 1000},
        ]),
        &json!({}),
    );
    let seam = Seam::new(rig.home.clone());
    let catalogue = result(&seam, "models", json!({"workspace": rig.workspace}));
    assert!(references(&catalogue).is_empty(), "{catalogue:?}");
    assert_eq!(
        catalogue["left_out"],
        json!([{
            "provider": "opencode",
            "extension": "github.com/aakshintala/fiber/providers/opencode",
            "file": "config/opencode.json",
            "setting": "workspace",
            "login": true,
            "models": [{"id": "m1"}],
        }]),
    );
}
