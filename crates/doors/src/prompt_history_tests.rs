//! The line `append` writes and the path it writes to.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::path::Path;

use contract::SessionId;
use contract::shapes::ContentPart;

use super::{append, path};

fn text(text: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text { text: text.into() }]
}

#[test]
fn the_history_is_beside_the_projects_sessions() {
    assert_eq!(
        path(Path::new("/h/projects/k/sessions/s_1")),
        Some(Path::new("/h/projects/k/history.jsonl").to_owned())
    );
    assert_eq!(path(Path::new("s_1")), None);
}

#[test]
fn each_append_is_one_whole_line_in_order() {
    let temp = fakes::TempDir::new("ph");
    let file = temp.path().join("history.jsonl");
    let session = SessionId("s_0123456789abcdef".into());
    append(&file, 1_791_234_567_890, &session, &text("fix the tests")).unwrap();
    append(&file, 7, &session, &text("say \"hi\"\n")).unwrap();
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        concat!(
            r#"{"ts":1791234567890,"session_id":"s_0123456789abcdef","content":[{"type":"text","text":"fix the tests"}]}"#,
            "\n",
            r#"{"ts":7,"session_id":"s_0123456789abcdef","content":[{"type":"text","text":"say \"hi\"\n"}]}"#,
            "\n",
        )
    );
}

#[test]
fn an_append_with_no_project_directory_fails() {
    let temp = fakes::TempDir::new("ph");
    let file = temp.path().join("gone/history.jsonl");
    let session = SessionId("s_1".into());
    assert!(append(&file, 1, &session, &text("x")).is_err());
    assert!(!file.exists());
}
