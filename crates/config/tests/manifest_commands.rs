//! `docs/configuration.md`, "An extension's manifest": `commands` maps each
//! command an extension registers to its description and optional argument
//! hint. A present key makes the extension lazy, so absent and empty differ.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use config::{ConfigError, Manifest, ManifestCommand, read_manifest};
use contract::ErrorCode;
use serde_json::{Value, json};

/// Reads a manifest whose `commands` key is `commands`; `None` leaves the
/// key out.
fn read(commands: Option<Value>) -> Result<Manifest, ConfigError> {
    let dir = fakes::TempDir::new("fiber-manifest-commands");
    let mut manifest = serde_json::Map::new();
    for (key, value) in [
        ("name", json!("fiber.test/lazy")),
        ("version", json!("v1")),
        ("fiber", json!("0.1.0")),
        ("api", json!(1)),
    ] {
        manifest.insert(key.into(), value);
    }
    if let Some(commands) = commands {
        manifest.insert("commands".into(), commands);
    }
    dir.write("extension.json", &Value::Object(manifest).to_string());
    read_manifest(dir.path())
}

#[test]
fn tui_session_versions_require_strings_and_distinguish_null_from_omission() {
    for (section, valid) in [
        (json!({}), true),
        (json!({"session_versions":{"min":"v1.0.0"}}), true),
        (
            json!({"session_versions":{"min":"v1.0.0","before":"v2.0.0"}}),
            true,
        ),
        (json!({"session_versions":null}), false),
        (json!({"session_versions":{}}), false),
        (json!({"session_versions":{"min":1}}), false),
        (
            json!({"session_versions":{"min":"v1.0.0","before":null}}),
            false,
        ),
        (
            json!({"session_versions":{"min":"v1.0.0","before":2}}),
            false,
        ),
    ] {
        let dir = fakes::TempDir::new("fiber-tui-manifest");
        dir.write(
            "extension.json",
            &json!({"name":"test","version":"v1.0.0","fiber":"0.1.0","api":1,"tui":section})
                .to_string(),
        );
        let result = read_manifest(dir.path());
        assert_eq!(result.is_ok(), valid, "{section}: {result:?}");
        if let Ok(manifest) = result {
            let tui: config::TuiManifest = manifest.tui.unwrap();
            let _: Option<config::SessionVersions> = tui.session_versions;
        }
    }
}

fn refused_name(name: &str) -> bool {
    let result = read(Some(json!({ name: { "description": "d" } })));
    matches!(result, Err(ConfigError::WrongType { key, .. }) if key == "commands")
}

#[test]
fn the_documented_example_parses() {
    let manifest = read(Some(json!({
        "acme-status": { "description": "Show the Acme sync state", "argument_hint": "[project]" }
    })))
    .unwrap();
    let commands = manifest.commands.unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(
        commands["acme-status"],
        ManifestCommand {
            description: "Show the Acme sync state".into(),
            argument_hint: Some("[project]".into()),
        }
    );
}

#[test]
fn an_absent_hint_is_none() {
    let manifest = read(Some(json!({ "x": { "description": "d" } }))).unwrap();
    assert_eq!(manifest.commands.unwrap()["x"].argument_hint, None);
}

#[test]
fn an_absent_key_is_none_and_an_empty_object_is_some() {
    assert_eq!(read(None).unwrap().commands, None);
    assert_eq!(
        read(Some(json!({}))).unwrap().commands,
        Some(std::collections::BTreeMap::new())
    );
}

#[test]
fn a_command_without_a_description_is_config_invalid() {
    let error = read(Some(json!({ "x": { "argument_hint": "[a]" } }))).unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
}

#[test]
fn unknown_inner_keys_are_ignored() {
    let manifest = read(Some(json!({ "x": { "description": "d", "other": 1 } }))).unwrap();
    assert_eq!(manifest.commands.unwrap().len(), 1);
}

#[test]
fn a_name_fiber_command_would_refuse_is_config_invalid() {
    for name in ["", "a b", "a\tb", "a/b", "a:b"] {
        assert!(refused_name(name), "{name:?} was not refused");
    }
}

#[test]
fn a_name_fiber_command_accepts_loads() {
    for name in ["x", ".", "export", "acme-status", "x.y"] {
        assert!(!refused_name(name), "{name:?} was refused");
        assert!(
            read(Some(json!({ name: { "description": "d" } }))).is_ok(),
            "{name:?} did not load"
        );
    }
}

#[test]
fn a_bad_name_among_good_ones_is_refused() {
    let result = read(Some(json!({
        "good": { "description": "d" }, "bad name": { "description": "d" }
    })));
    assert!(matches!(result, Err(ConfigError::WrongType { key, .. }) if key == "commands"));
}

#[test]
fn null_and_non_objects_are_config_invalid_not_absent() {
    for value in [json!(null), json!([]), json!("export"), json!(1)] {
        let error = read(Some(value.clone())).unwrap_err();
        assert_eq!(error.code(), ErrorCode::ConfigInvalid, "{value}");
    }
}
