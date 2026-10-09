//! What a tool's `run` returns, read as a result (`docs/extensions.md`,
//! "Registering").

#![allow(clippy::unwrap_used, reason = "test code")]

use contract::events::Control;
use serde_json::json;

use super::*;

fn text(text: &str) -> ContentPart {
    ContentPart::Text {
        text: text.to_owned(),
    }
}

#[test]
fn a_string_is_one_text_part() {
    assert_eq!(
        output_from(json!("3 notes")).unwrap(),
        text_output(vec![text("3 notes")])
    );
}

#[test]
fn one_text_part_on_its_own_is_the_content() {
    assert_eq!(
        output_from(json!({"type": "text", "text": "hello"})).unwrap(),
        text_output(vec![text("hello")])
    );
}

#[test]
fn a_full_table_maps_field_for_field() {
    let output = output_from(json!({
        "content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
        "details": {"count": 3, "nested": {"list": [1, 2]}},
        "error": {"code": "nonzero_exit", "message": "m"},
        "control": {"handoff": "n"},
    }))
    .unwrap();
    assert_eq!(
        output,
        Output {
            content: vec![text("a"), text("b")],
            error: Some(failure(ErrorCode::NonzeroExit, "m".to_owned())),
            details: Some(json!({"count": 3, "nested": {"list": [1, 2]}})),
            control: Some(Control {
                handoff: Some("n".to_owned()),
                questions: None,
                skill: None,
            }),
            ..Output::default()
        }
    );
}

#[test]
fn content_may_be_a_string_or_an_empty_list() {
    assert_eq!(
        output_from(json!({"content": "x"})).unwrap(),
        text_output(vec![text("x")])
    );
    assert_eq!(
        output_from(json!({"content": []})).unwrap(),
        Output::default()
    );
}

#[test]
fn an_error_code_this_build_does_not_know_is_kept() {
    let output =
        output_from(json!({"content": "", "error": {"code": "weird", "message": "m"}})).unwrap();
    assert_eq!(
        output.error,
        Some(failure(
            ErrorCode::Other("weird".to_owned()),
            "m".to_owned()
        ))
    );
}

#[test]
fn each_return_that_does_not_read_says_why() {
    let error = "an `error` that is not `{ code, message }` of two strings";
    let image = "a `image` part; a tool returns only text parts";
    let cases = [
        (json!(null), "nothing"),
        (json!(3), "a number"),
        (json!(true), "a boolean"),
        (json!(["a"]), "a list with no `content`"),
        (
            json!({"content": "x", "colour": "red"}),
            "`colour`, which is not `content`, `details`, `error` or `control`",
        ),
        (json!({"details": 1}), "no `content`"),
        (
            json!({"content": 3}),
            "a `content` that is not a string or a list of parts",
        ),
        (json!({"content": ["a"]}), "\"a\" where a part belongs"),
        (
            json!({"content": [{"type": "image", "path": "a.png"}]}),
            image,
        ),
        (json!({"type": "image", "path": "a.png"}), image),
        (json!({"content": [{"text": "a"}]}), "a part with no `type`"),
        (
            json!({"type": "text", "text": "x", "extra": 1}),
            "a text part holding `extra`",
        ),
        (
            json!({"type": "text", "text": 1}),
            "a text part whose `text` is not a string",
        ),
        (json!({"content": "", "error": "boom"}), error),
        (json!({"content": "", "error": {"code": "x"}}), error),
        (
            json!({"content": "", "error": {"code": "x", "message": "m", "at": 1}}),
            error,
        ),
        (
            json!({"content": "", "error": {"code": 1, "message": "m"}}),
            error,
        ),
    ];
    for (value, why) in cases {
        assert_eq!(output_from(value.clone()), Err(why.to_owned()), "{value}");
    }
    let Err(why) = output_from(json!({"content": "", "control": {"handoff": 1}})) else {
        panic!("a bad control read");
    };
    assert!(why.starts_with("a `control` that does not read: "), "{why}");
}
