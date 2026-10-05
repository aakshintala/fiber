//! Builders for a call's completion and its decision line: what the model
//! is told when a call never ran, was denied or failed before it ran, and
//! the `permission_resolved` line behind each.

use contract::events::{CallStatus, DecidedBy, Decision, PermissionResolved, ToolCallCompleted};
use contract::shapes::{ContentPart, Failure};
use contract::{ErrorCode, RequestId};

/// How a call the reply cut off completes (`docs/loop.md`, "A reply cut off
/// by the output limit").
pub(crate) fn truncated() -> ToolCallCompleted {
    failed(
        ErrorCode::OutputTruncated,
        "Your reply reached its output limit, so this call did not run and its \
         arguments may be incomplete. Make the call again, split into smaller \
         calls if it was large."
            .to_owned(),
    )
}

/// A `permission_resolved` line carrying `request_id`, `decision`,
/// `decided_by`, `reason` and `feedback`: every other key is absent. A call
/// whose answer remembered something sets `grant` or `rule` on it.
pub(crate) fn resolved(
    request_id: Option<RequestId>,
    decision: Decision,
    decided_by: DecidedBy,
    reason: Option<String>,
    feedback: Option<String>,
) -> PermissionResolved {
    PermissionResolved {
        request_id,
        decision,
        decided_by,
        reason,
        feedback,
        grant: None,
        rule: None,
        reviewer: None,
    }
}

/// A call that was denied with `reason`, the model told `text`.
pub(crate) fn denied(reason: &str, text: String) -> Box<ToolCallCompleted> {
    Box::new(ToolCallCompleted {
        status: CallStatus::Denied,
        reason: Some(reason.to_owned()),
        ..completed(text, None)
    })
}

/// A call that failed with `code` before it ran, the model told `message`.
pub(crate) fn failed(code: ErrorCode, message: String) -> ToolCallCompleted {
    ToolCallCompleted {
        status: CallStatus::Failed,
        ..completed(
            message.clone(),
            Some(Failure {
                code,
                message,
                retry_after: None,
                provider: None,
            }),
        )
    }
}

pub(crate) fn completed(text: String, error: Option<Failure>) -> ToolCallCompleted {
    ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error,
        process: None,
        content: if text.is_empty() {
            Vec::new()
        } else {
            vec![ContentPart::Text { text }]
        },
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
    }
}
