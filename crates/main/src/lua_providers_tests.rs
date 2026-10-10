//! `add_lua` fills per-account host placeholders
//! (`docs/model-routing.md`, "A per-account host").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use contract::signing::SignRequest;
use extensions::Providers;
use serde_json::json;
use tools::PathLocks;

use super::{add_lua, session_credential};

/// How long the test waits for the credential lookup.
const WAIT: Duration = Duration::from_secs(5);

/// Writes the install record `extensions/<dir>/.fiber.json` holds, so the
/// directory is healthy: a directory with no record is damaged and its
/// providers are left out (`docs/extensions.md`, "Installing").
fn write_record(dir: &std::path::Path) {
    let text = std::fs::read_to_string(dir.join("extension.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_str(&text).unwrap();
    let name = manifest.get("name").and_then(|n| n.as_str()).unwrap();
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("v0.0.0");
    std::fs::write(
        dir.join(".fiber.json"),
        serde_json::json!({"name": name, "version": version, "requested": true, "source": {"path": "/p"}}).to_string(),
    )
    .unwrap();
}

fn setup_home(name: &str) -> (fakes::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let root = fakes::TempDir::new(name);
    let home = root.path().join("home");
    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    (root, home, workspace)
}

fn install_acme(home: &std::path::Path) {
    let dir = home.join("extensions/acme");
    std::fs::create_dir_all(dir.join("providers")).unwrap();
    std::fs::write(
        dir.join("extension.json"),
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    write_record(&dir);
    std::fs::write(
        dir.join("providers/acme.json"),
        json!({
            "name": "acme",
            "placeholders": {"workspace": {}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1", "context_window": 1000}],
        })
        .to_string(),
    )
    .unwrap();
}

fn load_config(home: &std::path::Path, workspace: &std::path::Path, overrides: &[&str]) -> Config {
    Config::load(Sources {
        home: home.to_path_buf(),
        workspace: workspace.to_path_buf(),
        project: ProjectKey::new("p").unwrap(),
        overrides: overrides.iter().map(|s| (*s).to_owned()).collect(),
    })
    .unwrap()
}

#[test]
fn an_unconfigured_model_is_left_out_and_a_setting_fills_it() {
    let (_root, home, workspace) = setup_home("fiber-lua-placeholders");
    install_acme(&home);
    let config = load_config(&home, &workspace, &[]);
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    let naming = add_lua(&extensions, &mut providers, &config).unwrap();
    assert!(
        naming.contains(&("acme".to_owned(), "m".to_owned())),
        "the naming list keeps the unconfigured model: {naming:?}"
    );
    assert_eq!(
        providers.resolve("acme/m").unwrap_err().code(),
        ErrorCode::ModelUnconfigured
    );

    let config = load_config(
        &home,
        &workspace,
        &["extensions.\"acme\".settings.workspace=adb-1.example"],
    );
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    add_lua(&extensions, &mut providers, &config).unwrap();
    assert_eq!(
        providers.resolve("acme/m").unwrap().model.base_url,
        "https://adb-1.example/v1"
    );
}

#[test]
fn a_settings_file_that_cannot_be_read_fails_with_config_invalid() {
    let (_root, home, workspace) = setup_home("fiber-lua-placeholders-bad");
    install_acme(&home);
    std::fs::create_dir_all(home.join("config")).unwrap();
    std::fs::write(home.join("config/acme.json"), "not json").unwrap();
    let config = load_config(&home, &workspace, &[]);
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    let err = add_lua(&extensions, &mut providers, &config).unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
}

#[test]
fn the_session_label_and_shared_credential_name_reach_credential() {
    let (_root, home, workspace) = setup_home("fiber-lua-credential-pair");
    let src = home.join("src").join("acme");
    std::fs::create_dir_all(src.join("providers")).unwrap();
    std::fs::write(
        src.join("extension.json"),
        json!({"name": "acme", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
    )
    .unwrap();
    write_record(&src);
    std::fs::write(
        src.join("init.lua"),
        "fiber.provider(\"acme\", { credential = { timeout = 60000, run = function(who)\
          return { token = who.credential .. \"/\" .. who.label, expires_at = 4102444800 } end } })\n",
    )
    .unwrap();
    std::fs::write(
        src.join("providers/acme.json"),
        json!({
            "name": "acme",
            "credential_name": "shared",
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://x.example/v1"}],
        })
        .to_string(),
    )
    .unwrap();
    extensions::plan(
        &home,
        &extensions::Request::Path(src),
        "0.1.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    let config = load_config(&home, &workspace, &[]);
    let (mut providers, _) = Providers::load(&home).unwrap();
    let extensions = extensions::SessionExtensions::load(
        &home,
        &config,
        fakes::clock::FakeClock::new(),
        Arc::new(PathLocks::new()),
        None,
    );
    add_lua(&extensions, &mut providers, &config).unwrap();
    let data = providers.data("acme");
    // The lookup blocks on Lua, so it runs on its own thread under a
    // deadline instead of hanging the test.
    let (done, looked_up) = mpsc::channel();
    std::thread::spawn(move || {
        drop(done.send(session_credential(
            providers.lua("acme"),
            &data,
            "work",
            || panic!("no key is read when credential() is registered"),
        )))
    });
    let (key, signer) = looked_up
        .recv_timeout(WAIT)
        .expect("the lookup ended in time")
        .unwrap();
    assert!(key.is_none(), "no key file is needed past credential()");
    let signer = signer.unwrap();
    let headers = signer
        .sign(&SignRequest {
            method: "POST",
            url: "https://x.example/v1",
            headers: &[],
            body: b"{}",
        })
        .unwrap();
    assert_eq!(
        headers,
        [("authorization".to_owned(), "Bearer shared/work".to_owned())]
    );
}
