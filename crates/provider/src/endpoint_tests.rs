use serde_json::json;

use super::{Compat, Endpoint, ReasoningField};

fn endpoint(max_output_tokens: Option<u64>) -> Endpoint {
    Endpoint {
        max_output_tokens,
        ..Endpoint::default()
    }
}

#[test]
fn output_limit_is_the_smaller_of_the_two_that_are_set() {
    assert_eq!(endpoint(None).output_limit(None), None);
    assert_eq!(endpoint(None).output_limit(Some(1)), Some(1));
    assert_eq!(endpoint(Some(4096)).output_limit(None), Some(4096));
    assert_eq!(endpoint(Some(4096)).output_limit(Some(1)), Some(1));
    assert_eq!(endpoint(Some(4096)).output_limit(Some(9000)), Some(4096));
}

#[test]
fn compat_flags_are_read_from_data_with_thinking_budget_unset_when_absent() {
    assert!(!Compat::from_data(&serde_json::Map::new()).thinking_budget);
    assert!(!Compat::from_data(&serde_json::Map::new()).bearer);
    for (data, want) in [
        (json!({"thinking_budget": true}), true),
        (json!({"thinking_budget": false}), false),
        (json!({"thinking_budget": "yes"}), false),
    ] {
        assert_eq!(
            Compat::from_data(data.as_object().unwrap()).thinking_budget,
            want,
            "{data}"
        );
    }
    for (data, want) in [
        (json!({"bearer": true}), true),
        (json!({"bearer": false}), false),
        (json!({"bearer": "yes"}), false),
    ] {
        assert_eq!(
            Compat::from_data(data.as_object().unwrap()).bearer,
            want,
            "{data}"
        );
    }
    assert!(!Compat::from_data(&serde_json::Map::new()).bedrock_invoke);
    for (data, want) in [
        (json!({"bedrock_invoke": true}), true),
        (json!({"bedrock_invoke": false}), false),
        (json!({"bedrock_invoke": 1}), false),
    ] {
        assert_eq!(
            Compat::from_data(data.as_object().unwrap()).bedrock_invoke,
            want,
            "{data}"
        );
    }
}

#[test]
fn the_converse_flags_are_read_from_data_and_unset_when_absent_or_unknown() {
    let empty = Compat::from_data(&serde_json::Map::new());
    assert!(!empty.cache_point);
    assert_eq!(empty.reasoning_field, None);
    for (data, want) in [
        (json!({"cache_point": true}), true),
        (json!({"cache_point": false}), false),
        (json!({"cache_point": "yes"}), false),
    ] {
        assert_eq!(
            Compat::from_data(data.as_object().unwrap()).cache_point,
            want,
            "{data}"
        );
    }
    for (data, want) in [
        (
            json!({"reasoning_field": "reasoning_effort"}),
            Some(ReasoningField::ReasoningEffort),
        ),
        (
            json!({"reasoning_field": "reasoningConfig"}),
            Some(ReasoningField::ReasoningConfig),
        ),
        (json!({"reasoning_field": "reasoning_config"}), None),
        (json!({"reasoning_field": true}), None),
    ] {
        assert_eq!(
            Compat::from_data(data.as_object().unwrap()).reasoning_field,
            want,
            "{data}"
        );
    }
}
