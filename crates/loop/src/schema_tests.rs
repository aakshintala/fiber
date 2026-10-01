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
fn each_repair_the_schema_allows_one_reading_of() {
    let (repaired, made) = fixes(&json!({
        "path": "/a",
        "limit": " 5 ",
        "ratio": "0.5",
        "all": "false",
        "edits": "[1, 2]",
        "mode": null
    }))
    .unwrap();
    assert_eq!(
        repaired,
        json!({"path": "/a", "limit": 5, "ratio": 0.5, "all": false, "edits": [1, 2]})
    );
    assert!(check(&schema(), &repaired).is_empty());
    let mut made = made;
    made.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        made,
        [
            ("/all".to_owned(), RepairFix::StringToBoolean),
            ("/edits".to_owned(), RepairFix::StringParsed),
            ("/limit".to_owned(), RepairFix::StringToNumber),
            ("/mode".to_owned(), RepairFix::NullDropped),
            ("/ratio".to_owned(), RepairFix::StringToNumber),
        ]
    );
}

#[test]
fn nothing_with_two_readings_or_none_is_repaired() {
    for arguments in [
        // A required property's null, and a null the schema allows.
        json!({"path": null}),
        json!({"path": "/a", "note": null}),
        // Not an integer, not a boolean, JSON that does not pass the check,
        // JSON that does not parse, and `anyOf` with no reading that passes.
        json!({"path": "/a", "limit": "2.5"}),
        json!({"path": "/a", "all": "yes"}),
        json!({"path": "/a", "edits": "[\"x\"]"}),
        json!({"path": "/a", "edits": "[1,"}),
        json!({"path": "/a", "either": "x"}),
        // A string where the schema takes one, alone or beside another type.
        json!({"path": "5"}),
        json!({"path": "/a", "id": "5"}),
    ] {
        assert_eq!(fixes(&arguments), None, "{arguments}");
    }
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
fn the_string_true_becomes_a_boolean() {
    let made = repair(&schema(), &json!({"path": "a", "all": "true"})).unwrap();
    assert_eq!(made.repaired["all"], true);
}

#[test]
fn an_any_of_with_exactly_one_reading_is_repaired() {
    let made = repair(&schema(), &json!({"path": "a", "either": "3"})).unwrap();
    assert_eq!(made.repaired["either"], 3);
    assert_eq!(made.repairs.len(), 1);
    assert_eq!(made.repairs[0].path, "/either");
    assert_eq!(made.repairs[0].fix, RepairFix::StringToNumber);
    let made = repair(&schema(), &json!({"path": "a", "either": "false"})).unwrap();
    assert_eq!(made.repaired["either"], false);
    // Two branches reading the same value are one reading.
    let same = json!({"anyOf": [{"type": "integer"}, {"type": "number"}]});
    assert!(
        repair(
            &json!({"type": "object", "properties": {"n": same}}),
            &json!({"n": "3"})
        )
        .is_some()
    );
}

#[test]
fn an_any_of_with_two_readings_is_not_repaired() {
    let two = json!({"anyOf": [
        {"type": "object", "properties": {"a": {"type": "integer"}}},
        {"type": "object", "properties": {"b": {"type": "integer"}}}
    ]});
    assert_eq!(repair(&two, &json!({"a": "1", "b": "2"})), None);
    // Arguments that already pass need no reading.
    assert_eq!(repair(&two, &json!({"a": "1"})), None);
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
fn the_keywords_beside_an_any_of_are_repaired_too() {
    let schema = json!({
        "type": "object",
        "properties": {"n": {"type": "integer", "anyOf": [{}]}}
    });
    let made = repair(&schema, &json!({"n": "3"})).unwrap();
    assert_eq!(made.repaired["n"], 3);
    assert_eq!(made.repairs[0].path, "/n");
}

#[test]
fn an_ambiguous_branch_makes_the_whole_any_of_ambiguous() {
    // Branch one's `x` has two readings; branch two alone could repair `y`.
    let two = json!({"anyOf": [
        {"type": "object", "properties": {"a": {"type": "integer"}}},
        {"type": "object", "properties": {"b": {"type": "integer"}}}
    ]});
    let schema = json!({"anyOf": [
        {"type": "object", "properties": {"x": two}},
        {"type": "object", "properties": {"y": {"type": "integer"}}}
    ]});
    let arguments = json!({"x": {"a": "1", "b": "2"}, "y": "3"});
    assert_eq!(repair(&schema, &arguments), None);
    // Without the ambiguity, branch two's reading is the one.
    let made = repair(&schema, &json!({"x": {"a": 1}, "y": "3"}));
    assert!(made.is_none(), "already passes through branch one");
    let made = repair(&schema, &json!({"x": 5, "y": "3"})).unwrap();
    assert_eq!(made.repaired["y"], 3);
}

#[test]
fn a_null_an_any_of_allows_is_kept() {
    let schema = json!({
        "type": "object",
        "properties": {
            "n": {"anyOf": [{"type": "integer"}, {"type": "null"}]},
            "m": {"anyOf": [{"type": "integer"}, {"type": "string"}]}
        }
    });
    let made = repair(&schema, &json!({"n": null, "m": null})).unwrap();
    assert_eq!(made.repaired, *json!({"n": null}).as_object().unwrap());
}
