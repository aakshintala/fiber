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

fn with_property(property: serde_json::Value) -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {"p": property},
        "required": ["p"],
        "additionalProperties": false
    })
}

#[test]
fn enums_within_openais_limits_fit_and_past_them_do_not() {
    let values = |n: usize, width: usize| -> Vec<String> {
        (0..n).map(|i| format!("{i:0width$}")).collect()
    };
    let enum_of = |v: Vec<String>| with_property(json!({"type": "string", "enum": v}));
    assert!(fits(&enum_of(values(1_000, 4))));
    assert!(!fits(&enum_of(values(1_001, 4))));
    // More than 250 values: at most 15,000 characters in all.
    assert!(fits(&enum_of(values(300, 50))));
    assert!(!fits(&enum_of(values(300, 51))));
}

#[test]
fn nesting_past_ten_levels_does_not_fit() {
    let nested = |levels: usize| {
        let mut schema = json!({"type": "string"});
        for _ in 1..levels {
            schema = with_property(schema);
        }
        schema
    };
    assert!(fits(&nested(10)));
    assert!(!fits(&nested(11)));
}

#[test]
fn more_than_5000_properties_or_120000_characters_do_not_fit() {
    let object = |n: usize, width: usize| {
        let names: Vec<String> = (0..n).map(|i| format!("{i:0width$}")).collect();
        let properties: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .map(|n| (n.clone(), json!({"type": "string"})))
            .collect();
        json!({
            "type": "object",
            "properties": properties,
            "required": names,
            "additionalProperties": false
        })
    };
    assert!(fits(&object(5_000, 4)));
    assert!(!fits(&object(5_001, 4)));
    assert!(fits(&object(4_000, 30)));
    assert!(!fits(&object(4_000, 31)));
}

#[test]
fn the_large_enum_length_limit_starts_past_250_values() {
    let enum_of = |n: usize, width: usize| {
        let values: Vec<String> = (0..n).map(|i| format!("{i:0width$}")).collect();
        with_property(json!({"type": "string", "enum": values}))
    };
    // 250 values of 100 characters: 25,000 characters, no length limit yet.
    assert!(fits(&enum_of(250, 100)));
    assert!(!fits(&enum_of(251, 100)));
}

#[test]
fn enum_values_count_toward_the_120000_character_limit() {
    // Four enums of 250 values each: 1,000 values, no single large enum.
    let schema = |width: usize| {
        let values: Vec<String> = (0..250).map(|i| format!("{i:0width$}")).collect();
        let property = json!({"type": "string", "enum": values});
        json!({
            "type": "object",
            "properties": {"a": property, "b": property, "c": property, "d": property},
            "required": ["a", "b", "c", "d"],
            "additionalProperties": false
        })
    };
    assert!(fits(&schema(119)));
    assert!(!fits(&schema(121)));
}

#[test]
fn arrays_count_as_levels_of_nesting() {
    let nested = |levels: usize| {
        let mut schema = json!({"type": "string"});
        for _ in 2..levels {
            schema = json!({"type": "array", "items": schema});
        }
        with_property(schema)
    };
    assert!(fits(&nested(10)));
    assert!(!fits(&nested(11)));
}

#[test]
fn a_scalar_with_items_or_required_naming_an_unknown_property_does_not_fit() {
    assert!(!fits(&with_property(
        json!({"type": "string", "items": {"type": "string"}})
    )));
    assert!(!fits(&json!({
        "type": "object",
        "properties": {"a": {"type": "string"}},
        "required": ["b"],
        "additionalProperties": false
    })));
}
