//! `docs/extensions.md`, "Code a repository ships", "Declaring":
//! `repository_extensions` is read from the repository's file alone, and
//! `config::declared` returns what the repository's own files declare.

mod common;

use std::os::unix::fs::symlink;

use common::Setup;
use config::{Declared, RepositoryExtension, Source};
use contract::ErrorCode;
use serde_json::json;

const LISTED: &str =
    r#"{"repository_extensions": [{"path": "tools/a", "required": true}, {"path": "tools/b"}]}"#;

const HOOKS_FILE: &str = "config/hooks.json";

#[test]
fn a_repository_lists_extensions_with_no_notice() {
    let setup = Setup::new();
    setup.write(&setup.repository(), LISTED);
    let config = setup.load(&[]).unwrap();
    assert!(config.notices().is_empty(), "{:?}", config.notices());
    assert_eq!(
        config.get("repository_extensions", None),
        Some((
            json!([{"path": "tools/a", "required": true}, {"path": "tools/b"}]),
            Source::Repository(setup.repository())
        ))
    );
}

#[test]
fn another_layer_cannot_list_extensions() {
    for layer in ["global", "project"] {
        let setup = Setup::new();
        let file = if layer == "global" {
            setup.global()
        } else {
            setup.project()
        };
        setup.write(&file, LISTED);
        let config = setup.load(&[]).unwrap();
        let [notice] = config.notices() else {
            panic!("{layer}: {:?}", config.notices());
        };
        assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
        assert_eq!(
            notice.message,
            format!(
                "{}: ignored `repository_extensions`, which only a repository's own file may set.",
                file.display()
            )
        );
        assert_eq!(config.get("repository_extensions", None), None, "{layer}");
    }
}

#[test]
fn a_run_flag_cannot_list_extensions() {
    let setup = Setup::new();
    let config = setup
        .load(&[r#"repository_extensions=[{"path":"x"}]"#])
        .unwrap();
    let [notice] = config.notices() else {
        panic!("{:?}", config.notices());
    };
    assert_eq!(notice.code, ErrorCode::ConfigKeyIgnored);
    assert_eq!(config.get("repository_extensions", None), None);
}

#[test]
fn a_malformed_entry_is_config_invalid() {
    for bad in [
        r#"{"repository_extensions": [{"required": true}]}"#,
        r#"{"repository_extensions": [{"path": 5}]}"#,
        r#"{"repository_extensions": [{"path": "a", "required": "yes"}]}"#,
        r#"{"repository_extensions": ["a"]}"#,
        r#"{"repository_extensions": {"path": "a"}}"#,
    ] {
        let setup = Setup::new();
        setup.write(&setup.repository(), bad);
        let e = setup.load(&[]).unwrap_err();
        assert_eq!(e.code(), ErrorCode::ConfigInvalid, "{bad}");
        assert!(
            e.to_string().starts_with(&format!(
                "{}: `repository_extensions` must be",
                setup.repository().display()
            )),
            "{e}"
        );
        assert_eq!(
            config::declared(&setup.workspace()).unwrap_err().code(),
            ErrorCode::ConfigInvalid,
            "{bad}"
        );
    }
}

#[test]
fn a_repository_with_nothing_declared_gets_an_empty_set() {
    let setup = Setup::new();
    assert_eq!(
        config::declared(&setup.workspace()).unwrap(),
        Declared::default()
    );
    setup.write(&setup.repository(), r#"{"model": "a/b"}"#);
    assert_eq!(
        config::declared(&setup.workspace()).unwrap(),
        Declared::default()
    );
}

#[test]
fn declared_returns_extensions_hooks_and_servers() {
    let setup = Setup::new();
    setup.write(
        &setup.repository(),
        r#"{"repository_extensions": [{"path": "tools/a", "required": true}, {"path": "tools/b"}],
            "mcp": {"servers": {"db": {"command": "scripts/db.sh", "args": ["--ro"], "required": true,
                                       "tools": {"x": {"hints": {"read_only": true}}}}}}}"#,
    );
    setup.write(
        &setup.workspace().join(".fiber").join(HOOKS_FILE),
        r#"{"hooks": {"fmt": {"point": "after_tool", "command": "cargo", "args": ["fmt"]}}}"#,
    );
    let declared = config::declared(&setup.workspace()).unwrap();
    assert_eq!(
        declared.extensions,
        vec![
            RepositoryExtension {
                path: "tools/a".into(),
                required: true
            },
            RepositoryExtension {
                path: "tools/b".into(),
                required: false
            },
        ]
    );
    assert_eq!(
        serde_json::Value::Object(declared.hooks),
        json!({"fmt": {"point": "after_tool", "command": "cargo", "args": ["fmt"]}})
    );
    // A repository may not set a server's hints, so they are not declared.
    assert_eq!(
        serde_json::Value::Object(declared.mcp_servers),
        json!({"db": {"command": "scripts/db.sh", "args": ["--ro"], "required": true, "tools": {"x": {}}}})
    );
}

#[test]
fn declared_reads_only_the_repository_layer() {
    let setup = Setup::new();
    setup.write(
        &setup.global(),
        r#"{"mcp": {"servers": {"g": {"command": "x"}}}}"#,
    );
    setup.write(
        &setup.home().join(HOOKS_FILE),
        r#"{"hooks": {"mine": {"command": "x"}}}"#,
    );
    assert_eq!(
        config::declared(&setup.workspace()).unwrap(),
        Declared::default()
    );
}

#[test]
fn a_hooks_setting_that_is_not_an_object_is_config_invalid() {
    let setup = Setup::new();
    setup.write(
        &setup.workspace().join(".fiber").join(HOOKS_FILE),
        r#"{"hooks": ["fmt"]}"#,
    );
    let e = config::declared(&setup.workspace()).unwrap_err();
    assert_eq!(e.code(), ErrorCode::ConfigInvalid);
}

#[test]
fn a_symbolic_link_repository_file_is_refused() {
    let setup = Setup::new();
    let outside = setup.root().join("elsewhere.json");
    setup.write(&outside, LISTED);
    std::fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    symlink(&outside, setup.repository()).unwrap();
    assert_eq!(
        config::declared(&setup.workspace()).unwrap_err().code(),
        ErrorCode::ConfigInvalid
    );
}
