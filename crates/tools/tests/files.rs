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
use contract::shapes::Effect;
use contract::tool::Tool;
use fakes::{CancelToken, Recorder, TempDir};
use serde_json::json;
use tools::Files;

#[test]
fn read_returns_the_file_text_and_a_continue_notice() {
    let dir = TempDir::new("fiber-files-read");
    fs::write(dir.path().join("a.txt"), "x\ny\nz\n").unwrap();
    let files = Files::new(dir.path().to_path_buf());
    let output = files.read().run(
        &fakes::tool_args(json!({"path": "a.txt", "offset": 2, "limit": 1})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(output.error.is_none());
    assert_eq!(
        fakes::text_of(&output),
        "y\n[Showing lines 2-2 of 3. Continue with offset=3.]"
    );
    let effects = files
        .read()
        .effects(&fakes::tool_args(json!({"path": "a.txt"})))
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
        &fakes::tool_args(json!({"path": "nope.txt"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        missing.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::NotFound)
    );
    fs::create_dir(dir.path().join("sub")).unwrap();
    let directory = files.read().run(
        &fakes::tool_args(json!({"path": "sub"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        directory.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::UnsupportedFile)
    );
    assert!(
        fakes::text_of(&directory).contains("shell"),
        "{}",
        fakes::text_of(&directory)
    );
}

#[test]
fn write_creates_a_file_and_refuses_an_unread_replace() {
    let dir = TempDir::new("fiber-files-write");
    let files = Files::new(dir.path().to_path_buf());
    let created = files.write().run(
        &fakes::tool_args(json!({"path": "a.txt", "content": "x\ny\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert!(created.error.is_none(), "{}", fakes::text_of(&created));
    assert!(
        fakes::text_of(&created).contains("Created "),
        "{}",
        fakes::text_of(&created)
    );
    assert!(
        fakes::text_of(&created).contains("4 bytes, 2 lines."),
        "{}",
        fakes::text_of(&created)
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
        .effects(&fakes::tool_args(json!({"path": "b.txt", "content": "z"})))
        .unwrap();
    assert_eq!(effects.declared.effects, vec![Effect::Writes]);
    assert!(effects.declared.reversible);

    fs::write(dir.path().join("c.txt"), "old\n").unwrap();
    let stale = files.write().run(
        &fakes::tool_args(json!({"path": "c.txt", "content": "new\n"})),
        &CancelToken::new(),
        &Recorder::default(),
    );
    assert_eq!(
        stale.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::StaleFile)
    );
    assert_eq!(fs::read(dir.path().join("c.txt")).unwrap(), b"old\n");
}
