//! An `extensions."<name>"` key spelled with a short name or a full name
//! (`docs/configuration.md`, "Keys").

mod common;

use common::Setup;
use config::{ConfigError, Source};
use contract::ErrorCode;
use serde_json::json;

const MEMORY: &str = "github.com/aakshintala/fiber/extensions/memory";

#[test]
fn a_short_name_and_a_full_name_are_the_same_key() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"extensions": {"memory": {"enabled": false}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    let expected = Some((json!(false), Source::Global(setup.global())));
    assert_eq!(config.get("extensions.memory.enabled", None), expected);
    assert_eq!(
        config.get(&format!("extensions.\"{MEMORY}\".enabled"), None),
        expected
    );
    // The merged configuration holds the full name only.
    let merged = config.merged(None);
    assert_eq!(merged["extensions"][MEMORY]["enabled"], json!(false));
    assert!(merged["extensions"].get("memory").is_none());
}

#[test]
fn an_unset_key_has_its_default_under_either_spelling() {
    let config = Setup::new().load(&[]).unwrap();
    let expected = Some((json!(true), Source::Default));
    assert_eq!(config.get("extensions.memory.enabled", None), expected);
    assert_eq!(
        config.get(&format!("extensions.\"{MEMORY}\".enabled"), None),
        expected
    );
}

#[test]
fn one_key_set_under_both_spellings_in_one_file_is_invalid() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &json!({"extensions": {
            "memory": {"enabled": false},
            MEMORY: {"enabled": true},
        }})
        .to_string(),
    );
    let e = setup.load(&[]).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    let ConfigError::DuplicateExtension { source_name, key } = &e else {
        panic!("{e:?}");
    };
    assert_eq!(source_name, &setup.global().display().to_string());
    assert_eq!(key, "extensions.memory.enabled");
}

#[test]
fn a_nested_key_set_under_both_spellings_names_its_whole_path() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        &json!({"extensions": {
            "memory": {"commands": {"a": "x", "b": "y"}},
            MEMORY: {"commands": {"b": "z"}},
        }})
        .to_string(),
    );
    let e = setup.load(&[]).unwrap_err();
    assert!(
        matches!(&e, ConfigError::DuplicateExtension { key, .. } if key == "extensions.memory.commands.b"),
        "{e:?}"
    );
}

#[test]
fn different_keys_under_the_two_spellings_merge() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        &json!({"extensions": {
            "memory": {"enabled": false, "commands": {"a": "x"}},
            MEMORY: {"startup_timeout_ms": 9000, "commands": {"b": "y"}},
        }})
        .to_string(),
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config.get("extensions.memory.enabled", None).unwrap().0,
        json!(false)
    );
    assert_eq!(
        config
            .get("extensions.memory.startup_timeout_ms", None)
            .unwrap()
            .0,
        json!(9000)
    );
    assert_eq!(
        config.get("extensions.memory.commands", None).unwrap().0,
        json!({"a": "x", "b": "y"})
    );
}

#[test]
fn the_two_spellings_in_different_layers_merge_as_layers_do() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"extensions": {"memory": {"enabled": false}}}"#,
    );
    setup.write(
        &setup.project(),
        &json!({"extensions": {MEMORY: {"enabled": true}}}).to_string(),
    );
    let config = setup
        .load(&["extensions.memory.startup_timeout_ms=7000"])
        .unwrap();
    assert_eq!(
        config.get("extensions.memory.enabled", None),
        Some((json!(true), Source::Project(setup.project())))
    );
    assert_eq!(
        config.get(&format!("extensions.\"{MEMORY}\".startup_timeout_ms"), None),
        Some((json!(7000), Source::Run))
    );
}

#[test]
fn a_run_flag_setting_under_a_short_name_reaches_the_full_name() {
    let setup = Setup::new();
    let config = setup.load(&["extensions.memory.settings.k=v"]).unwrap();
    assert_eq!(
        config.extensions().get(MEMORY, &[], "k").unwrap(),
        Some(json!("v"))
    );
    assert_eq!(
        config.extensions().get("memory", &[], "k").unwrap(),
        Some(json!("v"))
    );
}

#[test]
fn one_run_flag_setting_under_both_spellings_is_invalid() {
    let setup = Setup::new();
    let full = format!("extensions.\"{MEMORY}\".settings.k=w");
    let e = setup
        .load(&["extensions.memory.settings.k=v", &full])
        .unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
    assert!(
        matches!(&e, ConfigError::DuplicateExtension { source_name, key }
            if source_name == "-c" && key == "extensions.memory.settings.k"),
        "{e:?}"
    );
}

#[test]
fn a_third_party_name_is_left_alone() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"extensions": {"github.com/acme/lint": {"enabled": false}, "lint": {"enabled": true}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config
            .get("extensions.\"github.com/acme/lint\".enabled", None)
            .unwrap()
            .0,
        json!(false)
    );
    assert_eq!(
        config.get("extensions.lint.enabled", None).unwrap().0,
        json!(true)
    );
}
