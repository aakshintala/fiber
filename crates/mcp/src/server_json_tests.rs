//! Lenient reads, dropped entries, kept unknown fields and content parsing.

use serde_json::{Map, Value, json};

use super::{
    Argument, CallResult, Content, ListedPrompt, ListedTool, Message, Resource, entries,
    from_object,
};

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
    let prompts: Vec<ListedPrompt> = entries(vec![json!({"name": "greet"}), json!("nope")]);
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
    assert_eq!(read.rest.get("extra"), Some(&Value::from("kept")));
    let again: ListedPrompt =
        serde_json::from_value(serde_json::to_value(&read).expect("serializes"))
            .expect("round trips");
    assert_eq!(again, read);
}

fn audio(data: Option<Value>, mime_type: Option<&str>) -> Content {
    Content::Audio {
        data,
        mime_type: mime_type.map(str::to_owned),
    }
}

fn image(data: Option<Value>, mime_type: Option<&str>) -> Content {
    Content::Image {
        data,
        mime_type: mime_type.map(str::to_owned),
    }
}

fn resource_link(uri: Option<&str>, name: Option<&str>, mime_type: Option<&str>) -> Content {
    Content::ResourceLink {
        uri: uri.map(str::to_owned),
        name: name.map(str::to_owned),
        mime_type: mime_type.map(str::to_owned),
    }
}

fn resource(
    text: Option<&str>,
    blob: Option<Value>,
    uri: Option<&str>,
    mime_type: Option<&str>,
) -> Content {
    Content::Resource {
        resource: Resource {
            text: text.map(str::to_owned),
            blob,
            uri: uri.map(str::to_owned),
            mime_type: mime_type.map(str::to_owned),
        },
    }
}

#[test]
fn content_parses_each_shape() {
    let result: CallResult = serde_json::from_value(json!({
        "content": [
            {"type": "text", "text": "hi"},
            {"type": "text"},
            {"type": "image"},
            {"type": "image", "data": "aGk=", "mimeType": "image/png"},
            {"type": "audio"},
            {"type": "audio", "data": "aGk=", "mimeType": "audio/mpeg"},
            {"type": "resource", "resource": {"text": "from a file"}},
            {"type": "resource", "resource": {"blob": "aGk="}},
            {"type": "resource", "resource": {"text": "t", "uri": "file:///x", "mimeType": "text/plain"}},
            {"type": "resource", "resource": {}},
            {"type": "resource"},
            {"type": "resource", "resource": null},
            {"type": "resource", "resource": 5},
            {"type": "resource", "resource": ["hello", null]},
            {"type": "resource_link"},
            {"type": "resource_link", "uri": "file:///x", "name": "x", "mimeType": "text/plain"},
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
            image(None, None),
            image(Some(Value::from("aGk=")), Some("image/png")),
            audio(None, None),
            audio(Some(Value::from("aGk=")), Some("audio/mpeg")),
            resource(Some("from a file"), None, None, None),
            resource(None, Some(Value::from("aGk=")), None, None),
            resource(Some("t"), None, Some("file:///x"), Some("text/plain")),
            resource(None, None, None, None),
            resource(None, None, None, None),
            resource(None, None, None, None),
            resource(None, None, None, None),
            resource(None, None, None, None),
            resource_link(None, None, None),
            resource_link(Some("file:///x"), Some("x"), Some("text/plain")),
            Content::Other,
            Content::Unreadable,
            Content::Unreadable,
        ]
    );
    let empty: CallResult = from_object(json!(7)).unwrap_or_default();
    assert_eq!(empty, CallResult::default());
    let _: Map<String, Value> = Map::new();
}

#[test]
fn new_content_fields_read_present_absent_and_wrong() {
    for (name, value, expected) in [
        ("audio absent", json!({"type": "audio"}), audio(None, None)),
        (
            "audio null",
            json!({"type": "audio", "data": null, "mimeType": null}),
            audio(None, None),
        ),
        (
            "audio present",
            json!({"type": "audio", "data": "aGk=", "mimeType": "audio/mpeg"}),
            audio(Some(Value::from("aGk=")), Some("audio/mpeg")),
        ),
        (
            "audio array data and numeric type read as absent",
            json!({"type": "audio", "data": ["aGk="], "mimeType": 7}),
            audio(None, None),
        ),
        (
            "audio numeric data stays a value",
            json!({"type": "audio", "data": 7}),
            audio(Some(Value::from(7)), None),
        ),
        (
            "link absent",
            json!({"type": "resource_link"}),
            resource_link(None, None, None),
        ),
        (
            "link null",
            json!({"type": "resource_link", "uri": null, "name": null, "mimeType": null}),
            resource_link(None, None, None),
        ),
        (
            "link present",
            json!({"type": "resource_link", "uri": "u", "name": "n", "mimeType": "m"}),
            resource_link(Some("u"), Some("n"), Some("m")),
        ),
        (
            "link wrong types read as absent",
            json!({"type": "resource_link", "uri": 7, "name": 7, "mimeType": 7}),
            resource_link(None, None, None),
        ),
        (
            "resource uri and type absent",
            json!({"type": "resource", "resource": {"text": "t"}}),
            resource(Some("t"), None, None, None),
        ),
        (
            "resource uri and type present",
            json!({"type": "resource", "resource": {"uri": "u", "mimeType": "m"}}),
            resource(None, None, Some("u"), Some("m")),
        ),
        (
            "resource uri and type wrong read as absent",
            json!({"type": "resource", "resource": {"uri": 7, "mimeType": 7}}),
            resource(None, None, None, None),
        ),
        ("image absent", json!({"type": "image"}), image(None, None)),
        (
            "image null",
            json!({"type": "image", "data": null, "mimeType": null}),
            image(None, None),
        ),
        (
            "image present",
            json!({"type": "image", "data": "aGk=", "mimeType": "image/png"}),
            image(Some(Value::from("aGk=")), Some("image/png")),
        ),
        (
            "image array data and numeric type read as absent",
            json!({"type": "image", "data": ["aGk="], "mimeType": 7}),
            image(None, None),
        ),
        (
            "image numeric data stays a value",
            json!({"type": "image", "data": 7}),
            image(Some(Value::from(7)), None),
        ),
    ] {
        let result: CallResult =
            serde_json::from_value(json!({"content": [value]})).expect("content reads");
        assert_eq!(result.content, vec![expected], "row: {name}");
    }
}

#[test]
fn array_shapes_never_read_positionally() {
    let tools: Vec<ListedTool> = entries(vec![json!([
        "echo",
        "Echoes.",
        {"type": "object"},
        {},
        {},
    ])]);
    assert!(tools.is_empty());
    let prompts: Vec<ListedPrompt> = entries(vec![json!(["greet", "Greets.", [], {}])]);
    assert!(prompts.is_empty());
    let read = prompt(json!({"name": "greet", "arguments": [["who", true, {}]]}));
    assert!(read.arguments.is_empty());
    let read = tool(json!({"name": "echo", "annotations": [true]}));
    assert_eq!(read.annotations, super::Annotations::default());
    let message: Message =
        serde_json::from_value(json!(["hello"])).expect("an array message reads");
    assert!(message.content.is_empty());
    assert!(from_object::<super::PromptResult>(json!([[{"messages": []}]])).is_err());
    let result: CallResult =
        from_object(json!([[{"type": "text", "text": "hi"}], true])).unwrap_or_default();
    assert_eq!(result, CallResult::default());
}
