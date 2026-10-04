//! `edit` through [`tools::Files`] (`docs/tools.md`, "File tools").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::fs;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use contract::tool::Tool;
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Map, Value, json};
use tools::Files;

const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "enum",
    "description",
];

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn text(output: &contract::tool::Output) -> String {
    match output.content.first() {
        Some(ContentPart::Text { text }) => text.clone(),
        Some(ContentPart::Image { .. } | ContentPart::Unknown) | None => String::new(),
    }
}

fn strict(schema: &Value) {
    let Some(map) = schema.as_object() else {
        panic!("schema node is an object");
    };
    for key in map.keys() {
        assert!(KEYWORDS.contains(&key.as_str()), "{key}");
    }
    match map.get("type").and_then(Value::as_str) {
        Some("object") => {
            let properties = map.get("properties").unwrap().as_object().unwrap();
            let required: BTreeSet<_> = map
                .get("required")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap())
                .collect();
            let names: BTreeSet<_> = properties.keys().map(String::as_str).collect();
            assert_eq!(required, names);
            assert_eq!(map.get("additionalProperties"), Some(&Value::Bool(false)));
            for property in properties.values() {
                strict(property);
            }
        }
        Some("array") => strict(map.get("items").unwrap()),
        Some("string" | "number" | "integer" | "boolean" | "null") => {}
        _ => panic!("schema type"),
    }
}

#[test]
fn edit_applies_every_block_or_none_and_a_later_write_is_allowed() {
    let dir = TempDir::new("fiber-edit-api");
    fs::write(dir.path().join("a.txt"), "aaa\nbbb\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.edit().run(
        &args(json!({
            "path": "a.txt",
            "edits": [
                {"old_text": "bbb", "new_text": "BBB"},
                {"old_text": "aaa", "new_text": "AAA"}
            ]
        })),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", text(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"AAA\nBBB\n");
    assert!(text(&output).contains("edits[0]: replaced lines 2-2 with lines 2-2."));
    assert!(text(&output).contains("edits[1]: replaced lines 1-1 with lines 1-1."));
    assert!(text(&output).contains("Wrote "));
    let diff = output.details.as_ref().unwrap()["diff"].as_str().unwrap();
    assert!(diff.contains("--- "), "{diff}");
    assert!(!text(&output).contains(diff));
    assert_eq!(
        output
            .changes
            .as_ref()
            .map(|changes| (changes[0].added, changes[0].removed)),
        Some((2, 2))
    );

    let missed = files.edit().run(
        &args(json!({
            "path": "a.txt",
            "edits": [
                {"old_text": "AAA", "new_text": "nope"},
                {"old_text": "missing", "new_text": "x"}
            ]
        })),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        missed.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::NoMatch)
    );
    assert_eq!(
        missed.error.as_ref().map(|error| error.message.as_str()),
        Some("edits[1]: old_text was not found. Read the file again and copy the text exactly.")
    );
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"AAA\nBBB\n");

    let replaced = files.write().run(
        &args(json!({"path": "a.txt", "content": "done\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(replaced.error.is_none(), "{}", text(&replaced));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"done\n");
}

#[test]
fn a_missing_file_and_a_directory_fail_and_effects_are_irreversible() {
    let dir = TempDir::new("fiber-edit-api-miss");
    let files = Files::new(dir.path().to_path_buf());
    let missing = files.edit().run(
        &args(json!({
            "path": "nope.txt",
            "edits": [{"old_text": "a", "new_text": "b"}]
        })),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        missing.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::NotFound)
    );
    assert!(!dir.path().join("nope.txt").exists());
    fs::create_dir(dir.path().join("sub")).unwrap();
    let directory = files.edit().run(
        &args(json!({
            "path": "sub",
            "edits": [{"old_text": "a", "new_text": "b"}]
        })),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        directory.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::UnsupportedFile)
    );
    let effects = files
        .edit()
        .effects(&args(json!({
            "path": "nope.txt",
            "edits": [{"old_text": "a", "new_text": "b"}]
        })))
        .unwrap();
    assert_eq!(effects.declared.effects, vec![Effect::Writes]);
    assert!(!effects.declared.reversible);
    strict(&files.edit().definition().input_schema);
}
