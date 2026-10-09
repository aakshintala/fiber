//! One call the provider ran itself, such as a hosted web search, shared by
//! the protocols that read one back (`docs/tools.md`, "Hosted by the
//! provider").

use contract::ErrorCode;
use contract::events::{CallStatus, ToolCallCompleted};
use contract::shapes::{ContentPart, Failure};
use serde_json::Value;

/// A hosted search's success: the result URLs, one per line, with the raw
/// block as `provider_item`.
pub(crate) fn completed(item: Value, urls: &[&str]) -> ToolCallCompleted {
    ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text {
            text: urls.join("\n"),
        }],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: Some(item),
    }
}

/// A hosted search's failure: the vendor's error code in the message, as
/// both the content and the error, with the raw block as `provider_item`.
pub(crate) fn failed(item: Value, code: &str) -> ToolCallCompleted {
    let message = format!("The provider's search failed: {code}.");
    ToolCallCompleted {
        status: CallStatus::Failed,
        reason: None,
        error: Some(Failure {
            code: ErrorCode::ToolError,
            message: message.clone(),
            retry_after_ms: None,
            provider: None,
        }),
        process: None,
        content: vec![ContentPart::Text { text: message }],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: Some(item),
    }
}
