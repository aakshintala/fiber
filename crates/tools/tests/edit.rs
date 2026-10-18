//! `edit` through [`tools::Files`] (`docs/tools.md`, "File tools").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;

use contract::ErrorCode;
use contract::shapes::Effect;
use contract::tool::Tool;
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::json;
use tools::Files;

#[test]
fn edit_applies_every_block_or_none_and_a_later_write_is_allowed() {
    let dir = TempDir::new("fiber-edit-api");
    fs::write(dir.path().join("a.txt"), "aaa\nbbb\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.edit().run(
        &fakes::tool_args(json!({
            "path": "a.txt",
            "edits": [
                {"old_text": "bbb", "new_text": "BBB"},
                {"old_text": "aaa", "new_text": "AAA"}
            ]
        })),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none(), "{}", fakes::text_of(&output));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"AAA\nBBB\n");
    assert!(fakes::text_of(&output).contains("edits[0]: replaced lines 2-2 with lines 2-2."));
    assert!(fakes::text_of(&output).contains("edits[1]: replaced lines 1-1 with lines 1-1."));
    assert!(fakes::text_of(&output).contains("Wrote "));
    let diff = output.details.as_ref().unwrap()["diff"].as_str().unwrap();
    assert!(diff.contains("--- "), "{diff}");
    assert!(!fakes::text_of(&output).contains(diff));
    assert_eq!(
        output
            .changes
            .as_ref()
            .map(|changes| (changes[0].added, changes[0].removed)),
        Some((2, 2))
    );

    let missed = files.edit().run(
        &fakes::tool_args(json!({
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
        &fakes::tool_args(json!({"path": "a.txt", "content": "done\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(replaced.error.is_none(), "{}", fakes::text_of(&replaced));
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"done\n");
}

#[test]
fn a_missing_file_and_a_directory_fail_and_effects_are_irreversible() {
    let dir = TempDir::new("fiber-edit-api-miss");
    let files = Files::new(dir.path().to_path_buf());
    let missing = files.edit().run(
        &fakes::tool_args(json!({
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
        &fakes::tool_args(json!({
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
        .effects(&fakes::tool_args(json!({
            "path": "nope.txt",
            "edits": [{"old_text": "a", "new_text": "b"}]
        })))
        .unwrap();
    assert_eq!(effects.declared.effects, vec![Effect::Writes]);
    assert!(!effects.declared.reversible);
}
