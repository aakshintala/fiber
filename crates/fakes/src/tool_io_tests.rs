use contract::shapes::ContentPart;
use serde_json::json;

use super::{text_of, tool_args};

#[test]
fn an_object_in_is_the_same_map_out() {
    let map = tool_args(json!({"path": "a", "n": 1}));
    assert_eq!(
        map,
        json!({"path": "a", "n": 1}).as_object().unwrap().clone()
    );
}

#[test]
fn a_non_object_fails_naming_the_object() {
    for value in [json!(null), json!([1]), json!("x"), json!(1), json!(true)] {
        let failed = std::panic::catch_unwind(|| tool_args(value)).expect_err("a non-object fails");
        let message = panic_message(failed);
        assert!(
            message.contains("arguments are an object"),
            "the failure names the object: {message}"
        );
    }
}

#[test]
fn first_text_part_wins() {
    let output = output_with(vec![
        ContentPart::Text {
            text: "first".to_owned(),
        },
        ContentPart::Text {
            text: "second".to_owned(),
        },
    ]);
    assert_eq!(text_of(&output), "first");
}

#[test]
fn no_parts_is_empty() {
    assert_eq!(text_of(&output_with(vec![])), "");
}

#[test]
fn a_first_image_is_empty_even_with_a_later_text() {
    let output = output_with(vec![
        ContentPart::Image {
            path: "a.png".to_owned(),
            mime_type: "image/png".to_owned(),
            width: 1,
            height: 1,
        },
        ContentPart::Text {
            text: "later".to_owned(),
        },
    ]);
    assert_eq!(text_of(&output), "");
}

fn output_with(content: Vec<ContentPart>) -> contract::tool::Output {
    contract::tool::Output {
        content,
        ..Default::default()
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "a non-string panic".to_owned()
    }
}
