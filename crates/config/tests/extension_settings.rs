//! `docs/configuration.md`, "Extension settings": each layer's own file,
//! merged as Fiber's keys are, with a repository limited to `repo_settings`.

mod common;

use common::{PROJECT, Setup};
use contract::ErrorCode;
use serde_json::json;

const ACME: &str = "github.com/acme/fiber-acme";
const FILE: &str = "config/github.com-acme-fiber-acme.json";

#[test]
fn settings_merge_across_the_layers_in_order() {
    let setup = Setup::new();
    setup.write(
        &setup.home().join(FILE),
        r#"{"a": 1, "b": {"x": 1}, "c": "global"}"#,
    );
    setup.write(
        &setup.workspace().join(".fiber").join(FILE),
        r#"{"workspace_url": "https://repo", "c": "repo"}"#,
    );
    setup.write(
        &setup.home().join("projects").join(PROJECT).join(FILE),
        r#"{"b": {"y": 2}, "workspace_url": "https://mine"}"#,
    );
    let config = setup
        .load(&[
            "extensions.\"github.com/acme/fiber-acme\".settings.a=5",
            "model=a/b",
        ])
        .unwrap();
    let (settings, notices) = config
        .extension_settings(ACME, &["workspace_url", "c"])
        .unwrap();
    assert_eq!(
        settings,
        json!({"a": 5, "b": {"x": 1, "y": 2}, "c": "repo", "workspace_url": "https://mine"})
    );
    assert!(notices.is_empty());
    assert!(config.notices().is_empty());
    assert_eq!(config.merged(None).get("extensions"), None);
}

#[test]
fn a_repository_key_not_in_repo_settings_is_a_notice_and_ignored() {
    let setup = Setup::new();
    let repo_file = setup.workspace().join(".fiber").join(FILE);
    setup.write(
        &repo_file,
        r#"{"workspace_url": "https://repo", "token_command": "curl evil"}"#,
    );
    let (settings, notices) = setup
        .load(&[])
        .unwrap()
        .extension_settings(ACME, &["workspace_url"])
        .unwrap();
    assert_eq!(settings, json!({"workspace_url": "https://repo"}));
    let [notice] = notices.as_slice() else {
        panic!("{notices:?}");
    };
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
    assert_eq!(notice.extension.as_deref(), Some(ACME));
    assert_eq!(
        notice.message,
        format!(
            "{}: ignored `token_command`, which the extension does not list under repo_settings.",
            repo_file.display()
        )
    );
}

#[test]
fn the_person_s_own_files_set_any_key() {
    let setup = Setup::new();
    setup.write(&setup.home().join(FILE), r#"{"token_command": "op read"}"#);
    let (settings, notices) = setup
        .load(&[])
        .unwrap()
        .extension_settings(ACME, &[])
        .unwrap();
    assert_eq!(settings, json!({"token_command": "op read"}));
    assert!(notices.is_empty());
}

#[test]
fn settings_in_config_json_are_unknown_keys() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"extensions": {"acme": {"settings": {"a": 1}}}}"#,
    );
    let config = setup.load(&[]).unwrap();
    assert!(
        config.notices()[0]
            .message
            .contains("`extensions.acme.settings`")
    );
    assert_eq!(config.extension_settings("acme", &[]).unwrap().0, json!({}));
}

#[test]
fn a_settings_file_that_is_not_an_object_or_not_json_is_config_invalid() {
    for text in ["[1]", "{", "\"a\""] {
        let setup = Setup::new();
        let file = setup.home().join("projects").join(PROJECT).join(FILE);
        setup.write(&file, text);
        let e = setup
            .load(&[])
            .unwrap()
            .extension_settings(ACME, &[])
            .unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{text}");
        assert!(
            e.to_string().starts_with(&file.display().to_string()),
            "{e}"
        );
    }
}

#[test]
fn another_extension_s_run_settings_are_not_this_one_s() {
    let setup = Setup::new();
    let config = setup.load(&["extensions.other.settings.a=1"]).unwrap();
    assert_eq!(config.extension_settings(ACME, &[]).unwrap().0, json!({}));
    assert_eq!(
        config.extension_settings("other", &[]).unwrap().0,
        json!({"a": 1})
    );
}
