//! `Config::extension_setting`, `workspace`, `project` and
//! `Manifest::repo_settings` (`docs/configuration.md`, "Extension settings").

mod common;

use common::{PROJECT, Setup};
use serde_json::json;

const ACME: &str = "github.com/acme/fiber-acme";
const FILE: &str = "config/github.com-acme-fiber-acme.json";

#[test]
fn the_highest_layer_wins_and_a_nested_key_reads_through() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join(FILE),
        r#"{"picker": {"model": "global/m", "other": 1}}"#,
    );
    setup.write(
        &setup.home().join("projects").join(PROJECT).join(FILE),
        r#"{"picker": {"model": "project/m"}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.extension_setting(ACME, &[], "picker.model").unwrap(),
        Some(json!("project/m"))
    );
    assert_eq!(
        config.extension_setting(ACME, &[], "picker.other").unwrap(),
        Some(json!(1))
    );
}

#[test]
fn an_unset_key_is_none_and_a_bad_key_is_a_usage_error() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.extension_setting(ACME, &[], "picker.model").unwrap(),
        None
    );
    assert_eq!(
        config
            .extension_setting("other", &[], "anything.at.all")
            .unwrap(),
        None
    );
    let e = config
        .extension_setting(ACME, &[], "unclosed.\"quote")
        .unwrap_err();
    assert_eq!(e.code(), contract::ErrorCode::Usage);
}

#[test]
fn a_run_flag_wins_over_the_files() {
    let setup = Setup::new();
    setup.write(&setup.home().join(FILE), r#"{"a": "global"}"#);
    let config = setup
        .load(&["extensions.\"github.com/acme/fiber-acme\".settings.a=\"run\""])
        .unwrap();
    assert_eq!(
        config.extension_setting(ACME, &[], "a").unwrap(),
        Some(json!("run"))
    );
}

#[test]
fn a_repository_key_outside_repo_settings_is_ignored() {
    let setup = Setup::new();
    setup.write(
        &setup.workspace().join(".fiber").join(FILE),
        r#"{"workspace_url": "https://repo", "token_command": "curl evil"}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config
            .extension_setting(ACME, &["workspace_url"], "workspace_url")
            .unwrap(),
        Some(json!("https://repo"))
    );
    assert_eq!(
        config
            .extension_setting(ACME, &["workspace_url"], "token_command")
            .unwrap(),
        None
    );
}

#[test]
fn workspace_and_project_name_the_session_s_layers() {
    let setup = Setup::new();
    let config = setup.load(&[]).unwrap();
    assert_eq!(config.workspace().to_path_buf(), setup.workspace());
    assert_eq!(config.project(), &common::key());
}

#[test]
fn the_manifest_reads_repo_settings_and_defaults_them_to_empty() {
    let setup = Setup::new();
    let dir = setup.root().join("ext");
    std::fs::create_dir_all(&dir).unwrap();
    setup.write(
        &dir.join("extension.json"),
        r#"{"name": "fiber.test/x", "version": "v1.0.0", "fiber": "0.1.0", "api": 1,
            "repo_settings": ["workspace_url", "hooks"]}"#,
    );
    let manifest = config::read_manifest(&dir).unwrap();
    assert_eq!(manifest.repo_settings, ["workspace_url", "hooks"]);
    setup.write(
        &dir.join("extension.json"),
        r#"{"name": "fiber.test/x", "version": "v1.0.0", "fiber": "0.1.0", "api": 1}"#,
    );
    let manifest = config::read_manifest(&dir).unwrap();
    assert!(manifest.repo_settings.is_empty());
}
