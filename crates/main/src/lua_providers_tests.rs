//! `add_lua` fills per-account host placeholders
//! (`docs/model-routing.md`, "A per-account host").

#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::sync::Arc;

use config::{Config, ProjectKey, Sources};
use contract::ErrorCode;
use extensions::Providers;
use serde_json::json;
use tools::PathLocks;

use super::add_lua;

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
    std::fs::write(
        dir.join("providers/acme.json"),
        json!({
            "name": "acme",
            "placeholders": {"workspace": {}},
            "models": [{"id": "m", "protocol": "openai-responses",
                        "base_url": "https://{workspace}/v1"}],
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
    );
    add_lua(&extensions, &mut providers, &config).unwrap();
    assert!(matches!(
        providers.resolve("acme/m").unwrap_err(),
        extensions::Error::UnknownModel { .. }
    ));

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
    );
    let err = add_lua(&extensions, &mut providers, &config).unwrap_err();
    assert_eq!(err.code, ErrorCode::ConfigInvalid);
}
