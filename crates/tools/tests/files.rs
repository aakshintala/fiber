//! The file tools through [`tools::Files`] (`docs/tools.md`, "File tools").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;

use contract::ErrorCode;
use contract::shapes::{ContentPart, Effect};
use contract::tool::Tool;
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::{Map, Value, json};
use tools::Files;

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

#[test]
fn read_returns_the_file_text_and_a_continue_notice() {
    let dir = TempDir::new("fiber-files-read");
    fs::write(dir.path().join("a.txt"), "x\ny\nz\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.read().run(
        &args(json!({"path": "a.txt", "offset": 2, "limit": 1})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        text(&output),
        "y\n[Showing lines 2-2 of 3. Continue with offset=3.]"
    );
    let effects = files
        .read()
        .effects(&args(json!({"path": "a.txt"})))
        .unwrap();
    assert_eq!(effects.declared.effects, vec![Effect::Reads]);
    assert!(effects.declared.reversible);
    let subject = effects.subject.expect("subject");
    assert!(subject.ends_with("/a.txt"), "{subject}");
    let prefix = effects.prefix.expect("prefix");
    assert!(prefix.ends_with('/'), "{prefix}");
}

#[test]
fn read_of_a_missing_file_is_not_found_and_a_directory_points_at_the_shell() {
    let dir = TempDir::new("fiber-files-read-miss");
    let files = Files::new(dir.path().to_path_buf());
    let missing = files.read().run(
        &args(json!({"path": "nope.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        missing.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::NotFound)
    );
    fs::create_dir(dir.path().join("sub")).unwrap();
    let directory = files.read().run(
        &args(json!({"path": "sub"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        directory.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::UnsupportedFile)
    );
    assert!(text(&directory).contains("shell"), "{}", text(&directory));
}

#[test]
fn write_creates_a_file_and_refuses_an_unread_replace() {
    let dir = TempDir::new("fiber-files-write");
    let files = Files::new(dir.path().to_path_buf());
    let created = files.write().run(
        &args(json!({"path": "a.txt", "content": "x\ny\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(created.error.is_none(), "{}", text(&created));
    assert!(text(&created).contains("Created "), "{}", text(&created));
    assert!(
        text(&created).contains("4 bytes, 2 lines."),
        "{}",
        text(&created)
    );
    assert_eq!(
        created
            .changes
            .as_ref()
            .map(|changes| (changes[0].added, changes[0].removed)),
        Some((2, 0))
    );
    assert_eq!(fs::read(dir.path().join("a.txt")).unwrap(), b"x\ny\n");
    let effects = files
        .write()
        .effects(&args(json!({"path": "b.txt", "content": "z"})))
        .unwrap();
    assert_eq!(effects.declared.effects, vec![Effect::Writes]);
    assert!(effects.declared.reversible);

    fs::write(dir.path().join("c.txt"), "old\n").unwrap();
    let stale = files.write().run(
        &args(json!({"path": "c.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        stale.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::StaleFile)
    );
    assert_eq!(fs::read(dir.path().join("c.txt")).unwrap(), b"old\n");
}
