//! Writing case configuration and extension settings (`docs/testing.md`, "Testing an extension").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "the config assertions fail the test with their path context"
)]

use std::fs;

use serde_json::{Value, json};

use super::{write_config, write_settings};

#[test]
fn the_case_config_is_written_whole_into_a_missing_home() {
    let root = fakes::TempDir::new("fiber-case-config");
    let home = root.path().join("home");
    assert!(!home.exists(), "the home starts missing");
    let config = json!({"handoff": {"tokens": 5}, "model": "a/b"});
    let file = home.join("config.json");
    write_config(&file, &config).unwrap();
    let text = fs::read_to_string(&file).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), config);
    let leftovers: Vec<std::path::PathBuf> = fs::read_dir(&home)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn an_unwritable_home_names_the_home() {
    let root = fakes::TempDir::new("fiber-case-config-unwritable");
    let home = root.path().join("home");
    fs::write(&home, "in the way").unwrap();
    let file = home.join("config.json");
    let err = write_config(&file, &json!({"model": "a/b"})).unwrap_err();
    assert!(
        err.contains(&home.display().to_string()),
        "the error names the home: {err}"
    );
}

#[test]
fn each_settings_entry_is_written_as_that_extensions_global_file() {
    let root = fakes::TempDir::new("fiber-case-settings");
    let home = root.path().join("home");
    write_settings(
        &home,
        &json!({"hooks": {"hooks": {"probe": {"point": "session_start"}}}, "other": {}}),
    )
    .unwrap();
    let text = fs::read_to_string(home.join("config").join("hooks.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"hooks": {"probe": {"point": "session_start"}}}),
    );
    let text = fs::read_to_string(home.join("config").join("other.json")).unwrap();
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({}));
}
