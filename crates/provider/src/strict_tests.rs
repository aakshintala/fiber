use serde_json::json;

use super::fits;

#[test]
fn the_opencode_probe_schema_fits() {
    // research/opencode-probe: OpenCode Go accepted it with `strict: true`.
    let schema = json!({
        "type": "object",
        "properties": {"city": {"type": "string"}},
        "required": ["city"],
        "additionalProperties": false
    });
    assert!(fits(&schema));
}

#[test]
fn the_openai_probe_schema_with_an_optional_property_does_not() {
    // research/openai-responses-probe: refused with `strict: true`.
    let schema = json!({
        "type": "object",
        "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
        "required": ["a"]
    });
    assert!(!fits(&schema));
}

#[test]
fn a_nested_object_must_fit_too() {
    let inner_open = json!({
        "type": "object",
        "properties": {
            "where": {"type": "object", "properties": {"x": {"type": "number"}}, "required": ["x"]}
        },
        "required": ["where"],
        "additionalProperties": false
    });
    assert!(!fits(&inner_open));
    let nested = json!({
        "type": "object",
        "properties": {
            "points": {"type": "array", "items": {
                "type": "object",
                "properties": {"x": {"type": "number", "description": "x"}},
                "required": ["x"],
                "additionalProperties": false
            }},
            "mode": {"type": "string", "enum": ["a", "b"]}
        },
        "required": ["mode", "points"],
        "additionalProperties": false
    });
    assert!(fits(&nested));
}

#[test]
fn a_keyword_outside_the_subset_or_a_type_list_does_not_fit() {
    let keyword = json!({
        "type": "object",
        "properties": {"s": {"type": "string", "minLength": 1}},
        "required": ["s"],
        "additionalProperties": false
    });
    assert!(!fits(&keyword));
    let type_list = json!({
        "type": "object",
        "properties": {"s": {"type": ["string", "null"]}},
        "required": ["s"],
        "additionalProperties": false
    });
    assert!(!fits(&type_list));
}

#[test]
fn required_must_name_each_property_once() {
    let duplicated = json!({
        "type": "object",
        "properties": {"a": {"type": "string"}, "b": {"type": "string"}},
        "required": ["a", "a"],
        "additionalProperties": false
    });
    assert!(!fits(&duplicated));
}

#[test]
fn the_root_must_be_an_object() {
    assert!(!fits(
        &json!({"type": "array", "items": {"type": "string"}})
    ));
    assert!(!fits(&json!(true)));
}
