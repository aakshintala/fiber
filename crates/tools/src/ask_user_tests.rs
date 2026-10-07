use std::sync::Mutex;

use contract::events::{Answer, Control, FormAnswer, Interaction};
use contract::shapes::{Choice, ContentPart, Question, True};
use contract::tool::{Answered, Ask, Asking, Output, Tool};
use contract::{ActionId, ErrorCode};
use fakes::{CancelToken, Recorder};
use serde_json::{Map, Value, json};

use super::{AskUser, DECLINED, SENT};

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

/// An asker that replays scripted answers, reports a settable
/// `answerable`, and records what each ask carried.
struct FakeAsk {
    answerable: bool,
    answers: Mutex<Vec<Answered>>,
    asked: Mutex<Vec<Recorded>>,
}

/// What one ask carried; `Asking` holds a closure, so it is not compared
/// whole.
#[derive(Debug, PartialEq)]
struct Recorded {
    interaction: Interaction,
    action_ids: Vec<ActionId>,
    until: Option<std::time::Instant>,
    checked: bool,
}

impl FakeAsk {
    fn answering(answered: Answered) -> Self {
        Self {
            answerable: true,
            answers: Mutex::new(vec![answered]),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn unanswerable() -> Self {
        Self {
            answerable: false,
            answers: Mutex::new(Vec::new()),
            asked: Mutex::new(Vec::new()),
        }
    }

    fn asked(&self) -> Vec<Recorded> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

impl Ask for FakeAsk {
    fn action(&self) -> ActionId {
        ActionId("a_1".to_owned())
    }

    fn answerable(&self) -> bool {
        self.answerable
    }

    fn ask(&self, asking: Asking) -> Answered {
        self.asked.lock().unwrap().push(Recorded {
            interaction: asking.interaction,
            action_ids: asking.action_ids,
            until: asking.until,
            checked: asking.check.is_some(),
        });
        self.answers.lock().unwrap().remove(0)
    }
}

fn two_questions() -> Map<String, Value> {
    args(json!({"questions": [
        {"header": "Base", "question": "Which branch?", "multiSelect": true,
         "options": [{"label": "main (Recommended)"}, {"label": "dev"}]},
        {"header": "Name", "question": "What name?"}
    ]}))
}

fn run_asking(arguments: &Map<String, Value>, ask: &FakeAsk) -> Output {
    AskUser.run_asking(arguments, &CancelToken::new(), &Recorder::default(), ask)
}

fn text_of(output: &Output) -> &str {
    match output.content.as_slice() {
        [ContentPart::Text { text }] => text,
        other => panic!("one text part: {other:?}"),
    }
}

fn answered(labels: &[&str], text: Option<&str>) -> FormAnswer {
    FormAnswer::Answered {
        labels: labels.iter().map(|label| (*label).to_owned()).collect(),
        text: text.map(str::to_owned),
    }
}

fn skipped() -> FormAnswer {
    FormAnswer::Skipped { skipped: True }
}

fn form(answers: Vec<FormAnswer>, note: Option<&str>) -> Answered {
    Answered::Reply(Answer::Form {
        answers,
        note: note.map(str::to_owned),
    })
}

/// The result `ask_user` gives `arguments` when `answered`.
fn result(arguments: &Map<String, Value>, answered: Answered) -> Output {
    run_asking(arguments, &FakeAsk::answering(answered))
}

/// One question with `header` and `labels` as its options.
fn one(header: &str, labels: &[&str], multi: bool) -> Map<String, Value> {
    let options: Vec<Value> = labels.iter().map(|label| json!({"label": label})).collect();
    let question = if options.is_empty() {
        json!({"header": header, "question": "?"})
    } else {
        json!({"header": header, "question": "?", "options": options, "multiSelect": multi})
    };
    args(json!({ "questions": [question] }))
}

#[test]
fn a_session_nobody_can_answer_gets_the_questions_without_an_ask() {
    let arguments = two_questions();
    let ask = FakeAsk::unanswerable();
    assert_eq!(run_asking(&arguments, &ask), run(&arguments));
    assert_eq!(ask.asked(), []);
}

#[test]
fn the_ask_is_one_form_of_the_questions_with_no_timeout() {
    let arguments = two_questions();
    let ask = FakeAsk::answering(form(vec![skipped(), skipped()], None));
    let _ = run_asking(&arguments, &ask);
    let questions: Vec<Question> = serde_json::from_value(arguments["questions"].clone()).unwrap();
    assert_eq!(
        ask.asked(),
        [Recorded {
            interaction: Interaction::Form { fields: questions },
            action_ids: Vec::new(),
            until: None,
            checked: false,
        }]
    );
}

#[test]
fn each_answer_is_one_line_in_field_order() {
    let rows: [(Map<String, Value>, Vec<FormAnswer>, &str); 8] = [
        (
            one("Base", &["main (Recommended)", "dev"], true),
            vec![answered(&["main (Recommended)", "dev"], None)],
            "Base: main (Recommended), dev",
        ),
        (
            one("Base", &["main", "dev"], false),
            vec![answered(&["dev"], None)],
            "Base: dev",
        ),
        (
            one("Base", &["main", "dev"], true),
            vec![answered(&["dev"], Some("and tags"))],
            "Base: dev, \"and tags\"",
        ),
        (
            one("Name", &[], false),
            vec![answered(&[], Some("fiber-cli"))],
            "Name: \"fiber-cli\"",
        ),
        (
            one("Name", &[], false),
            vec![answered(&[], Some("a\nb \"c\""))],
            "Name: \"a\\nb \\\"c\\\"\"",
        ),
        (one("Name", &[], false), vec![skipped()], "Name: skipped"),
        (
            one("Pick", &["a", "b"], true),
            vec![answered(&[], None)],
            "Pick: ",
        ),
        (
            one("Pick\nnow", &["a\tb \"c\"", "d"], false),
            vec![answered(&["a\tb \"c\""], None)],
            "Pick\\nnow: a\\tb \\\"c\\\"",
        ),
    ];
    for (arguments, answers, line) in rows {
        let output = result(&arguments, form(answers, None));
        assert_eq!(text_of(&output), line);
        assert_eq!(output.error, None, "{line}");
        assert_eq!(output.control, None, "{line}");
    }
}

#[test]
fn four_answers_come_in_field_order() {
    let arguments = args(json!({"questions": [
        {"header": "One", "question": "?"},
        {"header": "Two", "question": "?", "options": [{"label": "x"}, {"label": "y"}]},
        {"header": "Three", "question": "?"},
        {"header": "Four", "question": "?", "options": [{"label": "p"}, {"label": "q"}],
         "multiSelect": true}
    ]}));
    let answers = vec![
        answered(&[], Some("1")),
        answered(&["y"], None),
        skipped(),
        answered(&["p", "q"], None),
    ];
    assert_eq!(
        text_of(&result(&arguments, form(answers, None))),
        "One: \"1\"\nTwo: y\nThree: skipped\nFour: p, q"
    );
}

#[test]
fn a_note_is_its_own_line_after_the_answers() {
    let answers = vec![
        answered(&["main (Recommended)", "dev"], Some("and tags")),
        answered(&[], Some("fiber-cli")),
    ];
    assert_eq!(
        text_of(&result(&two_questions(), form(answers, Some("by friday")))),
        "Base: main (Recommended), dev, \"and tags\"\nName: \"fiber-cli\"\nnote: \"by friday\""
    );
}

#[test]
fn a_person_who_declines_gets_declined() {
    let output = result(
        &two_questions(),
        Answered::Reply(Answer::Declined { declined: True }),
    );
    assert_eq!(text_of(&output), DECLINED);
    assert_eq!(DECLINED, "declined");
    assert_eq!(output.control, None);
    assert_eq!(output.error, None);
}

#[test]
fn no_answer_on_a_cancel_is_declined() {
    let ask = FakeAsk::answering(Answered::NoAnswer);
    let cancel = CancelToken::new();
    cancel.cancel();
    let output = AskUser.run_asking(&two_questions(), &cancel, &Recorder::default(), &ask);
    assert_eq!(text_of(&output), DECLINED);
    assert_eq!(output.control, None);
    assert_eq!(output.error, None);
}

#[test]
fn no_answer_without_a_cancel_sends_the_questions_to_the_driver() {
    let arguments = two_questions();
    let output = result(&arguments, Answered::NoAnswer);
    assert_eq!(output, run(&arguments));
    assert_eq!(text_of(&output), SENT);
    assert!(
        output
            .control
            .is_some_and(|control| control.questions.is_some())
    );
}

#[test]
fn an_answer_of_another_kind_fails() {
    for answer in [
        Answer::Confirmed { confirmed: true },
        Answer::Labels {
            labels: vec!["dev".to_owned()],
        },
        Answer::Text {
            text: "x".to_owned(),
        },
    ] {
        let output = result(&two_questions(), Answered::Reply(answer.clone()));
        assert_eq!(
            output
                .error
                .as_ref()
                .map(|error| (&error.code, error.message.as_str())),
            Some((&ErrorCode::ToolError, "The answer does not fit the form.")),
            "{answer:?}"
        );
        assert_eq!(output.control, None, "{answer:?}");
    }
}

#[test]
fn arguments_that_are_not_questions_fail_without_an_ask() {
    for arguments in [
        json!({"questions": 3}),
        json!({}),
        json!({"questions": [{}]}),
    ] {
        let ask = FakeAsk::answering(Answered::NoAnswer);
        let output = run_asking(&args(arguments.clone()), &ask);
        assert_eq!(
            output.error.as_ref().map(|error| &error.code),
            Some(&ErrorCode::InvalidArguments),
            "{arguments}"
        );
        assert_eq!(ask.asked(), [], "{arguments}");
    }
}
