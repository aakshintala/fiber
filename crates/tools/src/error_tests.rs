//! Tests for [`crate::error`]: one row per case, pinning each code and
//! each message verbatim.

use contract::ErrorCode;

use super::{ArgumentsError, FileError, ToolError};

#[test]
fn arguments_error_reports_the_schema_mismatch() {
    let empty = serde_json::Map::new();
    let error = crate::tool_util::parsed::<Args>(&empty).expect_err("a missing path fails");
    assert!(matches!(error, ArgumentsError::DoNotFit { .. }));
    let text = error.to_string();
    assert!(
        text.starts_with("The arguments do not fit the schema:"),
        "unexpected message: {text}"
    );
    assert!(
        text.contains("missing field `path`"),
        "unexpected message: {text}"
    );
}

#[test]
fn a_moved_read_names_the_read() {
    let error = FileError::ChangedBeforeRead {
        raw: "a.txt".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::PathChanged);
    assert_eq!(
        error.to_string(),
        "`a.txt` changed between the permission check and the read. Nothing was read."
    );
}

#[test]
fn a_moved_write_names_the_write() {
    let error = FileError::ChangedBeforeWrite {
        raw: "a.txt".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::PathChanged);
    assert_eq!(
        error.to_string(),
        "`a.txt` changed between the permission check and the write. Nothing was written."
    );
}

#[test]
fn the_root_carries_its_case_code_and_message() {
    let root = ToolError::from(FileError::ChangedBeforeWrite {
        raw: "a.txt".to_owned(),
    });
    assert_eq!(root.code(), ErrorCode::PathChanged);
    assert_eq!(
        root.to_string(),
        "`a.txt` changed between the permission check and the write. Nothing was written."
    );
}

#[test]
fn a_case_converts_to_a_failure_with_no_retry_or_provider() {
    let root = ToolError::from(FileError::ChangedBeforeWrite {
        raw: "a.txt".to_owned(),
    });
    let failure = contract::shapes::Failure::from(&root);
    assert_eq!(failure.code, ErrorCode::PathChanged);
    assert_eq!(
        failure.message,
        "`a.txt` changed between the permission check and the write. Nothing was written."
    );
    assert_eq!(failure.retry_after_ms, None);
    assert_eq!(failure.provider, None);
    let owned = contract::shapes::Failure::from(root);
    assert_eq!(owned, failure);
}

#[derive(Debug, serde::Deserialize)]
struct Args {
    #[allow(dead_code, reason = "the shape under test names a path")]
    path: String,
}
