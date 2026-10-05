use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::events::{ReasoningCompleted, TextCompleted, ToolCallRequested};
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
        },
        ToolDefinition {
            name: "a".into(),
            description: "first".into(),
            input_schema: json!({"type": "object"}),
            deferred: true,
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
