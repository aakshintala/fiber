//! Tests for [`crate::error`]: one row per case, pinning each code and
//! each message verbatim.

use contract::ErrorCode;

use super::{ArgumentsError, AskUserError, FileError, ToolError, WebSearchError};
use crate::error::{SessionListError, SessionMessageError};

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

#[test]
fn arguments_error_maps_to_invalid_arguments() {
    let empty = serde_json::Map::new();
    let error = crate::tool_util::parsed::<Args>(&empty).expect_err("a missing path fails");
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::InvalidArguments);
    assert!(
        root.to_string()
            .starts_with("The arguments do not fit the schema:")
    );
}

#[test]
fn a_session_list_failure_names_the_scope() {
    let error = SessionListError::Io {
        path: std::path::PathBuf::from("/run/fiber"),
        message: "denied".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "Could not list sessions at /run/fiber: denied"
    );
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn an_unknown_session_id_is_unreachable() {
    let error = SessionMessageError::Unreachable {
        id: contract::SessionId("abc".to_owned()),
    };
    assert_eq!(error.code(), ErrorCode::Unreachable);
    assert_eq!(error.to_string(), "No running session has the id abc.");
}

#[test]
fn a_rejected_message_forwards_the_target_code_and_drops_retry() {
    let failure = contract::shapes::Failure {
        code: ErrorCode::Closing,
        message: "closing".to_owned(),
        retry_after_ms: Some(5),
        provider: Some(contract::shapes::ProviderFailure {
            name: "test".to_owned(),
            status: Some(429),
            message: "slow".to_owned(),
        }),
    };
    let error = SessionMessageError::Rejected(failure.clone());
    assert_eq!(error.code(), ErrorCode::Closing);
    assert_eq!(error.to_string(), "closing");
    let converted = contract::shapes::Failure::from(&ToolError::from(error));
    assert_eq!(converted.code, ErrorCode::Closing);
    assert_eq!(converted.message, "closing");
    assert_eq!(converted.retry_after_ms, None);
    assert_eq!(converted.provider, None);
}

#[test]
fn an_undelivered_message_names_the_session() {
    let error = SessionMessageError::Io {
        id: contract::SessionId("abc".to_owned()),
        message: "refused".to_owned(),
    };
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "Could not message session abc: refused");
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[test]
fn a_hosted_search_has_none_to_run() {
    let error = WebSearchError::Hosted;
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(
        error.to_string(),
        "The provider runs a hosted search; Fiber has none to run."
    );
}

#[test]
fn an_empty_query_and_two_domain_lists_are_invalid() {
    let error = WebSearchError::EmptyQuery;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(error.to_string(), "`query` must be a non-empty string.");
    let error = WebSearchError::BothDomainLists;
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert_eq!(
        error.to_string(),
        "Pass either `allowed_domains` or `blocked_domains`, never both."
    );
}

#[test]
fn a_backend_failure_forwards_its_code_and_drops_retry() {
    let failure = contract::shapes::Failure {
        code: ErrorCode::Timeout,
        message: "timed out".to_owned(),
        retry_after_ms: Some(5),
        provider: Some(contract::shapes::ProviderFailure {
            name: "test".to_owned(),
            status: None,
            message: "slow".to_owned(),
        }),
    };
    let error = WebSearchError::Backend(failure);
    assert_eq!(error.code(), ErrorCode::Timeout);
    assert_eq!(error.to_string(), "timed out");
    let converted = contract::shapes::Failure::from(&ToolError::from(error));
    assert_eq!(converted.code, ErrorCode::Timeout);
    assert_eq!(converted.retry_after_ms, None);
    assert_eq!(converted.provider, None);
}

#[test]
fn unreadable_questions_name_the_field() {
    let source = serde_json::from_str::<String>("3x").expect_err("trailing text fails");
    let error = AskUserError::QuestionsUnreadable { source };
    assert_eq!(error.code(), ErrorCode::InvalidArguments);
    assert!(
        error
            .to_string()
            .starts_with("`questions` cannot be read: "),
        "unexpected message: {error}"
    );
}

#[test]
fn a_mismatched_answer_is_a_tool_error() {
    let error = AskUserError::AnswerMismatch;
    assert_eq!(error.code(), ErrorCode::ToolError);
    assert_eq!(error.to_string(), "The answer does not fit the form.");
    let root = ToolError::from(error);
    assert_eq!(root.code(), ErrorCode::ToolError);
}

#[derive(Debug, serde::Deserialize)]
struct Args {
    #[allow(dead_code, reason = "the shape under test names a path")]
    path: String,
}
