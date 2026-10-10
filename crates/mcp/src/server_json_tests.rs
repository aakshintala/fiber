//! Lenient reads, dropped entries, kept unknown fields and content parsing.

use serde_json::{Map, Value, json};

use super::{Argument, CallResult, Content, ListedPrompt, ListedTool, entries};

fn tool(value: Value) -> ListedTool {
    serde_json::from_value(value).expect("a tool reads")
}

fn prompt(value: Value) -> ListedPrompt {
    serde_json::from_value(value).expect("a prompt reads")
}

#[test]
fn lenient_fields_read_missing_null_wrong_and_right() {
    for (value, name, description, schema, read_only, destructive, open_world) in [
        (
            json!({}),
            "",
            "",
            json!({"type": "object"}),
            None,
            None,
            None,
        ),
        (
            json!({
                "name": null,
                "description": null,
                "inputSchema": null,
                "annotations": null,
            }),
            "",
            "",
            Value::Null,
            None,
            None,
            None,
        ),
        (
            json!({
                "name": 7,
                "description": 7,
                "inputSchema": 7,
                "annotations": 7,
            }),
            "",
            "",
            json!(7),
            None,
            None,
            None,
        ),
        (
            json!({
                "name": "echo",
                "description": "Echoes.",
                "inputSchema": {"type": "object"},
                "annotations": {
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "openWorldHint": true,
                },
            }),
            "echo",
            "Echoes.",
            json!({"type": "object"}),
            Some(true),
            Some(false),
            Some(true),
        ),
    ] {
        let read = tool(value);
        assert_eq!(read.name, name);
        assert_eq!(read.description, description);
        assert_eq!(read.schema, schema);
        assert_eq!(read.annotations.read_only, read_only);
        assert_eq!(read.annotations.destructive, destructive);
        assert_eq!(read.annotations.open_world, open_world);
    }
    for (value, name, description, required) in [
        (json!({}), "", "", false),
        (
            json!({"name": null, "description": null, "arguments": null}),
            "",
            "",
            false,
        ),
        (
            json!({"name": 7, "description": 7, "arguments": 7}),
            "",
            "",
            false,
        ),
        (
            json!({
                "name": "greet",
                "description": "Greets.",
                "arguments": [{"name": "who", "required": true}],
            }),
            "greet",
            "Greets.",
            true,
        ),
    ] {
        let read = prompt(value);
        assert_eq!(read.name, name);
        assert_eq!(read.description, description);
        if required {
            assert_eq!(read.arguments.len(), 1);
            assert_eq!(read.arguments[0].name, "who");
            assert!(read.arguments[0].required);
        } else {
            assert!(read.arguments.is_empty());
        }
    }
    for (value, required) in [
        (json!({"name": "who"}), false),
        (json!({"name": "who", "required": null}), false),
        (json!({"name": "who", "required": 7}), false),
        (json!({"name": "who", "required": true}), true),
    ] {
        let argument: Argument = serde_json::from_value(value).expect("an argument reads");
        assert_eq!(argument.required, required);
    }
}

#[test]
fn a_nameless_or_empty_named_argument_is_dropped() {
    let read = prompt(json!({
        "name": "greet",
        "arguments": [{"required": true}, {"name": "", "required": true}, {"name": "who"}],
    }));
    assert_eq!(read.arguments.len(), 1);
    assert_eq!(read.arguments[0].name, "who");
}

#[test]
fn a_non_object_entry_is_dropped() {
    let tools: Vec<ListedTool> = entries(vec![json!({"name": "echo"}), json!(7), json!(null)]);
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "echo");
    let prompts: Vec<ListedPrompt> =
        entries(vec![json!({"name": "greet"}), json!("nope")]);
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0].name, "greet");
}

#[test]
fn unknown_fields_survive_a_round_trip() {
    let read = tool(json!({
        "name": "echo",
        "annotations": {"readOnlyHint": true, "idempotentHint": true},
        "extra": 7,
    }));
    assert_eq!(
        read.annotations.rest.get("idempotentHint"),
        Some(&Value::from(true))
    );
    assert_eq!(read.rest.get("extra"), Some(&Value::from(7)));
    let again: ListedTool =
        serde_json::from_value(serde_json::to_value(&read).expect("serializes"))
            .expect("round trips");
    assert_eq!(again, read);
    let read = prompt(json!({"name": "greet", "extra": "kept"}));
    assert_eq!(
        read.rest.get("extra"),
        Some(&Value::from("kept"))
    );
    let again: ListedPrompt =
        serde_json::from_value(serde_json::to_value(&read).expect("serializes"))
            .expect("round trips");
    assert_eq!(again, read);
}

#[test]
fn content_parses_each_shape() {
    let result: CallResult = serde_json::from_value(json!({
        "content": [
            {"type": "text", "text": "hi"},
            {"type": "text"},
            {"type": "image"},
            {"type": "audio"},
            {"type": "resource", "resource": {"text": "from a file"}},
            {"type": "resource", "resource": {"blob": "aGk="}},
            {"type": "resource", "resource": {}},
            {"type": "resource"},
            {"type": "resource", "resource": null},
            {"type": "resource", "resource": 5},
            {"type": "resource_link"},
            {"type": "bogus"},
            {"no": "type"},
            7,
        ],
    }))
    .expect("content reads");
    assert_eq!(
        result.content,
        vec![
            Content::Text {
                text: Some("hi".to_owned())
            },
            Content::Text { text: None },
            Content::Image,
            Content::Audio,
            Content::Resource {
                resource: super::Resource {
                    text: Some("from a file".to_owned()),
                    blob: None,
                }
            },
            Content::Resource {
                resource: super::Resource {
                    text: None,
                    blob: Some(Value::from("aGk=")),
                }
            },
            Content::Resource {
                resource: super::Resource {
                    text: None,
                    blob: None,
                }
            },
            Content::Resource {
                resource: super::Resource {
                    text: None,
                    blob: None,
                }
            },
            Content::Resource {
                resource: super::Resource {
                    text: None,
                    blob: None,
                }
            },
            Content::Resource {
                resource: super::Resource {
                    text: None,
                    blob: None,
                }
            },
            Content::ResourceLink,
            Content::Other,
            Content::Unreadable,
            Content::Unreadable,
        ]
    );
    let empty: CallResult = serde_json::from_value(json!(7)).expect("a non-object is default");
    assert_eq!(empty, CallResult::default());
    let _: Map<String, Value> = Map::new();
}
