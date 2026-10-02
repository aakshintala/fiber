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

#[test]
fn an_ambiguous_item_makes_its_branch_ambiguous() {
    let two = json!({"anyOf": [
        {"type": "object", "properties": {"a": {"type": "integer"}}},
        {"type": "object", "properties": {"b": {"type": "integer"}}}
    ]});
    let schema = json!({"anyOf": [
        {"type": "array", "items": two},
        {"type": "array", "items": {"type": "object", "properties": {"a": {"type": "integer"}}}}
    ]});
    let schema = json!({"type": "object", "properties": {"l": schema}});
    assert_eq!(repair(&schema, &json!({"l": [{"a": "1", "b": "2"}]})), None);
}

#[test]
fn a_branch_that_cannot_match_adds_no_ambiguity() {
    // Branch one's `x` has two readings, but branch one needs `missing`.
    let two = json!({"anyOf": [
        {"type": "object", "properties": {"a": {"type": "integer"}}},
        {"type": "object", "properties": {"b": {"type": "integer"}}}
    ]});
    let schema = json!({"anyOf": [
        {"type": "object", "required": ["missing"], "properties": {"x": two}},
        {"type": "object", "properties": {"y": {"type": "integer"}}}
    ]});
    let made = repair(&schema, &json!({"x": {"a": "1", "b": "2"}, "y": "3"})).unwrap();
    assert_eq!(made.repaired["y"], 3);
    assert_eq!(made.repaired["x"], json!({"a": "1", "b": "2"}));
}

#[test]
fn readings_from_an_impossible_branch_still_count() {
    // Branch one needs `missing`, but each of its readings of `x` passes
    // branch two, as does branch two's own: three readings, none repaired.
    let two = json!({"anyOf": [
        {"type": "object", "properties": {"a": {"type": "integer"}}},
        {"type": "object", "properties": {"b": {"type": "integer"}}}
    ]});
    let schema = json!({"anyOf": [
        {
            "type": "object",
            "required": ["missing"],
            "properties": {"x": two, "y": {"type": "integer"}}
        },
        {"type": "object", "properties": {"y": {"type": "integer"}}}
    ]});
    assert_eq!(
        repair(&schema, &json!({"x": {"a": "1", "b": "2"}, "y": "3"})),
        None
    );
}

#[test]
fn a_call_no_reading_passes_is_not_repaired() {
    // `limit` alone could be read, but `bogus` fails every reading.
    let arguments = json!({"path": "/a", "limit": "5", "bogus": 1});
    assert_eq!(fixes(&arguments), None);
    assert_eq!(
        check(&schema(), &arguments),
        [
            "`/bogus`: not allowed",
            "`/limit`: expected integer, got a string"
        ]
    );
}

#[test]
fn two_branches_yielding_one_reading_repair() {
    // Both branches read `x` as `{a: 1, b: "2"}`; branch one's other reading
    // fails both branches, so one reading passes.
    let two = json!({"anyOf": [
        {"type": "object", "properties": {"a": {"type": "integer"}}},
        {"type": "object", "properties": {"b": {"type": "integer"}}}
    ]});
    let x = json!({
        "type": "object",
        "properties": {"a": {"type": "integer"}, "b": {"type": "string"}}
    });
    let schema = json!({"anyOf": [
        {
            "type": "object",
            "required": ["missing"],
            "properties": {"x": two, "y": {"type": "integer"}}
        },
        {"type": "object", "properties": {"x": x, "y": {"type": "integer"}}}
    ]});
    let made = repair(&schema, &json!({"x": {"a": "1", "b": "2"}, "y": "3"})).unwrap();
    assert_eq!(
        Value::Object(made.repaired),
        json!({"x": {"a": 1, "b": "2"}, "y": 3})
    );
}

#[test]
fn a_reading_counts_whichever_branch_made_it() {
    // Branch one makes `y: 3` but needs `missing`; only branch two passes it.
    let schema = json!({"anyOf": [
        {"type": "object", "required": ["missing"], "properties": {"y": {"type": "integer"}}},
        {"type": "object", "properties": {"y": {"enum": [3]}}}
    ]});
    let made = repair(&schema, &json!({"y": "3"})).unwrap();
    assert_eq!(made.repaired["y"], 3);
}

#[test]
fn a_nested_any_of_with_one_reading_is_repaired() {
    let n = json!({"anyOf": [{"type": "integer"}, {"type": "boolean"}]});
    let schema = json!({
        "type": "object",
        "properties": {"x": {"anyOf": [
            {"type": "object", "properties": {"n": n}},
            {"type": "null"}
        ]}}
    });
    let made = repair(&schema, &json!({"x": {"n": "3"}})).unwrap();
    assert_eq!(made.repaired["x"], json!({"n": 3}));
    assert_eq!(made.repairs[0].path, "/x/n");
}

#[test]
fn each_item_is_read_on_its_own() {
    let list = |each: Value| json!({"type": "object", "properties": {"l": {"type": "array", "items": each}}});
    let one = list(json!({"anyOf": [{"type": "integer"}, {"type": "boolean"}]}));
    let made = repair(&one, &json!({"l": ["3", "true", 4]})).unwrap();
    assert_eq!(made.repaired["l"], json!([3, true, 4]));
    let two = list(json!({"anyOf": [{"type": "integer"}, {"type": "string"}]}));
    assert_eq!(repair(&two, &json!({"l": [1, "3"]})), None);
}

#[test]
fn json_is_parsed_only_into_an_object_or_array() {
    let schema = json!({"type": "object", "properties": {"n": {"type": ["array", "null"]}}});
    assert_eq!(repair(&schema, &json!({"n": "null"})), None);
    let made = repair(&schema, &json!({"n": "[1]"})).unwrap();
    assert_eq!(made.repaired["n"], json!([1]));
}
