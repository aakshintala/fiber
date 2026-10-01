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
        "all": "true",
        "edits": "[1, 2]",
        "mode": null
    }))
    .unwrap();
    assert_eq!(
        repaired,
        json!({"path": "/a", "limit": 5, "ratio": 0.5, "all": true, "edits": [1, 2]})
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
        // JSON that does not parse, and `anyOf`.
        json!({"path": "/a", "limit": "2.5"}),
        json!({"path": "/a", "all": "yes"}),
        json!({"path": "/a", "edits": "[\"x\"]"}),
        json!({"path": "/a", "edits": "[1,"}),
        json!({"path": "/a", "either": "3"}),
        // A string where the schema takes one.
        json!({"path": "5"}),
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
