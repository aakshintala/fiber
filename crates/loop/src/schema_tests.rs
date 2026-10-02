use contract::events::RepairFix;
use serde_json::{Value, json};

use super::{check, repair};

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "minLength": 1},
            "limit": {"type": "integer", "minimum": 1, "maximum": 100},
            "ratio": {"type": "number"},
            "all": {"type": "boolean"},
            "mode": {"type": "string", "enum": ["fast", "slow"]},
            "edits": {"type": "array", "items": {"type": "integer"}},
            "note": {"type": ["string", "null"]},
            "id": {"type": ["string", "integer"]},
            "either": {"anyOf": [{"type": "integer"}, {"type": "boolean"}]}
        },
        "required": ["path"],
        "additionalProperties": false
    })
}

fn fixes(arguments: &Value) -> Option<(Value, Vec<(String, RepairFix)>)> {
    repair(&schema(), arguments).map(|r| {
        (
            Value::Object(r.repaired),
            r.repairs.into_iter().map(|r| (r.path, r.fix)).collect(),
        )
    })
}

#[test]
fn arguments_that_pass_need_no_repair() {
    let good = json!({"path": "/a", "limit": 5, "all": true, "edits": [1], "note": null});
    assert!(check(&schema(), &good).is_empty());
    assert_eq!(fixes(&good), None);
}

#[test]
fn a_repair_inside_an_array_names_its_item() {
    let (repaired, made) = fixes(&json!({"path": "/a", "edits": [1, "2"]})).unwrap();
    assert_eq!(repaired["edits"], json!([1, 2]));
    assert_eq!(made, [("/edits/1".to_owned(), RepairFix::StringToNumber)]);
}

#[test]
fn the_check_gives_one_line_per_bad_field() {
    let errors = check(
        &schema(),
        &json!({
            "limit": 0,
            "ratio": "x",
            "mode": "medium",
            "edits": [1, "two"],
            "path/x": 1,
            "either": "s"
        }),
    );
    assert_eq!(
        errors,
        [
            "`/path`: missing",
            "`/edits/1`: expected integer, got a string",
            "`/either`: matches none of the shapes allowed",
            "`/limit`: must be at least 1",
            "`/mode`: must be one of \"fast\", \"slow\"",
            "`/path~1x`: not allowed",
            "`/ratio`: expected number, got a string",
        ]
    );
    assert_eq!(
        check(&schema(), &json!({"path": "", "limit": 101})),
        [
            "`/limit`: must be at most 100",
            "`/path`: must be at least 1 characters",
        ]
    );
    assert_eq!(
        check(&schema(), &json!("raw")),
        ["arguments: expected object, got a string"]
    );
}

#[test]
fn keywords_outside_the_subset_are_skipped() {
    let schema = json!({"type": "object", "properties": {"x": {"type": "uuid", "format": "z"}}});
    assert!(check(&schema, &json!({"x": 1, "y": 2})).is_empty());
}

#[test]
fn bounds_are_inclusive_and_each_type_is_checked() {
    let at_bounds = json!({"path": "a", "limit": 1, "id": -3});
    assert!(check(&schema(), &at_bounds).is_empty());
    assert!(check(&schema(), &json!({"path": "a", "limit": 100})).is_empty());
    assert_eq!(
        check(
            &schema(),
            &json!({"path": 5, "edits": 5, "note": 1, "limit": -1, "all": "true"})
        ),
        [
            "`/all`: expected boolean, got a string",
            "`/edits`: expected array, got a number",
            "`/limit`: must be at least 1",
            "`/note`: expected string or null, got a number",
            "`/path`: expected string, got a number",
        ]
    );
}

#[test]
fn an_any_of_does_not_hide_the_keywords_beside_it() {
    let schema = json!({
        "type": "object",
        "required": ["city"],
        "properties": {"city": {"type": "string"}},
        "anyOf": [{"type": "object"}]
    });
    assert_eq!(check(&schema, &json!({})), ["`/city`: missing"]);
    assert_eq!(
        check(&schema, &json!({"city": 1})),
        ["`/city`: expected string, got a number"]
    );
    assert_eq!(
        check(&schema, &json!("x")),
        [
            "arguments: matches none of the shapes allowed",
            "arguments: expected object, got a string"
        ]
    );
}

#[test]
fn each_repair_a_single_type_allows() {
    let (repaired, mut made) = fixes(&json!({
        "path": "/a",
        "limit": "5",
        "ratio": "0.5",
        "all": "false",
        "edits": "[1, \"2\"]",
        "mode": null
    }))
    .unwrap();
    assert_eq!(
        repaired,
        json!({"path": "/a", "limit": 5, "ratio": 0.5, "all": false, "edits": [1, 2]})
    );
    made.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        made,
        [
            ("/all".to_owned(), RepairFix::StringToBoolean),
            ("/edits".to_owned(), RepairFix::StringParsed),
            ("/edits/1".to_owned(), RepairFix::StringToNumber),
            ("/limit".to_owned(), RepairFix::StringToNumber),
            ("/mode".to_owned(), RepairFix::NullDropped),
            ("/ratio".to_owned(), RepairFix::StringToNumber),
        ]
    );
}

fn object(properties: Value) -> Value {
    json!({"type": "object", "properties": properties})
}

#[test]
fn an_any_of_is_never_repaired() {
    assert_eq!(fixes(&json!({"path": "/a", "either": "3"})), None);
    let branch = object(json!({"n": {"type": "integer"}}));
    let inside = object(json!({"x": {"anyOf": [branch]}}));
    assert_eq!(repair(&inside, &json!({"x": {"n": "3"}})), None);
    let beside = object(json!({"n": {"type": "integer", "anyOf": [{}]}}));
    assert_eq!(repair(&beside, &json!({"n": "3"})), None);
    let optional = object(json!({"n": {"type": "integer", "anyOf": [{}]}}));
    assert_eq!(repair(&optional, &json!({"n": null})), None);
    let around = object(json!({"x": {"anyOf": [{}], "properties": {"n": {"type": "integer"}}}}));
    assert_eq!(repair(&around, &json!({"x": {"n": "3"}})), None);
}

#[test]
fn a_one_of_is_never_repaired() {
    let alone = object(json!({"n": {"oneOf": [{"type": "integer"}]}}));
    assert_eq!(repair(&alone, &json!({"n": "3"})), None);
    let beside = object(json!({"n": {"type": "integer", "oneOf": [{}]}}));
    assert_eq!(repair(&beside, &json!({"n": "3"})), None);
    assert_eq!(repair(&beside, &json!({"n": null})), None);
    let around = object(json!({"x": {"oneOf": [{}], "items": {"type": "integer"}}}));
    assert_eq!(repair(&around, &json!({"x": ["3"]})), None);
}

#[test]
fn integer_or_null_is_not_a_single_type() {
    let schema = object(json!({"n": {"type": ["integer", "null"]}}));
    assert_eq!(repair(&schema, &json!({"n": null})), None);
    assert_eq!(repair(&schema, &json!({"n": "5"})), None);
    let one = object(json!({"n": {"type": ["integer"]}}));
    assert_eq!(repair(&one, &json!({"n": "5"})).unwrap().repaired["n"], 5);
}

#[test]
fn a_null_is_dropped_only_where_optional() {
    assert_eq!(fixes(&json!({"path": null})), None);
    let (repaired, made) = fixes(&json!({"path": "/a", "mode": null})).unwrap();
    assert_eq!(repaired, json!({"path": "/a"}));
    assert_eq!(made, [("/mode".to_owned(), RepairFix::NullDropped)]);
    // A schema with no type, and one that is only `null`.
    let untyped = object(json!({"n": {}, "z": {"type": "null"}}));
    assert_eq!(repair(&untyped, &json!({"n": null, "z": null})), None);
}

#[test]
fn only_a_plain_json_number_is_read() {
    let schema = object(json!({"i": {"type": "integer"}, "f": {"type": "number"}}));
    let read = |key: &str, text: &str| {
        repair(&schema, &json!({key: text})).map(|made| made.repaired[key].clone())
    };
    assert_eq!(read("f", "5"), Some(json!(5)));
    assert_eq!(read("f", "-2.5"), Some(json!(-2.5)));
    assert_eq!(read("f", "1e3"), Some(json!(1000.0)));
    assert_eq!(read("i", "5"), Some(json!(5)));
    assert_eq!(read("i", "5.0"), Some(json!(5)));
    assert_eq!(read("i", "1e3"), Some(json!(1000)));
    assert_eq!(read("i", "-7"), Some(json!(-7)));
    assert_eq!(read("i", "18446744073709551615"), Some(json!(u64::MAX)));
    for text in [
        "5.5", "1e20", " 5", "5 ", "0x10", "NaN", "+5", "05", "", "five",
    ] {
        assert_eq!(read("i", text), None, "{text:?}");
    }
    for text in [" 5", "0x10", "NaN", "Infinity", "+5", "1.", ".5"] {
        assert_eq!(read("f", text), None, "{text:?}");
    }
}

#[test]
fn only_true_and_false_are_booleans() {
    let schema = object(json!({"b": {"type": "boolean"}}));
    let read = |text: &str| repair(&schema, &json!({"b": text})).map(|m| m.repaired["b"].clone());
    assert_eq!(read("true"), Some(json!(true)));
    assert_eq!(read("false"), Some(json!(false)));
    for text in ["True", " true", "1", "yes"] {
        assert_eq!(read(text), None, "{text:?}");
    }
}

#[test]
fn json_in_a_string_is_parsed_and_repaired_inside() {
    let inner = json!({
        "type": "object",
        "properties": {"m": {"type": "integer"}},
        "additionalProperties": false
    });
    let schema = object(json!({
        "o": {
            "type": "object",
            "properties": {"n": {"type": "integer"}, "inner": inner},
            "additionalProperties": false
        },
        "l": {"type": "array"}
    }));
    let sent = json!({"o": r#"{"n": "3", "inner": "{\"m\": \"4\"}"}"#});
    let made = repair(&schema, &sent).unwrap();
    assert_eq!(made.repaired["o"], json!({"n": 3, "inner": {"m": 4}}));
    let mut paths: Vec<_> = made.repairs.iter().map(|r| r.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["/o", "/o/inner", "/o/inner/m", "/o/n"]);
    // It must then pass, and JSON holding a scalar is not parsed.
    assert_eq!(repair(&schema, &json!({"o": r#"{"bad": 1}"#})), None);
    assert_eq!(repair(&schema, &json!({"o": "[1]"})), None);
    assert_eq!(repair(&schema, &json!({"l": "null"})), None);
    assert_eq!(repair(&schema, &json!({"l": "5"})), None);
    assert_eq!(repair(&schema, &json!({"l": "[1"})), None);
}

#[test]
fn a_long_array_is_repaired_in_one_pass() {
    let list = |each: Value| object(json!({"l": {"type": "array", "items": each}}));
    let sent: Vec<Value> = (0..30).map(|i| json!(i.to_string())).collect();
    let made = repair(&list(json!({"type": "integer"})), &json!({"l": sent})).unwrap();
    assert_eq!(made.repaired["l"], json!((0..30).collect::<Vec<_>>()));
    assert_eq!(made.repairs.len(), 30);
    let two = json!({"anyOf": [
        object(json!({"a": {"type": "integer"}})),
        object(json!({"b": {"type": "integer"}}))
    ]});
    let sent = vec![json!({"a": "1", "b": "2"}); 30];
    assert_eq!(repair(&list(two), &json!({"l": sent})), None);
}

#[test]
fn a_ref_is_followed() {
    let schema = json!({
        "type": "object",
        "$defs": {
            "count": {"type": "integer"},
            "node": {
                "type": "object",
                "properties": {"v": {"type": "integer"}, "next": {"$ref": "#/$defs/node"}}
            }
        },
        "properties": {"n": {"$ref": "#/$defs/count"}, "list": {"$ref": "#/$defs/node"}}
    });
    let sent = json!({"n": "3", "list": {"v": "1", "next": {"v": "2", "next": null}}});
    let made = repair(&schema, &sent).unwrap();
    assert_eq!(
        Value::Object(made.repaired),
        json!({"n": 3, "list": {"v": 1, "next": {"v": 2}}})
    );
}

#[test]
fn a_cyclic_ref_repairs_nothing() {
    let schema = json!({
        "type": "object",
        "$defs": {"a": {"$ref": "#/$defs/b"}, "b": {"$ref": "#/$defs/a"}},
        "properties": {
            "n": {"$ref": "#/$defs/a"},
            "m": {"$ref": "#/properties/m"},
            "far": {"$ref": "https://example.com/schema"}
        }
    });
    assert_eq!(
        repair(&schema, &json!({"n": "3", "m": "4", "far": "5"})),
        None
    );
    assert_eq!(repair(&schema, &json!({"n": null, "m": null})), None);
}

#[test]
fn a_ref_is_checked() {
    let schema = json!({
        "type": "object",
        "$defs": {"count": {"type": "integer", "minimum": 1}},
        "definitions": {"word": {"type": "string"}},
        "properties": {
            "n": {"$ref": "#/$defs/count"},
            "w": {"$ref": "#/definitions/word"},
            "l": {"type": "array", "items": {"$ref": "#/$defs/count"}}
        }
    });
    assert!(check(&schema, &json!({"n": 2, "w": "a", "l": [1]})).is_empty());
    assert_eq!(
        check(&schema, &json!({"n": 0, "w": 1, "l": [1, 0]})),
        [
            "`/l/1`: must be at least 1",
            "`/n`: must be at least 1",
            "`/w`: expected string, got a number"
        ]
    );
}

#[test]
fn a_ref_inside_parsed_json_is_checked() {
    let schema = json!({
        "type": "object",
        "$defs": {"count": {"type": "integer", "minimum": 1}},
        "properties": {
            "o": {"type": "object", "properties": {"k": {"$ref": "#/$defs/count"}}}
        }
    });
    assert_eq!(repair(&schema, &json!({"o": r#"{"k": 0}"#})), None);
    let made = repair(&schema, &json!({"o": r#"{"k": "2"}"#})).unwrap();
    assert_eq!(made.repaired["o"], json!({"k": 2}));
}

#[test]
fn a_ref_that_cannot_be_followed_fails_the_check() {
    let schema = json!({
        "type": "object",
        "$defs": {"a": {"$ref": "#/$defs/b"}, "b": {"$ref": "#/$defs/a"}},
        "properties": {
            "far": {"$ref": "https://example.com/schema"},
            "gone": {"$ref": "#/$defs/missing"},
            "loop": {"$ref": "#/$defs/a"}
        }
    });
    assert_eq!(
        check(&schema, &json!({"far": 1, "gone": 1, "loop": 1})),
        [
            "`/far`: its schema's `$ref` to `https://example.com/schema` cannot be followed",
            "`/gone`: its schema's `$ref` to `#/$defs/missing` cannot be followed",
            "`/loop`: its schema's `$ref` to `#/$defs/a` leads back to itself"
        ]
    );
}
