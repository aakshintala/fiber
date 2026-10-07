use std::collections::BTreeMap;

use contract::provider::{CallUsage, InputSize};
use contract::shapes::Tokens;
use contract::{GenerationId, ProviderCallId};

use super::carried;
use crate::Error;

fn usage(generation: &str) -> CallUsage {
    CallUsage {
        generation_id: GenerationId(generation.into()),
        tokens: Tokens {
            input: 7,
            cache_read: 1,
            cache_write: BTreeMap::from([("5m".to_owned(), 2)]),
            output: 3,
        },
        web_searches: Some(1),
        input_size: InputSize {
            bytes: 100,
            media: true,
        },
    }
}

fn reply(generation: &str) -> contract::provider::Reply {
    let usage = usage(generation);
    contract::provider::Reply {
        actions: Vec::new(),
        finish: contract::provider::Finish::Completed,
        generation_id: usage.generation_id.clone(),
        tokens: usage.tokens.clone(),
        web_searches: usage.web_searches,
        cost: Some(0.5),
        input_size: usage.input_size,
    }
}

fn sized(bytes: u64) -> InputSize {
    InputSize {
        bytes,
        media: false,
    }
}

#[test]
fn a_completed_reply_with_an_id_carries_its_usage_with_the_call_s_input_size() {
    let mut completed = reply("gen_1");
    completed.input_size = sized(1);
    let carried = carried(&Ok(completed), sized(900));
    let mut want = usage("gen_1");
    want.input_size = sized(900);
    assert_eq!(carried, Some(Box::new(want)));
}

#[test]
fn a_completed_reply_with_an_empty_id_carries_nothing() {
    let mut completed = reply("");
    completed.generation_id = GenerationId(String::new());
    // An empty id carries none even when the reply holds tokens.
    assert_eq!(carried(&Ok(completed), sized(900)), None);
}

#[test]
fn a_failed_decode_carries_its_partial_with_the_call_s_input_size() {
    let mut partial = usage("gen_1");
    partial.input_size = sized(1);
    let failed: Result<contract::provider::Reply, (Error, Option<CallUsage>)> =
        Err((Error::StreamIncomplete("ended".into()), Some(partial)));
    let mut want = usage("gen_1");
    want.input_size = sized(900);
    assert_eq!(carried(&failed, sized(900)), Some(Box::new(want)));
}

#[test]
fn a_failed_decode_without_a_partial_carries_nothing() {
    let failed: Result<contract::provider::Reply, (Error, Option<CallUsage>)> =
        Err((Error::StreamIncomplete("ended".into()), None));
    assert_eq!(carried(&failed, sized(900)), None);
}

#[test]
fn a_provider_id_on_a_completed_reply_does_not_change_what_it_carries() {
    // The reply's own provider ids (tool calls) are not the generation;
    // only an empty generation id suppresses the carry.
    let mut completed = reply("gen_1");
    completed
        .actions
        .push(contract::provider::ReplyAction::ToolCall(
            contract::events::ToolCallRequested {
                name: "get_weather".into(),
                arguments: serde_json::json!({}),
                provider_id: Some(ProviderCallId("call_1".into())),
                repair: None,
                ran_by: None,
                provider_item: None,
            },
        ));
    let carried = carried(&Ok(completed), sized(5));
    assert!(carried.is_some());
}
