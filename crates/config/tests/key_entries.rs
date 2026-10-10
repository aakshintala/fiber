//! `docs/configuration.md`, "When Fiber writes": the `/keys` screen saves
//! the global `keys` in one locked write, keeping every other key.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]

mod common;

use std::fs;

use common::Setup;
use config::update_global_entries;
use contract::ErrorCode;
use serde_json::json;

#[test]
fn one_entry_with_no_file_writes_only_it() {
    let setup = Setup::new();
    update_global_entries(
        &setup.home(),
        "keys",
        &[("new_session".to_owned(), Some(json!(["ctrl+t"])))],
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        "{\n  \"keys\": {\n    \"new_session\": [\n      \"ctrl+t\"\n    ]\n  }\n}\n"
    );
}

#[test]
fn setting_one_entry_keeps_the_files_other_keys_and_entries() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"model": "acme/m1", "keys": {"copy_focused": "y"}}"#,
    );
    update_global_entries(
        &setup.home(),
        "keys",
        &[("new_session".to_owned(), Some(json!(["ctrl+t"])))],
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        concat!(
            "{\n",
            "  \"keys\": {\n",
            "    \"copy_focused\": \"y\",\n",
            "    \"new_session\": [\n",
            "      \"ctrl+t\"\n",
            "    ]\n",
            "  },\n",
            "  \"model\": \"acme/m1\"\n",
            "}\n"
        )
    );
}

#[test]
fn an_empty_list_unbinds() {
    let setup = Setup::new();
    update_global_entries(
        &setup.home(),
        "keys",
        &[("copy_focused".to_owned(), Some(json!([])))],
    )
    .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        "{\n  \"keys\": {\n    \"copy_focused\": []\n  }\n}\n"
    );
}

#[test]
fn removing_an_entry_keeps_the_others() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"keys": {"new_session": ["ctrl+t"], "copy_focused": "y"}}"#,
    );
    update_global_entries(&setup.home(), "keys", &[("new_session".to_owned(), None)])
        .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        "{\n  \"keys\": {\n    \"copy_focused\": \"y\"\n  }\n}\n"
    );
}

#[test]
fn removing_the_last_entry_removes_keys() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"model": "acme/m1", "keys": {"new_session": ["ctrl+t"]}}"#,
    );
    update_global_entries(&setup.home(), "keys", &[("new_session".to_owned(), None)])
        .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(
        fs::read_to_string(setup.global()).unwrap(),
        "{\n  \"model\": \"acme/m1\"\n}\n"
    );
}

#[test]
fn removing_an_entry_the_file_lacks_writes_nothing() {
    let setup = Setup::new();
    let text = "{\"keys\": {\"copy_focused\": \"y\"}, \"model\": \"acme/m1\"}";
    setup.write(&setup.global(), text);
    update_global_entries(&setup.home(), "keys", &[("new_session".to_owned(), None)])
        .unwrap_or_else(|e| panic!("write: {e}"));
    assert_eq!(fs::read_to_string(setup.global()).unwrap(), text);
}

#[test]
fn a_wrongly_typed_entry_is_config_invalid_and_writes_nothing() {
    let setup = Setup::new();
    let text = r#"{"keys": {"copy_focused": "y"}}"#;
    setup.write(&setup.global(), text);
    let error = update_global_entries(
        &setup.home(),
        "keys",
        &[("new_session".to_owned(), Some(json!(5)))],
    )
    .expect_err("a number is no key list");
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("keys"), "{error}");
    assert_eq!(fs::read_to_string(setup.global()).unwrap(), text);
}

#[test]
fn a_keys_that_is_no_object_is_refused_and_writes_nothing() {
    let setup = Setup::new();
    let text = r#"{"keys": "x"}"#;
    setup.write(&setup.global(), text);
    let error = update_global_entries(
        &setup.home(),
        "keys",
        &[("new_session".to_owned(), Some(json!(["ctrl+t"])))],
    )
    .expect_err("a string is no object");
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert!(error.to_string().contains("keys"), "{error}");
    assert_eq!(fs::read_to_string(setup.global()).unwrap(), text);
}
