use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::events::{
    CallStatus, ReasoningCompleted, TextCompleted, ToolCallCompleted, ToolCallRequested,
};
use crate::shapes::Failure;
use crate::shapes::Tokens;
use crate::{ActionId, GenerationId, ProviderCallId};

fn reply(actions: Vec<ReplyAction>) -> Reply {
    Reply {
        actions,
        finish: Finish::Completed,
        generation_id: GenerationId("g".into()),
        tokens: Tokens {
            input: 0,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
        web_searches: None,
        cost: None,
        input_size: InputSize::default(),
    }
}

#[test]
fn text_joins_text_parts_in_order_and_skips_the_rest() {
    let mixed = reply(vec![
        ReplyAction::Text(TextCompleted {
            text: "A".into(),
            provider_item: None,
        }),
        ReplyAction::Reasoning(ReasoningCompleted {
            text: "think".into(),
            provider_item: None,
        }),
        ReplyAction::ToolCall(ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({}),
            provider_id: Some(ProviderCallId("c".into())),
            repair: None,
            ran_by: None,
            provider_item: None,
        }),
        ReplyAction::Hosted(HostedCall {
            call: ToolCallRequested {
                name: "web_search".into(),
                arguments: json!({"query": "q"}),
                provider_id: Some(ProviderCallId("srvtoolu_01".into())),
                repair: None,
                ran_by: None,
                provider_item: Some(json!({"type": "server_tool_use"})),
            },
            completed: ToolCallCompleted {
                status: CallStatus::Completed,
                reason: None,
                error: None,
                process: None,
                content: vec![crate::shapes::ContentPart::Text {
                    text: "hosted result".into(),
                }],
                details: None,
                artifact: None,
                changes: None,
                control: None,
                changed_by: None,
                provider_item: Some(json!({"type": "web_search_tool_result"})),
            },
        }),
        ReplyAction::Text(TextCompleted {
            text: "B".into(),
            provider_item: None,
        }),
    ]);
    assert_eq!(mixed.text(), "AB");

    let none = reply(vec![
        ReplyAction::Reasoning(ReasoningCompleted {
            text: "think".into(),
            provider_item: None,
        }),
        ReplyAction::ToolCall(ToolCallRequested {
            name: "get_weather".into(),
            arguments: json!({}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        }),
    ]);
    assert_eq!(none.text(), "");
}

struct Fake;

impl Provider for Fake {
    fn call(&self, _request: &ModelRequest) -> Box<dyn ModelCall> {
        panic!("Fake::call is not used")
    }
}

#[test]
fn default_wire_tools_is_each_definition_as_an_object_in_name_order() {
    let tools = vec![
        ToolDefinition {
            name: "b".into(),
            description: "second".into(),
            input_schema: json!({"type": "object"}),
            deferred: false,
            hosted: None,
        },
        ToolDefinition {
            name: "a".into(),
            description: "first".into(),
            input_schema: json!({"type": "object"}),
            deferred: true,
            hosted: None,
        },
    ];
    let wired = Fake.wire_tools(&tools);
    let want: Vec<Map<String, Value>> = ["a", "b"]
        .iter()
        .map(|name| {
            let tool = tools.iter().find(|t| &t.name == name).unwrap();
            serde_json::to_value(tool)
                .unwrap()
                .as_object()
                .unwrap()
                .clone()
        })
        .collect();
    assert_eq!(wired, want);
}

#[test]
fn a_user_without_images_serialises_without_the_key_and_reads_back() {
    let plain = Input::User {
        text: "hi".into(),
        images: Vec::new(),
    };
    let value = serde_json::to_value(&plain).unwrap();
    assert_eq!(value, json!({"type": "user", "text": "hi"}));
    assert!(value.get("images").is_none(), "{value}");
    assert_eq!(serde_json::from_value::<Input>(value).unwrap(), plain);
    // A line written before the field existed has none.
    let old = json!({"type": "user", "text": "hi"});
    assert_eq!(serde_json::from_value::<Input>(old).unwrap(), plain);
}

#[test]
fn a_user_with_images_round_trips() {
    let with = Input::User {
        text: "look".into(),
        images: vec![ImageRef {
            path: "artifacts/i_1.png".into(),
            mime_type: "image/png".into(),
            width: 3,
            height: 2,
        }],
    };
    let value = serde_json::to_value(&with).unwrap();
    assert_eq!(value["images"][0]["path"], "artifacts/i_1.png");
    assert_eq!(serde_json::from_value::<Input>(value).unwrap(), with);
}

#[test]
fn a_tool_result_without_images_serialises_without_the_key_and_reads_back() {
    let plain = Input::ToolResult {
        action_id: ActionId("a_1".into()),
        text: "ok".into(),
        is_error: false,
        images: Vec::new(),
    };
    let value = serde_json::to_value(&plain).unwrap();
    assert!(value.get("images").is_none(), "{value}");
    assert_eq!(serde_json::from_value::<Input>(value).unwrap(), plain);
    // A line written before the field existed has none.
    let old = json!({"type": "tool_result", "action_id": "a_1", "text": "ok"});
    assert_eq!(serde_json::from_value::<Input>(old).unwrap(), plain);
}

#[test]
fn a_tool_result_with_images_round_trips() {
    let with = Input::ToolResult {
        action_id: ActionId("a_1".into()),
        text: "ok".into(),
        is_error: true,
        images: vec![ImageRef {
            path: "artifacts/i_1.png".into(),
            mime_type: "image/png".into(),
            width: 3,
            height: 2,
        }],
    };
    let value = serde_json::to_value(&with).unwrap();
    assert_eq!(value["images"][0]["path"], "artifacts/i_1.png");
    assert_eq!(serde_json::from_value::<Input>(value).unwrap(), with);
}

#[test]
fn a_request_without_a_session_dir_reads_an_empty_one() {
    let request = json!({
        "system_prompt": "s", "tools": [], "tool_choice": "auto",
        "cache_lifetime": "5m", "cache_key": "k", "conversation": [],
    });
    let read: ModelRequest = serde_json::from_value(request).unwrap();
    assert_eq!(read.session_dir, PathBuf::new());
}

#[test]
fn a_tool_definition_without_hosted_has_no_hosted_key_and_reads_back() {
    let plain = ToolDefinition {
        name: "read".into(),
        description: "d".into(),
        input_schema: json!({"type": "object"}),
        deferred: false,
        hosted: None,
    };
    let value = serde_json::to_value(&plain).unwrap();
    assert!(value.get("hosted").is_none(), "{value}");
    assert_eq!(
        serde_json::from_value::<ToolDefinition>(value).unwrap(),
        plain
    );
    let hosted = ToolDefinition {
        hosted: Some("web_search_20250305".into()),
        ..plain
    };
    let value = serde_json::to_value(&hosted).unwrap();
    assert_eq!(value["hosted"], "web_search_20250305");
    assert_eq!(
        serde_json::from_value::<ToolDefinition>(value).unwrap(),
        hosted
    );
}

fn call_usage() -> CallUsage {
    CallUsage {
        generation_id: GenerationId("gen_9".into()),
        tokens: Tokens {
            input: 7,
            cache_read: 1,
            cache_write: BTreeMap::from([("5m".to_owned(), 2)]),
            output: 3,
        },
        web_searches: Some(2),
        input_size: InputSize {
            bytes: 900,
            media: true,
        },
    }
}

#[test]
fn a_reply_s_usage_carries_its_generation_tokens_searches_and_input_size() {
    let usage = call_usage();
    let mut acted = reply(Vec::new());
    acted.generation_id = usage.generation_id.clone();
    acted.tokens = usage.tokens.clone();
    acted.web_searches = usage.web_searches;
    acted.input_size = usage.input_size;
    assert_eq!(acted.usage(), usage);
}

#[test]
fn a_call_error_s_usage_is_what_each_variant_carries() {
    let usage = call_usage();
    let failed = CallError::Failed {
        failure: Failure {
            code: crate::ErrorCode::RateLimited,
            message: "slow".into(),
            retry_after_ms: None,
            provider: None,
        },
        should_retry: None,
        usage: Some(Box::new(usage.clone())),
    };
    assert_eq!(failed.usage(), Some(&usage));
    let cancelled = CallError::Cancelled {
        usage: Some(Box::new(usage.clone())),
    };
    assert_eq!(cancelled.usage(), Some(&usage));
    let failed_none = CallError::Failed {
        failure: Failure {
            code: crate::ErrorCode::RateLimited,
            message: "slow".into(),
            retry_after_ms: None,
            provider: None,
        },
        should_retry: None,
        usage: None,
    };
    assert_eq!(failed_none.usage(), None);
    let cancelled_none = CallError::Cancelled { usage: None };
    assert_eq!(cancelled_none.usage(), None);
}
