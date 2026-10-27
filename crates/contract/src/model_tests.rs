//! Tests for [`super::HostModel`] (`docs/extensions.md`, "Host calls").

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
        | crate::inbox::Delivery::Message(..)
        | crate::inbox::Delivery::Model(..)
        | crate::inbox::Delivery::Credential(..)
        | crate::inbox::Delivery::Reload(_)
        | crate::inbox::Delivery::McpReconnect(..)
        | crate::inbox::Delivery::Name(..)
        | crate::inbox::Delivery::AddDirectory(..)
        | crate::inbox::Delivery::Rewind(..)
        | crate::inbox::Delivery::Handoff(..)
        | crate::inbox::Delivery::Reply(..)
        | crate::inbox::Delivery::Close(_)
        | crate::inbox::Delivery::Job(_)
        | crate::inbox::Delivery::JobLine(_)
        | crate::inbox::Delivery::ExtensionExec(_)
        | crate::inbox::Delivery::ProposeNotes(..)
        | crate::inbox::Delivery::ExtensionJob(..)
        | crate::inbox::Delivery::Interaction(_)
        | crate::inbox::Delivery::Resolved(..)
        | crate::inbox::Delivery::DelegateUsage { .. }
        | crate::inbox::Delivery::InnerCall(_)
        | crate::inbox::Delivery::ExtensionLog(_)
        | crate::inbox::Delivery::Cancelled
        | crate::inbox::Delivery::Shell(..) => {
            panic!("ExtensionUsage carries its line")
        }
    }
}
