//! The code an unknown-model reply maps to, from the recorded replies
//! (`research/retry-signals`, `research/provider-errors`).

use std::path::Path;

use contract::ErrorCode;
use serde_json::Value;

use crate::Error;

/// The recorded reply: its status and body.
fn recorded(path: &str) -> Error {
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../research")
        .join(path);
    let value: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    Error::Status {
        status: u16::try_from(value["status"].as_u64().unwrap()).unwrap(),
        body: value["body"].as_str().unwrap().to_owned(),
        retry_after: None,
        should_retry: None,
    }
}

fn status(status: u16, body: &str) -> ErrorCode {
    Error::Status {
        status,
        body: body.to_owned(),
        retry_after: None,
        should_retry: None,
    }
    .code()
}

#[test]
fn a_recorded_unknown_model_is_model_not_found() {
    for path in [
        "retry-signals/raw/muse.unknown-model.json",
        "retry-signals/raw/openai.unknown-model.json",
        "retry-signals/raw/anthropic.unknown-model.json",
        "retry-signals/raw/gemini.unknown-model.json",
        "retry-signals/raw/openrouter.unknown-model.json",
        "provider-errors/raw/or-completions-haiku.unknown-model.json",
    ] {
        assert_eq!(recorded(path).code(), ErrorCode::ModelNotFound, "{path}");
    }
}

#[test]
fn only_the_unknown_model_shapes_are_model_not_found() {
    assert_eq!(status(404, "{}"), ErrorCode::ModelNotFound);
    assert_eq!(status(400, "{}"), ErrorCode::InvalidRequest);
    assert_eq!(
        status(400, r#"{"error":{"code":"model_not_found"}}"#),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(
        status(400, r#"{"error":{"type":"not_found_error"}}"#),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(
        status(400, r#"{"error":{"message":"x is not a valid model ID"}}"#),
        ErrorCode::ModelNotFound,
    );
    assert_eq!(
        status(400, r#"{"error":{"message":"x is a valid model ID"}}"#),
        ErrorCode::InvalidRequest,
    );
    assert_eq!(
        status(400, r#"{"error":{"type":"invalid_request_error"}}"#),
        ErrorCode::InvalidRequest,
    );
}
