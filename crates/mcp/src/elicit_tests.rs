//! The elicitation form: reading, interactions, checks, values and
//! answers, with the shared fake asker the server and tool tests drive.

use std::sync::mpsc;
use std::time::Duration;

use contract::ActionId;
use contract::clock::Clock;
use contract::events::{Answer, Interaction};
use contract::shapes::{Choice, True};
use contract::tool::{Answered, Ask, Asking, Check};
use fakes::Deadline;
use serde_json::{Value, json};

use super::{Field, FieldKind, Form, check, interaction, read, respond};

/// How long a test waits for an ask or an answer, in real time.
const WITHIN: Duration = crate::test_support::WITHIN;

/// One ask `respond` emitted: the interaction, who it belongs to, when
/// it ends, and the check beyond the kind's.
pub(crate) struct Recorded {
    /// What it asked.
    pub interaction: Interaction,
    /// Every call it belongs to.
    pub action_ids: Vec<ActionId>,
    /// When it ends unanswered.
    pub until: Option<std::time::Instant>,
    /// The check beyond the kind's, if any.
    pub check: Option<Check>,
}

/// An asker scripted with answers: it records each ask on a channel and
/// answers from a channel of `Answered` with a deadline naming the wait.
/// `answerable` is true: an answer is always possible here. The answers
/// sit behind a lock, so one asker is shared by two calls on two threads.
pub(crate) struct FakeAsk {
    /// The call's own action.
    action: ActionId,
    /// Where each ask is recorded.
    seen: mpsc::Sender<Recorded>,
    /// The scripted answers, in order.
    replies: std::sync::Arc<std::sync::Mutex<mpsc::Receiver<Answered>>>,
}

impl FakeAsk {
    /// One asker for `action`, scripted with `answers` in order, and the
    /// channel its asks arrive on.
    pub(crate) fn scripted(
        action: &str,
        answers: Vec<Answered>,
    ) -> (Self, mpsc::Receiver<Recorded>) {
        let (seen, recorded) = mpsc::channel();
        let (send, replies) = mpsc::channel();
        for answer in answers {
            send.send(answer).expect("the script is read");
        }
        (
            Self {
                action: ActionId(action.to_owned()),
                seen,
                replies: std::sync::Arc::new(std::sync::Mutex::new(replies)),
            },
            recorded,
        )
    }

    /// One asker the test answers by hand, with the sender its answers
    /// arrive on: nothing is scripted, so the test decides when each ask
    /// ends, as the loop does at `until`.
    pub(crate) fn held(action: &str) -> (Self, mpsc::Receiver<Recorded>, mpsc::Sender<Answered>) {
        let (seen, recorded) = mpsc::channel();
        let (send, replies) = mpsc::channel();
        (
            Self {
                action: ActionId(action.to_owned()),
                seen,
                replies: std::sync::Arc::new(std::sync::Mutex::new(replies)),
            },
            recorded,
            send,
        )
    }

    /// One asker for another call sharing this one's answers and records:
    /// either call may take an elicitation, so both draw from one script.
    pub(crate) fn sharing(&self, action: &str) -> Self {
        Self {
            action: ActionId(action.to_owned()),
            seen: self.seen.clone(),
            replies: std::sync::Arc::clone(&self.replies),
        }
    }

    /// One asker that fails the test when asked: its answers are empty
    /// and the sender is dropped, so any ask panics naming the wait.
    pub(crate) fn refusing(action: &str) -> (Self, mpsc::Receiver<Recorded>) {
        let (seen, recorded) = mpsc::channel();
        let (_send, replies) = mpsc::channel::<Answered>();
        (
            Self {
                action: ActionId(action.to_owned()),
                seen,
                replies: std::sync::Arc::new(std::sync::Mutex::new(replies)),
            },
            recorded,
        )
    }
}

impl Ask for FakeAsk {
    fn action(&self) -> ActionId {
        self.action.clone()
    }

    fn answerable(&self) -> bool {
        true
    }

    fn ask(&self, asking: Asking) -> Answered {
        // An elicitation never suspends: the call waits on the person
        // within its own timeout instead.
        assert!(!asking.suspends, "an elicitation ask never suspends");
        self.seen
            .send(Recorded {
                interaction: asking.interaction,
                action_ids: asking.action_ids,
                until: asking.until,
                check: asking.check,
            })
            .expect("the test reads each ask");
        let replies = self.replies.lock().expect("one ask answers at a time");
        Deadline::after(WITHIN).recv_or_fail(&replies, "the next scripted answer")
    }
}

/// One elicitation line with `properties` verbatim, so the test pins the
/// server's document order, and `required` verbatim.
fn line(properties: &str, required: &str) -> String {
    // Concatenated, not formatted: no brace escapes to miscount.
    "{\"jsonrpc\":\"2.0\",\"id\":\"el-1\",\"method\":\"elicitation/create\",".to_owned()
        + "\"params\":{\"message\":\"Deploy settings\",\"requestedSchema\":{\"type\":\"object\","
        + "\"properties\":{"
        + properties
        + "},\"required\":"
        + required
        + "}}}"
}

/// The `check` of the field `name` read from `properties` with `required`.
fn check_of(properties: &str, required: &str, name: &str) -> Check {
    let body = line(properties, required);
    let form = read(&body).unwrap_or_else(|reason| panic!("the form reads: {reason}: {body}"));
    let field = form
        .fields
        .iter()
        .find(|field| field.name == name)
        .unwrap_or_else(|| panic!("field {name} is listed"));
    check(field).unwrap_or_else(|| panic!("field {name} has a check"))
}

/// Asks one field of `form` with `answer` and returns what `respond`
/// would send for the whole form.
fn answer_once(form: &Form, answer: Answered) -> Value {
    let (ask, _seen) = FakeAsk::scripted("a_1", vec![answer]);
    let fake = fakes::clock::FakeClock::new();
    respond(
        super::Asker {
            ask: &ask,
            server: "fx",
        },
        form,
        vec![ActionId("a_1".to_owned())],
        fake.now() + Duration::from_secs(60),
    )
}

/// One required field of `kind`, for the value rows.
fn one_field(name: &str, kind: FieldKind) -> Form {
    Form {
        message: "m".to_owned(),
        fields: vec![Field {
            name: name.to_owned(),
            title: None,
            description: None,
            required: true,
            kind,
        }],
    }
}

#[test]
fn fields_read_in_the_servers_order_with_required_set() {
    // `name` before `approve` is the reverse of sorted order, so the
    // order proves the read keeps the document order.
    let form = read(&line(
        r#""name":{"type":"string"},"approve":{"type":"boolean"},"count":{"type":"integer"}"#,
        r#"["name"]"#,
    ))
    .expect("the form reads");
    assert_eq!(form.message, "Deploy settings");
    let names: Vec<&str> = form
        .fields
        .iter()
        .map(|field| field.name.as_str())
        .collect();
    assert_eq!(names, vec!["name", "approve", "count"]);
    let required: Vec<bool> = form.fields.iter().map(|field| field.required).collect();
    assert_eq!(required, vec![true, false, false]);
}

#[test]
fn unknown_property_keys_are_ignored() {
    // `pattern`, `default` and `format` are the server's to check, not Fiber's.
    let form = read(&line(
        r#""name":{"type":"string","pattern":"^[a-z]+$","default":"prod","minLength":1}"#,
        r#"[]"#,
    ))
    .expect("the form reads");
    assert_eq!(form.fields.len(), 1);
    assert!(matches!(
        form.fields[0].kind,
        FieldKind::Text {
            min: Some(1),
            max: None
        }
    ));
    let formatted = read(&line(
        r#""email":{"type":"string","format":"email"}"#,
        r#"[]"#,
    ))
    .expect("a formatted string reads");
    assert!(matches!(formatted.fields[0].kind, FieldKind::Text { .. }));
}

#[test]
fn every_malformed_schema_names_its_key_or_property() {
    let cases: &[(&str, String, &str)] = &[
        (
            "no params",
            r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create"}"#.to_owned(),
            "`params`",
        ),
        (
            "params not an object",
            r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create","params":[]}"#.to_owned(),
            "`params`",
        ),
        (
            "message not a string",
            r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create","params":{"message":42,"requestedSchema":{"type":"object","properties":{"name":{"type":"string"}}}}}"#.to_owned(),
            "`message`",
        ),
        (
            "no requested schema",
            r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create","params":{"message":"m"}}"#.to_owned(),
            "`requestedSchema`",
        ),
        (
            "requested schema not an object",
            r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create","params":{"message":"m","requestedSchema":[]}}"#.to_owned(),
            "`requestedSchema`",
        ),
        (
            "schema type not object",
            line(r#""name":{"type":"string"}"#, r#"[]"#)
                .replace(r#""type":"object""#, r#""type":"array""#),
            "`requestedSchema.type`",
        ),
        (
            "no properties",
            r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create","params":{"message":"m","requestedSchema":{"type":"object"}}}"#.to_owned(),
            "`requestedSchema.properties`",
        ),
        (
            "properties not an object",
            r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create","params":{"message":"m","requestedSchema":{"type":"object","properties":[]}}}"#.to_owned(),
            "`requestedSchema.properties`",
        ),
        (
            "properties empty",
            r#"{"jsonrpc":"2.0","id":"el-1","method":"elicitation/create","params":{"message":"m","requestedSchema":{"type":"object","properties":{}}}}"#.to_owned(),
            "`requestedSchema.properties`",
        ),
        (
            "repeated key",
            line(r#""name":{"type":"string"},"name":{"type":"boolean"}"#, r#"[]"#),
            "`name`",
        ),
        (
            "property not an object",
            line(r#""name":"string""#, r#"[]"#),
            "`name`",
        ),
        (
            "property with no type",
            line(r#""name":{"title":"Name"}"#, r#"[]"#),
            "`name`",
        ),
        (
            "property of another type",
            line(r#""tags":{"type":"array"}"#, r#"[]"#),
            "`tags`",
        ),
        (
            "enum on a number",
            line(r#""count":{"type":"number","enum":[1,2]}"#, r#"[]"#),
            "`count`",
        ),
        (
            "enum on an integer",
            line(r#""count":{"type":"integer","enum":[1,2]}"#, r#"[]"#),
            "`count`",
        ),
        (
            "enum on a boolean",
            line(r#""ok":{"type":"boolean","enum":[true]}"#, r#"[]"#),
            "`ok`",
        ),
        (
            "empty enum",
            line(r#""env":{"type":"string","enum":[]}"#, r#"[]"#),
            "`env`",
        ),
        (
            "non-string enum",
            line(r#""env":{"type":"string","enum":["a",1]}"#, r#"[]"#),
            "`env`",
        ),
        (
            "repeated enum value",
            line(r#""env":{"type":"string","enum":["a","a"]}"#, r#"[]"#),
            "`env`",
        ),
        (
            "enum names of another length",
            line(r#""env":{"type":"string","enum":["a","b"],"enumNames":["A"]}"#, r#"[]"#),
            "`env`",
        ),
        (
            "non-string enum names",
            line(r#""env":{"type":"string","enum":["a"],"enumNames":[1]}"#, r#"[]"#),
            "`env`",
        ),
        (
            "title not a string",
            line(r#""name":{"type":"string","title":1}"#, r#"[]"#),
            "`name`",
        ),
        (
            "description not a string",
            line(r#""name":{"type":"string","description":1}"#, r#"[]"#),
            "`name`",
        ),
        (
            "negative minLength",
            line(r#""name":{"type":"string","minLength":-1}"#, r#"[]"#),
            "`name`",
        ),
        (
            "fractional maxLength",
            line(r#""name":{"type":"string","maxLength":2.5}"#, r#"[]"#),
            "`name`",
        ),
        (
            "minimum not a number",
            line(r#""count":{"type":"integer","minimum":"1"}"#, r#"[]"#),
            "`count`",
        ),
        (
            "maximum not a number",
            line(r#""n":{"type":"number","maximum":true}"#, r#"[]"#),
            "`n`",
        ),
        (
            "required not an array",
            line(r#""name":{"type":"string"}"#, r#""name""#),
            "`required`",
        ),
        (
            "required names no listed property",
            line(r#""name":{"type":"string"}"#, r#"["other"]"#),
            "`other`",
        ),
        (
            "required holds a non-string",
            line(r#""name":{"type":"string"}"#, r#"[1]"#),
            "`required`",
        ),
    ];
    for (name, body, needle) in cases {
        match read(body) {
            Ok(_) => panic!("{name} reads, naming {needle}"),
            Err(reason) => assert!(reason.contains(needle), "{name} names {needle}: {reason}"),
        }
    }
}

#[test]
fn each_field_maps_to_its_interaction_with_its_prompt() {
    let form = Form {
        message: "Deploy settings".to_owned(),
        fields: vec![
            Field {
                name: "approve".to_owned(),
                title: Some("Approve".to_owned()),
                description: None,
                required: true,
                kind: FieldKind::Boolean,
            },
            Field {
                name: "env".to_owned(),
                title: None,
                description: Some("The target".to_owned()),
                required: false,
                kind: FieldKind::Enum {
                    values: vec!["prod".to_owned(), "dev".to_owned()],
                    names: Some(vec!["Production".to_owned(), "Development".to_owned()]),
                },
            },
            Field {
                name: "note".to_owned(),
                title: Some("Note".to_owned()),
                description: Some("A note".to_owned()),
                required: false,
                kind: FieldKind::Text {
                    min: None,
                    max: None,
                },
            },
        ],
    };
    assert_eq!(
        interaction("fx", &form, &form.fields[0]),
        Interaction::Confirm {
            prompt: "The MCP server `fx` asks: Deploy settings\n\nApprove".to_owned(),
        }
    );
    assert_eq!(
        interaction("fx", &form, &form.fields[1]),
        Interaction::Select {
            prompt: "The MCP server `fx` asks: Deploy settings\n\nenv: The target".to_owned(),
            options: vec![
                Choice {
                    label: "prod".to_owned(),
                    description: Some("Production".to_owned()),
                },
                Choice {
                    label: "dev".to_owned(),
                    description: Some("Development".to_owned()),
                },
            ],
        }
    );
    assert_eq!(
        interaction("fx", &form, &form.fields[2]),
        Interaction::TextInput {
            prompt: "The MCP server `fx` asks: Deploy settings\n\nNote: A note".to_owned(),
        }
    );
}

#[test]
fn an_enum_without_names_offers_bare_labels() {
    let form = Form {
        message: "m".to_owned(),
        fields: vec![Field {
            name: "env".to_owned(),
            title: None,
            description: None,
            required: true,
            kind: FieldKind::Enum {
                values: vec!["prod".to_owned()],
                names: None,
            },
        }],
    };
    assert_eq!(
        interaction("fx", &form, &form.fields[0]),
        Interaction::Select {
            prompt: "The MCP server `fx` asks: m\n\nenv".to_owned(),
            options: vec![Choice {
                label: "prod".to_owned(),
                description: None,
            }],
        }
    );
}

#[test]
fn numbers_check_the_text_against_their_limits() {
    let check = check_of(
        r#""n":{"type":"number","minimum":1,"maximum":5}"#,
        r#"["n"]"#,
        "n",
    );
    for text in ["1", "5", " 3 "] {
        assert!(
            check(&Answer::Text {
                text: text.to_owned()
            }),
            "{text} passes",
        );
    }
    for text in ["0.999", "5.001", "abc", "NaN", "inf", ""] {
        assert!(
            !check(&Answer::Text {
                text: text.to_owned()
            }),
            "{text} fails",
        );
    }
}

#[test]
fn integers_check_whole_numbers_exactly() {
    let check = check_of(r#""count":{"type":"integer"}"#, r#"["count"]"#, "count");
    assert!(check(&Answer::Text {
        text: "3".to_owned()
    }));
    assert!(check(&Answer::Text {
        text: "-4".to_owned()
    }));
    for text in ["3.5", "abc", ""] {
        assert!(
            !check(&Answer::Text {
                text: text.to_owned()
            }),
            "{text} fails",
        );
    }
    // Each bound is checked on both sides: at it passes, past it fails.
    let bounded: &[(&str, &str, bool)] = &[
        (
            r#""count":{"type":"integer","maximum":9007199254740992}"#,
            "9007199254740992",
            true,
        ),
        (
            r#""count":{"type":"integer","maximum":9007199254740992}"#,
            "9007199254740993",
            false,
        ),
        (
            r#""count":{"type":"integer","minimum":9007199254740993}"#,
            "9007199254740993",
            true,
        ),
        (
            r#""count":{"type":"integer","minimum":9007199254740993}"#,
            "9007199254740992",
            false,
        ),
        (r#""count":{"type":"integer","minimum":-5}"#, "-5", true),
        (r#""count":{"type":"integer","minimum":-5}"#, "-6", false),
        (r#""count":{"type":"integer","maximum":-5}"#, "-5", true),
        (r#""count":{"type":"integer","maximum":-5}"#, "-4", false),
        (r#""count":{"type":"integer","minimum":1.5}"#, "1", false),
        (r#""count":{"type":"integer","minimum":1.5}"#, "2", true),
        (r#""count":{"type":"integer","maximum":-1.5}"#, "-2", true),
        (r#""count":{"type":"integer","maximum":-1.5}"#, "-1", false),
        (r#""count":{"type":"integer","minimum":3.0}"#, "2", false),
        (r#""count":{"type":"integer","minimum":3.0}"#, "3", true),
        (
            r#""count":{"type":"integer","maximum":18446744073709551615}"#,
            "9223372036854775807",
            true,
        ),
        // `9223372036854775808.0` equals the clamp's upper edge: as a
        // minimum it leaves no `i64` standing, so the largest one fails.
        (
            r#""count":{"type":"integer","minimum":9223372036854775808.0}"#,
            "9223372036854775807",
            false,
        ),
    ];
    for (properties, text, passes) in bounded {
        let check = check_of(properties, r#"["count"]"#, "count");
        assert_eq!(
            check(&Answer::Text {
                text: (*text).to_owned()
            }),
            *passes,
            "{text} against {properties}",
        );
    }
    // Past `i64` no whole number Fiber can send parses, so it fails.
    let past = check_of(
        r#""count":{"type":"integer","maximum":18446744073709551615}"#,
        r#"["count"]"#,
        "count",
    );
    assert!(!past(&Answer::Text {
        text: "9223372036854775808".to_owned()
    }));
    // Out-of-range bounds clamp: an unsatisfiable minimum rejects every
    // `i64`, an unconstraining maximum accepts every one, and the reverse
    // below it.
    let extremes: &[(&str, &str, bool)] = &[
        (
            r#""count":{"type":"integer","minimum":1e100}"#,
            "9223372036854775807",
            false,
        ),
        (
            r#""count":{"type":"integer","minimum":1e100}"#,
            "-9223372036854775808",
            false,
        ),
        (
            r#""count":{"type":"integer","maximum":1e100}"#,
            "9223372036854775807",
            true,
        ),
        (
            r#""count":{"type":"integer","maximum":1e100}"#,
            "-9223372036854775808",
            true,
        ),
        (
            r#""count":{"type":"integer","minimum":-1e100}"#,
            "9223372036854775807",
            true,
        ),
        (
            r#""count":{"type":"integer","minimum":-1e100}"#,
            "-9223372036854775808",
            true,
        ),
        (
            r#""count":{"type":"integer","maximum":-1e100}"#,
            "9223372036854775807",
            false,
        ),
        (
            r#""count":{"type":"integer","maximum":-1e100}"#,
            "-9223372036854775808",
            false,
        ),
    ];
    for (properties, text, passes) in extremes {
        let check = check_of(properties, r#"["count"]"#, "count");
        assert_eq!(
            check(&Answer::Text {
                text: (*text).to_owned()
            }),
            *passes,
            "{text} against {properties}",
        );
    }
}

#[test]
fn strings_count_chars_with_empty_optional_passing() {
    let check = check_of(
        r#""name":{"type":"string","minLength":2,"maxLength":3}"#,
        r#"["name"]"#,
        "name",
    );
    for text in ["ab", "abc", "éé", "ééé"] {
        assert!(
            check(&Answer::Text {
                text: text.to_owned()
            }),
            "{text} passes",
        );
    }
    for text in ["a", "abcd", "é", "éééé", ""] {
        assert!(
            !check(&Answer::Text {
                text: text.to_owned()
            }),
            "{text} fails",
        );
    }
    let optional = check_of(r#""n":{"type":"number"}"#, r#"[]"#, "n");
    assert!(optional(&Answer::Text {
        text: "".to_owned()
    }));
    assert!(optional(&Answer::Text {
        text: "   ".to_owned()
    }));
    let required_string = check_of(r#""name":{"type":"string"}"#, r#"["name"]"#, "name");
    assert!(required_string(&Answer::Text {
        text: "".to_owned()
    }));
}

#[test]
fn confirm_and_select_carry_no_check() {
    let form = read(&line(
        r#""approve":{"type":"boolean"},"env":{"type":"string","enum":["a"]}"#,
        r#"[]"#,
    ))
    .expect("the form reads");
    for field in &form.fields {
        assert!(check(field).is_none(), "{} carries no check", field.name);
    }
}

#[test]
fn three_answered_fields_accept_with_every_value() {
    let form = read(&line(
        r#""name":{"type":"string"},"approve":{"type":"boolean"},"count":{"type":"integer","minimum":1}"#,
        r#"["name","approve","count"]"#,
    ))
    .expect("the form reads");
    let (ask, seen) = FakeAsk::scripted(
        "a_1",
        vec![
            Answered::Reply(Answer::Text {
                text: "prod".to_owned(),
            }),
            Answered::Reply(Answer::Confirmed { confirmed: true }),
            Answered::Reply(Answer::Text {
                text: "3".to_owned(),
            }),
        ],
    );
    let fake = fakes::clock::FakeClock::new();
    let until = fake.now() + Duration::from_secs(60);
    let ids = vec![ActionId("a_1".to_owned())];
    let result = respond(
        super::Asker {
            ask: &ask,
            server: "fx",
        },
        &form,
        ids.clone(),
        until,
    );
    assert_eq!(
        result,
        json!({"action": "accept", "content": {"approve": true, "count": 3, "name": "prod"}}),
    );
    let wait = Deadline::after(WITHIN);
    let mut prompts = Vec::new();
    for (index, field) in form.fields.iter().enumerate() {
        let recorded = wait.recv_or_fail(&seen, "each field's ask");
        assert_eq!(recorded.action_ids, ids, "ask {index} carries the calls");
        assert_eq!(
            recorded.until,
            Some(until),
            "ask {index} carries the deadline"
        );
        assert_eq!(
            recorded.interaction,
            interaction("fx", &form, field),
            "ask {index} asks its field",
        );
        prompts.push(recorded);
    }
    assert!(
        seen.try_recv().is_err(),
        "three fields ask exactly three times"
    );
    // The integer field's ask carries the check the loop applies: it
    // refuses what the server's limits refuse. The boolean field's ask
    // carries none.
    let integer = prompts[2].check.as_ref().expect("the integer ask checks");
    assert!(!integer(&Answer::Text {
        text: "0".to_owned()
    }));
    assert!(!integer(&Answer::Text {
        text: "abc".to_owned()
    }));
    assert!(integer(&Answer::Text {
        text: "3".to_owned()
    }));
    assert!(prompts[1].check.is_none(), "the boolean ask checks nothing");
}

#[test]
fn a_declined_required_field_declines_at_once() {
    let form = read(&line(
        r#""name":{"type":"string"},"approve":{"type":"boolean"},"count":{"type":"integer"}"#,
        r#"["name","approve","count"]"#,
    ))
    .expect("the form reads");
    let (ask, seen) = FakeAsk::scripted(
        "a_1",
        vec![
            Answered::Reply(Answer::Text {
                text: "prod".to_owned(),
            }),
            Answered::Reply(Answer::Declined { declined: True }),
        ],
    );
    let fake = fakes::clock::FakeClock::new();
    let result = respond(
        super::Asker {
            ask: &ask,
            server: "fx",
        },
        &form,
        vec![ActionId("a_1".to_owned())],
        fake.now() + Duration::from_secs(60),
    );
    assert_eq!(result, json!({"action": "decline"}));
    let wait = Deadline::after(WITHIN);
    wait.recv_or_fail(&seen, "the first ask");
    wait.recv_or_fail(&seen, "the second ask");
    assert!(seen.try_recv().is_err(), "the third field is never asked");
}

#[test]
fn a_declined_optional_field_is_left_out_and_accepted() {
    let form = read(&line(
        r#""name":{"type":"string"},"nick":{"type":"string"}"#,
        r#"["name"]"#,
    ))
    .expect("the form reads");
    let (ask, seen) = FakeAsk::scripted(
        "a_1",
        vec![
            Answered::Reply(Answer::Text {
                text: "prod".to_owned(),
            }),
            Answered::Reply(Answer::Declined { declined: True }),
        ],
    );
    let fake = fakes::clock::FakeClock::new();
    let result = respond(
        super::Asker {
            ask: &ask,
            server: "fx",
        },
        &form,
        vec![ActionId("a_1".to_owned())],
        fake.now() + Duration::from_secs(60),
    );
    assert_eq!(
        result,
        json!({"action": "accept", "content": {"name": "prod"}})
    );
    let wait = Deadline::after(WITHIN);
    wait.recv_or_fail(&seen, "the first ask");
    wait.recv_or_fail(&seen, "the second ask");
}

#[test]
fn an_empty_optional_number_or_integer_is_left_out_and_the_rest_is_accepted() {
    // Empty text on an optional numeric field passes its check and is
    // left out of `content`, while the answered fields are still
    // accepted.
    let form = read(&line(
        r#""name":{"type":"string"},"n":{"type":"number"},"count":{"type":"integer"}"#,
        r#"["name"]"#,
    ))
    .expect("the form reads");
    let (ask, _seen) = FakeAsk::scripted(
        "a_1",
        vec![
            Answered::Reply(Answer::Text {
                text: "prod".to_owned(),
            }),
            Answered::Reply(Answer::Text {
                text: "".to_owned(),
            }),
            Answered::Reply(Answer::Text {
                text: "   ".to_owned(),
            }),
        ],
    );
    let fake = fakes::clock::FakeClock::new();
    let result = respond(
        super::Asker {
            ask: &ask,
            server: "fx",
        },
        &form,
        vec![ActionId("a_1".to_owned())],
        fake.now() + Duration::from_secs(60),
    );
    assert_eq!(
        result,
        json!({"action": "accept", "content": {"name": "prod"}}),
    );
}

#[test]
fn no_answer_declines_after_one_ask() {
    let form = read(&line(
        r#""name":{"type":"string"},"approve":{"type":"boolean"}"#,
        r#"["name"]"#,
    ))
    .expect("the form reads");
    let (ask, seen) = FakeAsk::scripted("a_1", vec![Answered::NoAnswer]);
    let fake = fakes::clock::FakeClock::new();
    let result = respond(
        super::Asker {
            ask: &ask,
            server: "fx",
        },
        &form,
        vec![ActionId("a_1".to_owned())],
        fake.now() + Duration::from_secs(60),
    );
    assert_eq!(result, json!({"action": "decline"}));
    Deadline::after(WITHIN).recv_or_fail(&seen, "the first ask");
    assert!(seen.try_recv().is_err(), "no later field is asked");
}

#[test]
fn a_shape_error_names_the_object_it_wanted() {
    // The serde message is what the reader sees for a `properties` that
    // is not a map, so it must say what a map is.
    let error = serde_json::from_str::<super::OrderedProperties>("[]")
        .err()
        .expect("an array is not a property object");
    assert!(
        error.to_string().contains("an object of properties"),
        "the error names the wanted object: {error}",
    );
}

#[test]
fn enum_names_are_read_when_they_match_the_values() {
    let form = read(&line(
        r#""env":{"type":"string","enum":["prod","dev"],"enumNames":["Production","Development"]}"#,
        r#"[]"#,
    ))
    .expect("matching names read");
    match &form.fields[0].kind {
        FieldKind::Enum { values, names } => {
            assert_eq!(values, &vec!["prod".to_owned(), "dev".to_owned()]);
            assert_eq!(
                names,
                &Some(vec!["Production".to_owned(), "Development".to_owned()]),
            );
        }
        FieldKind::Boolean
        | FieldKind::Text { .. }
        | FieldKind::Number { .. }
        | FieldKind::Integer { .. } => panic!("the field is an enum"),
    }
    // One name too many is refused as well as one too few.
    let longer = line(
        r#""env":{"type":"string","enum":["prod","dev"],"enumNames":["A","B","C"]}"#,
        r#"[]"#,
    );
    match read(&longer) {
        Ok(_) => panic!("three names for two values read"),
        Err(reason) => assert!(reason.contains("`env`"), "names the property: {reason}"),
    }
}

#[test]
fn an_unbounded_number_refuses_what_is_not_finite() {
    let check = check_of(r#""n":{"type":"number"}"#, r#"["n"]"#, "n");
    for text in ["inf", "-inf", "NaN"] {
        assert!(
            !check(&Answer::Text {
                text: text.to_owned()
            }),
            "{text} fails",
        );
    }
    assert!(check(&Answer::Text {
        text: "2.5".to_owned()
    }));
}

/// What `value` made of an answer, as one string the rows compare.
fn described(field: &Field, answer: &Answer) -> String {
    match super::value(field, answer) {
        super::Valued::Value(value) => format!("value {value}"),
        super::Valued::Omitted => "omitted".to_owned(),
        super::Valued::Decline => "decline".to_owned(),
    }
}

/// A field of `kind`, required or not, for the value rows.
fn field_of(kind: FieldKind, required: bool) -> Field {
    Field {
        name: "f".to_owned(),
        title: None,
        description: None,
        required,
        kind,
    }
}

/// A typed text answer.
fn typed(text: &str) -> Answer {
    Answer::Text {
        text: text.to_owned(),
    }
}

#[test]
fn a_select_answer_must_be_one_of_its_values() {
    let field = field_of(
        FieldKind::Enum {
            values: vec!["prod".to_owned(), "dev".to_owned()],
            names: None,
        },
        true,
    );
    let labels = |label: &str| Answer::Labels {
        labels: vec![label.to_owned()],
    };
    assert_eq!(described(&field, &labels("dev")), "value \"dev\"");
    assert_eq!(described(&field, &labels("staging")), "decline");
}

#[test]
fn an_empty_answer_is_kept_when_required_and_left_out_when_optional() {
    let text = |required| {
        field_of(
            FieldKind::Text {
                min: None,
                max: None,
            },
            required,
        )
    };
    let number = |required| {
        field_of(
            FieldKind::Number {
                min: None,
                max: None,
            },
            required,
        )
    };
    let integer = |required| {
        field_of(
            FieldKind::Integer {
                min: None,
                max: None,
            },
            required,
        )
    };
    // A required empty text is its own value; an optional one is omitted.
    assert_eq!(described(&text(true), &typed("")), "value \"\"");
    assert_eq!(described(&text(false), &typed("")), "omitted");
    // A number or integer that is empty is declined when required, and
    // left out when optional.
    assert_eq!(described(&number(true), &typed("")), "decline");
    assert_eq!(described(&number(false), &typed("")), "omitted");
    assert_eq!(described(&integer(true), &typed("")), "decline");
    assert_eq!(described(&integer(false), &typed("")), "omitted");
    // An optional field that has an answer keeps it.
    assert_eq!(described(&text(false), &typed(" x ")), "value \"x\"");
    assert_eq!(described(&number(false), &typed("3")), "value 3.0");
    assert_eq!(described(&integer(false), &typed("3")), "value 3");
}

#[test]
fn each_answer_maps_to_its_content_value() {
    let text = |text: &str| {
        Answered::Reply(Answer::Text {
            text: text.to_owned(),
        })
    };
    let cases: &[(&str, FieldKind, Answered, Value)] = &[
        (
            "on",
            FieldKind::Boolean,
            Answered::Reply(Answer::Confirmed { confirmed: true }),
            Value::Bool(true),
        ),
        (
            "env",
            FieldKind::Enum {
                values: vec!["prod".to_owned()],
                names: None,
            },
            Answered::Reply(Answer::Labels {
                labels: vec!["prod".to_owned()],
            }),
            Value::String("prod".to_owned()),
        ),
        (
            "name",
            FieldKind::Text {
                min: None,
                max: None,
            },
            text("hi"),
            Value::String("hi".to_owned()),
        ),
        (
            "count",
            FieldKind::Integer {
                min: None,
                max: None,
            },
            text("3"),
            json!(3),
        ),
        (
            "n",
            FieldKind::Number {
                min: None,
                max: None,
            },
            text("3"),
            json!(3.0),
        ),
        (
            "n",
            FieldKind::Number {
                min: None,
                max: None,
            },
            text("2.5"),
            json!(2.5),
        ),
    ];
    for (name, kind, answer, value) in cases {
        let form = one_field(name, kind.clone());
        let result = answer_once(&form, answer.clone());
        let mut content = serde_json::Map::new();
        content.insert((*name).to_owned(), value.clone());
        assert_eq!(
            result,
            json!({"action": "accept", "content": content}),
            "{name} maps to {value}",
        );
    }
    // An optional empty text is left out of `content`.
    let form = Form {
        message: "m".to_owned(),
        fields: vec![Field {
            name: "nick".to_owned(),
            title: None,
            description: None,
            required: false,
            kind: FieldKind::Text {
                min: None,
                max: None,
            },
        }],
    };
    assert_eq!(
        answer_once(
            &form,
            Answered::Reply(Answer::Text {
                text: "".to_owned()
            })
        ),
        json!({"action": "accept", "content": {}}),
    );
    // An answer of the wrong kind declines the whole elicitation rather
    // than sending a partial accept.
    let form = one_field(
        "name",
        FieldKind::Text {
            min: None,
            max: None,
        },
    );
    assert_eq!(
        answer_once(
            &form,
            Answered::Reply(Answer::Confirmed { confirmed: true })
        ),
        json!({"action": "decline"}),
    );
}

#[test]
fn a_float_bound_at_the_i64_edge_is_exact_and_past_it_saturates() {
    // `-2^63` as a float is an exact `i64::MIN`: the cast keeps it, and only
    // a float below it saturates one past the bottom.
    let at_edge = json!({ "bound": -9223372036854775808.0_f64 });
    let edge = at_edge.as_object().expect("an object");
    assert_eq!(
        super::read_effective("f", edge, "bound", true),
        Ok(Some(i128::from(i64::MIN))),
        "a minimum at -2^63 is i64::MIN",
    );
    assert_eq!(
        super::read_effective("f", edge, "bound", false),
        Ok(Some(i128::from(i64::MIN))),
        "a maximum at -2^63 is i64::MIN",
    );
    // The next float below -2^63 is -2^63 - 2048; it saturates one below.
    let below = json!({ "bound": -9223372036854777856.0_f64 });
    let below = below.as_object().expect("an object");
    assert_eq!(
        super::read_effective("f", below, "bound", true),
        Ok(Some(i128::from(i64::MIN) - 1)),
        "a minimum below -2^63 saturates",
    );
    assert_eq!(
        super::read_effective("f", below, "bound", false),
        Ok(Some(i128::from(i64::MIN) - 1)),
        "a maximum below -2^63 saturates",
    );
    // A maximum of exactly -2^63 admits i64::MIN and nothing above it.
    let check = check_of(
        r#""count":{"type":"integer","maximum":-9223372036854775808.0}"#,
        r#"["count"]"#,
        "count",
    );
    assert!(check(&Answer::Text {
        text: "-9223372036854775808".to_owned()
    }));
    assert!(!check(&Answer::Text {
        text: "-9223372036854775807".to_owned()
    }));
}

#[test]
fn a_required_number_declines_what_is_not_finite() {
    let number = field_of(
        FieldKind::Number {
            min: None,
            max: None,
        },
        true,
    );
    for text in ["inf", "-inf", "NaN"] {
        assert_eq!(
            described(&number, &typed(text)),
            "decline",
            "{text} declines"
        );
    }
    assert_eq!(described(&number, &typed("2.5")), "value 2.5");
}
