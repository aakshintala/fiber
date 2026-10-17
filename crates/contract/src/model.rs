//! The `host.model` seam (`docs/extensions.md`, "Host calls"): one model
//! request through the session's routing and credentials. `main` injects the
//! implementation; `extensions` only holds the trait object.

use std::sync::Arc;
use std::time::Instant;

use crate::events::UsageRecorded;
use crate::shapes::{ContentPart, Failure, Tokens};

/// One model request an extension makes through `host.model`.
pub struct HostModelRequest {
    /// The model reference, `provider/model`.
    pub model: String,
    /// The system text, when the caller passed one.
    pub system: Option<String>,
    /// The conversation.
    pub messages: Vec<HostMessage>,
    /// The request's own output limit.
    pub max_tokens: u64,
}

/// One message in a `host.model` request.
pub struct HostMessage {
    /// Who sent it.
    pub role: HostRole,
    /// Its content parts; a string content is one `Text` part.
    pub content: Vec<ContentPart>,
}

/// Who sent a `host.model` message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostRole {
    /// A person's message.
    User,
    /// A model's reply.
    Assistant,
}

/// What a `host.model` call returned: the successful attempt's text and
/// counts.
pub struct HostModelReply {
    /// The reply's text.
    pub text: String,
    /// The call's tokens.
    pub tokens: Tokens,
    /// The vendor's own figure where the reply reports one, else the
    /// priced figure, else `None`.
    pub cost: Option<f64>,
}

/// What bounds a `host.model` call: its deadline, clock, cancel signal and
/// the sink for one `usage_recorded` line per attempt.
pub struct Bound {
    /// The callback's deadline on `clock`; `None` never expires.
    pub deadline: Option<Instant>,
    /// The session clock the wait reads.
    pub clock: Arc<dyn crate::clock::Clock>,
    /// Set when the callback ends or the hub seals.
    pub cancel: Arc<dyn crate::tool::Cancel>,
    /// Takes one `usage_recorded` line per attempt, in order, failed ones
    /// included; every line carries `extension == Some(name)`.
    pub lines: Arc<dyn Fn(UsageRecorded) + Send + Sync>,
}

/// One model request through the session's routing and credentials, made by
/// the extension `extension` names. Blocking; called from the worker thread
/// the admit spawns. A wait ends at `bound.deadline` or `bound.cancel` with
/// a `timeout` failure and no send; one line per attempt goes to
/// `bound.lines`.
pub trait HostModel: Send + Sync {
    /// Runs the request to completion with the session's retry policy.
    fn call(
        &self,
        extension: &str,
        request: HostModelRequest,
        bound: Bound,
    ) -> Result<HostModelReply, Failure>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeHost;

    impl HostModel for FakeHost {
        fn call(
            &self,
            _extension: &str,
            _request: HostModelRequest,
            _bound: Bound,
        ) -> Result<HostModelReply, Failure> {
            Err(Failure {
                code: crate::ErrorCode::Cancelled,
                message: "fake".to_owned(),
                retry_after_ms: None,
                provider: None,
            })
        }
    }

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn host_model_is_send_and_sync() {
        assert_send_sync::<Arc<dyn HostModel>>();
        let _host: Arc<dyn HostModel> = Arc::new(FakeHost);
    }

    #[test]
    fn extension_usage_carries_its_line() {
        let line = UsageRecorded {
            generation_id: crate::GenerationId("g_1".to_owned()),
            model: "p/m".to_owned(),
            tokens: Tokens {
                input: 1,
                cache_read: 0,
                cache_write: std::collections::BTreeMap::new(),
                output: 2,
            },
            input_bytes: 10,
            input_media: None,
            web_searches: None,
            cost: None,
            subscription: None,
            extension: Some("tag".to_owned()),
            origin_session_id: None,
            reviewer: None,
        };
        let delivery = crate::inbox::Delivery::ExtensionUsage(line.clone());
        match delivery {
            crate::inbox::Delivery::ExtensionUsage(held) => assert_eq!(held, line),
            crate::inbox::Delivery::Prompt(..)
            | crate::inbox::Delivery::Steer(..)
            | crate::inbox::Delivery::SteerDrop(..)
            | crate::inbox::Delivery::Message(_)
            | crate::inbox::Delivery::Model(..)
            | crate::inbox::Delivery::Credential(..)
            | crate::inbox::Delivery::Name(..)
            | crate::inbox::Delivery::Rewind(..)
            | crate::inbox::Delivery::Handoff(..)
            | crate::inbox::Delivery::Reply(..)
            | crate::inbox::Delivery::Close(_)
            | crate::inbox::Delivery::Job(_)
            | crate::inbox::Delivery::JobLine(_)
            | crate::inbox::Delivery::ExtensionExec(_)
            | crate::inbox::Delivery::Interaction(_)
            | crate::inbox::Delivery::Resolved(..)
            | crate::inbox::Delivery::InnerCall(_)
            | crate::inbox::Delivery::ExtensionLog(_)
            | crate::inbox::Delivery::Cancelled
            | crate::inbox::Delivery::Shell(..) => {
                panic!("ExtensionUsage carries its line")
            }
        }
    }
}
