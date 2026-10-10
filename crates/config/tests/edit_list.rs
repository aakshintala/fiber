//! `config::edit_list` (`docs/configuration.md`, "When Fiber writes"): one
//! locked read-modify-write applies a switch's list edits to one layer's
//! file, never narrows an inherited list, and keeps the file's extension
//! spelling.

mod common;

use std::fs;

use common::{Setup, key};
use config::{ConfigError, Layer, ListChange, ListEdit, edit_list};
use contract::ErrorCode;
use serde_json::json;

const MEMORY_FULL: &str = "github.com/aakshintala/fiber/extensions/memory";

fn edit<'a>(key: &'a str, name: &'a str, change: ListChange) -> ListEdit<'a> {
    ListEdit {
        key,
        name,
        change,
        inherited: None,
    }
}

fn list_of(root: &serde_json::Value, key: &str) -> Vec<String> {
    let mut current = root;
    for segment in key.split('.') {
        current = current.get(segment).unwrap_or(&serde_json::Value::Null);
    }
    current
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|name| name.as_str().unwrap_or_default().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn adding_a_name_appends_it() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["a"]}}"#);
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("skills.disabled", "b", ListChange::Add)],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    assert_eq!(list_of(&root, "skills.disabled"), ["a", "b"]);
}

#[test]
fn adding_a_listed_name_writes_nothing() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["a"]}}"#);
    let before = fs::read(setup.project()).unwrap();
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("skills.disabled", "a", ListChange::Add)],
    )
    .unwrap();
    assert!(!wrote);
    assert_eq!(fs::read(setup.project()).unwrap(), before);
}

#[test]
fn removing_a_name_drops_every_copy() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        r#"{"skills": {"disabled": ["a", "b", "a"]}}"#,
    );
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("skills.disabled", "a", ListChange::Remove)],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    assert_eq!(list_of(&root, "skills.disabled"), ["b"]);
}

#[test]
fn removing_an_absent_name_writes_nothing() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["a"]}}"#);
    let before = fs::read(setup.project()).unwrap();
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("skills.disabled", "x", ListChange::Remove)],
    )
    .unwrap();
    assert!(!wrote);
    assert_eq!(fs::read(setup.project()).unwrap(), before);
}

#[test]
fn a_file_without_the_list_starts_from_the_inherited_one() {
    let setup = Setup::new();
    let inherited = vec!["g".to_owned()];
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[ListEdit {
            key: "skills.disabled",
            name: "b",
            change: ListChange::Add,
            inherited: Some(&inherited),
        }],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    assert_eq!(list_of(&root, "skills.disabled"), ["g", "b"]);
}

#[test]
fn a_file_with_the_list_ignores_the_inherited_one() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["p"]}}"#);
    let inherited = vec!["g".to_owned()];
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[ListEdit {
            key: "skills.disabled",
            name: "b",
            change: ListChange::Add,
            inherited: Some(&inherited),
        }],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    assert_eq!(list_of(&root, "skills.disabled"), ["p", "b"]);
}

#[test]
fn no_change_on_a_file_without_the_list_creates_nothing() {
    let setup = Setup::new();
    let inherited = vec!["g".to_owned()];
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[ListEdit {
            key: "skills.disabled",
            name: "x",
            change: ListChange::Remove,
            inherited: Some(&inherited),
        }],
    )
    .unwrap();
    assert!(!wrote);
    assert!(!setup.project().exists());
}

#[test]
fn the_rest_of_the_file_is_kept() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        r#"{"zeta": 1, "skills": {"disabled": ["a"]}, "alpha": true}"#,
    );
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("skills.disabled", "b", ListChange::Add)],
    )
    .unwrap();
    assert!(wrote);
    assert_eq!(
        fs::read_to_string(setup.project()).unwrap(),
        "{\n  \"alpha\": true,\n  \"skills\": {\n    \"disabled\": [\n      \"a\",\n      \"b\"\n    ]\n  },\n  \"zeta\": 1\n}\n"
    );
}

#[test]
fn an_extension_key_keeps_the_spelling_the_file_uses() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        r#"{"extensions": {"memory": {"tools": {"disabled": ["a"]}}}}"#,
    );
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit(
            &format!("extensions.\"{MEMORY_FULL}\".tools.disabled"),
            "b",
            ListChange::Add,
        )],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    assert_eq!(
        root,
        json!({"extensions": {"memory": {"tools": {"disabled": ["a", "b"]}}}})
    );
    setup.load(&[]).unwrap();
}

#[test]
fn an_extension_key_with_no_entry_uses_the_keys_spelling() {
    let setup = Setup::new();
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit(
            &format!("extensions.\"{MEMORY_FULL}\".tools.disabled"),
            "b",
            ListChange::Add,
        )],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    assert_eq!(
        root,
        json!({"extensions": {"github.com/aakshintala/fiber/extensions/memory": {"tools": {"disabled": ["b"]}}}})
    );
    setup.load(&[]).unwrap();
}

#[test]
fn each_list_keeps_the_spelling_that_holds_it() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        &serde_json::to_string(&json!({
            "extensions": {
                "memory": {"tools": {"disabled": ["a"]}},
                MEMORY_FULL: {"tools": {"enabled": ["x"]}},
            }
        }))
        .unwrap(),
    );
    let disabled = format!("extensions.\"{MEMORY_FULL}\".tools.disabled");
    let enabled = "extensions.\"memory\".tools.enabled";
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[
            edit(&disabled, "b", ListChange::Add),
            ListEdit {
                key: enabled,
                name: "t",
                change: ListChange::AddIfListed,
                inherited: None,
            },
        ],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    assert_eq!(
        root,
        json!({
            "extensions": {
                "memory": {"tools": {"disabled": ["a", "b"]}},
                "github.com/aakshintala/fiber/extensions/memory": {"tools": {"enabled": ["x", "t"]}},
            }
        })
    );
    let config = setup.load(&[]).unwrap();
    assert_eq!(
        config
            .get("extensions.\"memory\".tools.disabled", None)
            .unwrap()
            .0,
        json!(["a", "b"])
    );
    assert_eq!(
        config
            .get(&format!("extensions.\"{MEMORY_FULL}\".tools.enabled"), None)
            .unwrap()
            .0,
        json!(["x", "t"])
    );
}

#[test]
fn a_no_op_edit_does_not_stop_the_next() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        &serde_json::to_string(&json!({
            "mcp": {"servers": {"gh": {"tools": {"disabled": [], "enabled": ["x"]}}}}
        }))
        .unwrap(),
    );
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[
            edit("mcp.servers.\"gh\".tools.disabled", "t", ListChange::Remove),
            ListEdit {
                key: "mcp.servers.\"gh\".tools.enabled",
                name: "t",
                change: ListChange::AddIfListed,
                inherited: None,
            },
        ],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    let tools = &root["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert_eq!(tools["enabled"], json!(["x", "t"]));
}

#[test]
fn add_if_listed_with_no_list_in_force_writes_nothing() {
    let setup = Setup::new();
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("skills.disabled", "t", ListChange::AddIfListed)],
    )
    .unwrap();
    assert!(!wrote);
    assert!(!setup.project().exists());
}

#[test]
fn add_if_listed_starts_from_the_inherited_list() {
    let setup = Setup::new();
    let inherited = vec!["x".to_owned()];
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[ListEdit {
            key: "skills.disabled",
            name: "t",
            change: ListChange::AddIfListed,
            inherited: Some(&inherited),
        }],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    assert_eq!(list_of(&root, "skills.disabled"), ["x", "t"]);
}

#[test]
fn add_if_listed_leaves_a_list_that_names_it() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["t"]}}"#);
    let before = fs::read(setup.project()).unwrap();
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("skills.disabled", "t", ListChange::AddIfListed)],
    )
    .unwrap();
    assert!(!wrote);
    assert_eq!(fs::read(setup.project()).unwrap(), before);
}

#[test]
fn a_repository_refuses_a_key_it_may_not_set() {
    let setup = Setup::new();
    let error = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Repository,
        &[edit("skills.disabled", "t", ListChange::Add)],
    )
    .unwrap_err();
    assert!(matches!(error, ConfigError::Refused { .. }), "{error:?}");
    assert_eq!(error.code(), ErrorCode::Usage);
    assert!(!setup.repository().exists());
}

#[test]
fn a_value_that_is_not_a_list_of_strings_is_refused_and_kept() {
    let setup = Setup::new();
    setup.write(&setup.project(), r#"{"skills": {"disabled": ["a", 1]}}"#);
    let before = fs::read(setup.project()).unwrap();
    let error = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("skills.disabled", "b", ListChange::Add)],
    )
    .unwrap_err();
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert_eq!(fs::read(setup.project()).unwrap(), before);
}

#[test]
fn a_key_that_is_not_dotted_is_a_usage_error() {
    let setup = Setup::new();
    let error = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[edit("a..b", "t", ListChange::Add)],
    )
    .unwrap_err();
    assert!(matches!(error, ConfigError::Override { .. }), "{error:?}");
}

#[test]
fn two_edits_land_in_one_write() {
    let setup = Setup::new();
    setup.write(
        &setup.project(),
        &serde_json::to_string(&json!({
            "mcp": {"servers": {"gh": {"tools": {"disabled": ["t"], "enabled": ["x"]}}}}
        }))
        .unwrap(),
    );
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[
            edit("mcp.servers.\"gh\".tools.disabled", "t", ListChange::Remove),
            ListEdit {
                key: "mcp.servers.\"gh\".tools.enabled",
                name: "t",
                change: ListChange::AddIfListed,
                inherited: None,
            },
        ],
    )
    .unwrap();
    assert!(wrote);
    let root: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(setup.project()).unwrap()).unwrap();
    let tools = &root["mcp"]["servers"]["gh"]["tools"];
    assert_eq!(tools["disabled"], json!([]));
    assert_eq!(tools["enabled"], json!(["x", "t"]));
}

#[test]
fn a_refused_second_edit_writes_neither() {
    let setup = Setup::new();
    let error = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Repository,
        &[
            edit("mcp.servers.\"gh\".tools.disabled", "t", ListChange::Add),
            edit("skills.disabled", "t", ListChange::Add),
        ],
    )
    .unwrap_err();
    assert!(matches!(error, ConfigError::Refused { .. }), "{error:?}");
    assert!(!setup.repository().exists());
}

#[test]
fn an_empty_slice_writes_nothing() {
    let setup = Setup::new();
    let wrote = edit_list(
        &setup.home(),
        &setup.workspace(),
        &key(),
        Layer::Project,
        &[],
    )
    .unwrap();
    assert!(!wrote);
    assert!(!setup.project().exists());
}
