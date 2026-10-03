use std::collections::BTreeMap;

use serde_json::json;

use super::*;
use crate::events::{ReasoningCompleted, TextCompleted, ToolCallRequested};
use crate::shapes::Tokens;
use crate::{GenerationId, ProviderCallId};

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
        }),
    ]);
    assert_eq!(none.text(), "");
}
