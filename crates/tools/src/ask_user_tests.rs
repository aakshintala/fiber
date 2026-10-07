use contract::ErrorCode;
use contract::events::Control;
use contract::shapes::{Choice, ContentPart, Question};
use contract::tool::Tool;
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::{AskUser, SENT};

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            panic!("arguments are an object")
        }
    }
}

fn run(arguments: &Map<String, Value>) -> contract::tool::Output {
    AskUser.run(arguments, &CancelToken::new(), &Recorder::default())
}

#[test]
fn the_definition_is_at_most_1200_bytes() {
    let size = serde_json::to_vec(&AskUser.definition()).unwrap().len();
    assert!(size <= 1_200, "the definition is {size} bytes");
}

#[test]
fn the_schema_holds_the_limits() {
    let definition = AskUser.definition();
    let schema = &definition.input_schema;
    assert_eq!(definition.name, "ask_user");
    assert!(!definition.deferred);
    assert_eq!(definition.hosted, None);
    assert_eq!(schema["required"], json!(["questions"]));
    assert_eq!(schema["additionalProperties"], false);
    let questions = &schema["properties"]["questions"];
    assert_eq!(questions["type"], "array");
    assert_eq!(questions["minItems"], 1);
    assert_eq!(questions["maxItems"], 4);
    let question = &questions["items"];
    assert_eq!(question["required"], json!(["question", "header"]));
    assert_eq!(question["additionalProperties"], false);
    assert_eq!(question["properties"]["question"]["type"], "string");
    assert_eq!(question["properties"]["header"]["type"], "string");
    assert_eq!(question["properties"]["header"]["maxLength"], 12);
    assert_eq!(question["properties"]["multiSelect"]["type"], "boolean");
    let options = &question["properties"]["options"];
    assert_eq!(options["type"], "array");
    assert_eq!(options["minItems"], 2);
    assert_eq!(options["maxItems"], 4);
    let option = &options["items"];
    assert_eq!(option["required"], json!(["label"]));
    assert_eq!(option["additionalProperties"], false);
    assert_eq!(option["properties"]["label"]["type"], "string");
    assert_eq!(option["properties"]["description"]["type"], "string");
}

#[test]
fn the_description_asks_for_a_recommended_option_first() {
    let description = AskUser.definition().description;
    for words in [
        "(Recommended)",
        "the option you recommend first",
        "rather than listing choices in your reply",
    ] {
        assert!(description.contains(words), "{words}: {description}");
    }
}

#[test]
fn it_declares_no_effects() {
    let effects = AskUser
        .effects(&args(
            json!({"questions": [{"header": "h", "question": "q"}]}),
        ))
        .unwrap();

    assert!(effects.declared.effects.is_empty());
    assert!(effects.declared.reversible);
    assert_eq!(effects.declared.paths, None);
    assert_eq!(effects.subject, Some(String::new()));
    assert_eq!(effects.prefix, None);
}

#[test]
fn the_questions_go_to_the_driver() {
    let output = run(&args(json!({"questions": [
        {"header": "Base", "question": "Which branch?", "multiSelect": true,
         "options": [{"label": "main (Recommended)"}, {"label": "dev", "description": "d"}]},
        {"header": "Name", "question": "What name?"}
    ]})));

    assert_eq!(
        output.content,
        [ContentPart::Text {
            text: SENT.to_owned()
        }]
    );
    assert_eq!(
        SENT,
        "The questions went to the driver. The answers arrive as the next prompt."
    );
    assert!(output.error.is_none());
    assert_eq!(output.details, None);
    let asked = vec![
        Question {
            header: "Base".to_owned(),
            question: "Which branch?".to_owned(),
            options: vec![
                Choice {
                    label: "main (Recommended)".to_owned(),
                    description: None,
                },
                Choice {
                    label: "dev".to_owned(),
                    description: Some("d".to_owned()),
                },
            ],
            multi_select: Some(true),
        },
        Question {
            header: "Name".to_owned(),
            question: "What name?".to_owned(),
            options: Vec::new(),
            multi_select: None,
        },
    ];
    assert_eq!(
        output.control,
        Some(Control {
            handoff: None,
            questions: Some(asked),
        })
    );
}

#[test]
fn arguments_that_are_not_questions_fail() {
    for arguments in [
        json!({"questions": 3}),
        json!({}),
        json!({"questions": [{}]}),
    ] {
        let output = run(&args(arguments.clone()));
        assert_eq!(
            output.error.as_ref().map(|error| &error.code),
            Some(&ErrorCode::InvalidArguments),
            "{arguments}"
        );
        assert_eq!(output.control, None, "{arguments}");
        assert_eq!(output.content.len(), 1, "{arguments}");
    }
}
