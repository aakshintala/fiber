//! The `before_handoff` chain (`docs/extensions.md`, "The hook points",
//! "When several hooks share a point" and "When a hook fails"): what each
//! hook is shown, and what its answer decides.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use contract::ActionId;
use contract::ErrorCode;
use contract::events::{HandoffTrigger, ToolCallRequested};
use contract::hook::{BeforeHandoff, BeforeHandoffAnswer, BeforeHandoffOutcome, Hooks};
use contract::provider::{ImageRef, Input, PdfRef};
use contract::tool::Cancel;
use fakes::{Deadline, ProviderServer, Response};
use serde_json::{Value, json};

use super::super::SessionExtensions;
use crate::test_support::{Home, bounded};

/// A cancel that never fires: the chain runs every hook.
struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

const NEVER: Never = Never;

/// A cancel the test fires.
struct Flag {
    fired: AtomicBool,
}

impl Cancel for Flag {
    fn is_cancelled(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// A hook whose `run` is `body`.
fn hook(on_failure: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"before_handoff\", {{ timeout = 1000, on_failure = \"{on_failure}\",\n\
           run = function(call) {body} end }})\n"
    )
}

/// A hook in `phase` whose `run` is `body`.
fn phased_hook(phase: &str, on_failure: &str, body: &str) -> String {
    format!(
        "fiber.hook(\"before_handoff\", {{ phase = \"{phase}\", timeout = 1000, on_failure = \"{on_failure}\",\n\
           run = function(call) {body} end }})\n"
    )
}

/// Runs the `before_handoff` chain on its own thread under a deadline, so a
/// hook that never answers fails the test instead of hanging it.
#[track_caller]
fn dispatch(
    session: &Arc<SessionExtensions>,
    trigger: HandoffTrigger,
    instructions: Option<String>,
    conversation: Vec<Input>,
) -> BeforeHandoffAnswer {
    let session = Arc::clone(session);
    bounded(move || {
        let instructions = instructions.as_deref();
        session.before_handoff(&BeforeHandoff {
            trigger,
            instructions,
            conversation: &conversation,
            cancel: &NEVER,
        })
    })
}

fn note(answer: &BeforeHandoffAnswer) -> Option<(&str, &str)> {
    match &answer.outcome {
        BeforeHandoffOutcome::Note { text, extension } => Some((text, extension)),
        BeforeHandoffOutcome::NoNote | BeforeHandoffOutcome::Failed(_) => None,
    }
}

#[test]
fn one_hook_writes_the_note_and_is_shown_the_handoff() {
    let home = Home::new();
    home.install(
        "echo",
        &hook("non-blocking", r#"return { note = json.encode(call) }"#),
    );
    let session = home.load(&[]);
    let conversation = vec![Input::User {
        text: "Fix the bug.".into(),
        images: Vec::new(),
    }];
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation);
    let (text, extension) = note(&answer).expect("a hook wrote a note");
    assert_eq!(extension, "fiber.test/echo");
    let shown: Value = serde_json::from_str(text).unwrap();
    assert_eq!(shown["trigger"], json!("auto"));
    assert!(shown.get("instructions").is_none(), "{shown}");
    assert!(shown.get("note").is_none(), "{shown}");
    assert_eq!(
        shown["messages"],
        json!([{"role": "user", "content": [{"type": "text", "text": "Fix the bug."}]}])
    );
    assert!(answer.notices.is_empty());
    // With instructions, the hook is shown them.
    let answer = dispatch(
        &session,
        HandoffTrigger::Person,
        Some("Focus on the parser.".into()),
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
    );
    let (text, _) = note(&answer).expect("a hook wrote a note");
    let shown: Value = serde_json::from_str(text).unwrap();
    assert_eq!(shown["trigger"], json!("person"));
    assert_eq!(shown["instructions"], json!("Focus on the parser."));
}

#[test]
fn each_hook_sees_the_note_before_it_and_the_last_note_wins() {
    let home = Home::new();
    home.install("one", &hook("non-blocking", r#"return { note = "first" }"#));
    home.install(
        "two",
        &hook(
            "non-blocking",
            r#"if seen == nil then seen = true return { note = "second saw " .. tostring(call.note) } else return nil end"#,
        ),
    );
    let session = home.load(&[]);
    let conversation = vec![Input::User {
        text: "hi".into(),
        images: Vec::new(),
    }];
    // Both return a note: the second wins and names its extension.
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation.clone());
    assert_eq!(note(&answer), Some(("second saw first", "fiber.test/two")));
    // The second returns nothing: the first note stands and names the first.
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation);
    assert_eq!(note(&answer), Some(("first", "fiber.test/one")));
}

#[test]
fn an_empty_note_changes_nothing() {
    // Both hooks share one extension, so the blank the first sets is the
    // one the second returns: globals do not cross extensions.
    let home = Home::new();
    home.install(
        "pair",
        "fiber.hook(\"before_handoff\", { timeout = 1000, on_failure = \"non-blocking\",\n\
           run = function(call)\n\
             if fired == nil then fired = true blank = \"\" return nil\n\
             else blank = \" \\n\" return { note = \"first\" } end\n\
           end })\n\
         fiber.hook(\"before_handoff\", { timeout = 1000, on_failure = \"non-blocking\",\n\
           run = function(call) return { note = blank } end })\n",
    );
    let session = home.load(&[]);
    let conversation = || {
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }]
    };
    // An empty note alone writes nothing.
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation());
    assert_eq!(answer.outcome, BeforeHandoffOutcome::NoNote);
    // A blank note after an earlier note leaves it standing.
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation());
    assert_eq!(note(&answer), Some(("first", "fiber.test/pair")));
}

#[test]
fn nil_and_an_empty_table_return_nothing() {
    let home = Home::new();
    home.install("nil", &hook("non-blocking", r#"return nil"#));
    home.install("empty", &hook("non-blocking", r#"return {}"#));
    let session = home.load(&[]);
    let answer = dispatch(
        &session,
        HandoffTrigger::Auto,
        None,
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
    );
    assert_eq!(answer.outcome, BeforeHandoffOutcome::NoNote);
    assert!(answer.notices.is_empty());
}

#[test]
fn bad_returns_fail_the_hook() {
    for (shape, body) in [
        ("a string", r#"return "x""#),
        ("a number", r#"return 5"#),
        ("a boolean", r#"return true"#),
        ("an array", r#"return { "x" }"#),
        ("another key", r#"return { other = "x" }"#),
        ("a note of another kind", r#"return { note = 5 }"#),
    ] {
        for on_failure in ["blocking", "non-blocking"] {
            let home = Home::new();
            home.install("aaa", &hook(on_failure, body));
            home.install("zzz", &hook("non-blocking", r#"return { note = "good" }"#));
            let session = home.load(&[]);
            let answer = dispatch(
                &session,
                HandoffTrigger::Auto,
                None,
                vec![Input::User {
                    text: "hi".into(),
                    images: Vec::new(),
                }],
            );
            if on_failure == "blocking" {
                let BeforeHandoffOutcome::Failed(failure) = &answer.outcome else {
                    panic!(
                        "{shape}: a blocking failure ends the chain, not {:?}",
                        answer.outcome
                    )
                };
                assert_eq!(failure.extension, "fiber.test/aaa", "{shape}");
                assert!(!failure.message.is_empty(), "{shape}");
                assert!(answer.notices.is_empty(), "{shape}");
            } else {
                assert_eq!(
                    note(&answer),
                    Some(("good", "fiber.test/zzz")),
                    "{shape}: the chain goes on past a non-blocking failure"
                );
                assert_eq!(answer.notices.len(), 1, "{shape}: {:?}", answer.notices);
                let notice = &answer.notices[0];
                assert_eq!(notice.code, ErrorCode::HookFailed);
                assert_eq!(notice.extension.as_deref(), Some("fiber.test/aaa"));
                assert!(
                    notice.message.starts_with(
                        "The `before_handoff` hook failed, so its change was dropped: "
                    ),
                    "{shape}: {}",
                    notice.message
                );
                assert!(notice.message.contains("fiber.test/aaa"), "{shape}");
            }
        }
    }
}

#[test]
fn a_json_null_returns_nothing_and_a_function_fails_the_hook() {
    let home = Home::new();
    home.install(
        "null",
        &hook("non-blocking", r#"return json.decode("[null]")[1]"#),
    );
    let session = home.load(&[]);
    let answer = dispatch(
        &session,
        HandoffTrigger::Auto,
        None,
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
    );
    assert_eq!(answer.outcome, BeforeHandoffOutcome::NoNote);
    for on_failure in ["blocking", "non-blocking"] {
        let home = Home::new();
        home.install(
            "fun",
            &hook(on_failure, r#"return { note = function() end }"#),
        );
        let session = home.load(&[]);
        let answer = dispatch(
            &session,
            HandoffTrigger::Auto,
            None,
            vec![Input::User {
                text: "hi".into(),
                images: Vec::new(),
            }],
        );
        if on_failure == "blocking" {
            let BeforeHandoffOutcome::Failed(failure) = &answer.outcome else {
                panic!(
                    "a blocking failure ends the chain, not {:?}",
                    answer.outcome
                )
            };
            assert_eq!(failure.extension, "fiber.test/fun");
        } else {
            assert_eq!(answer.outcome, BeforeHandoffOutcome::NoNote);
            assert_eq!(answer.notices.len(), 1);
        }
    }
}

#[test]
fn a_blocking_failure_drops_an_earlier_note_and_runs_no_later_hook() {
    let home = Home::new();
    home.install("one", &hook("non-blocking", r#"return { note = "first" }"#));
    home.install("two", &hook("blocking", r#"error("boom")"#));
    home.install(
        "three",
        &hook("non-blocking", r#"return { note = "third" }"#),
    );
    let session = home.load(&[]);
    let answer = dispatch(
        &session,
        HandoffTrigger::Auto,
        None,
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
    );
    // The failure carries no note: the earlier note is dropped, and the
    // later hook, which would have returned one, never ran.
    let BeforeHandoffOutcome::Failed(failure) = &answer.outcome else {
        panic!(
            "a blocking failure ends the chain, not {:?}",
            answer.outcome
        )
    };
    assert_eq!(failure.extension, "fiber.test/two");
    assert!(failure.message.contains("boom"), "{}", failure.message);
    assert!(answer.notices.is_empty());
}

#[test]
fn sanitize_hooks_run_before_transform_ones() {
    let home = Home::new();
    home.install(
        "zzz",
        &phased_hook("sanitize", "non-blocking", r#"return { note = "san" }"#),
    );
    home.install(
        "aaa",
        &phased_hook(
            "transform",
            "non-blocking",
            r#"return { note = call.note .. "+tra" }"#,
        ),
    );
    let session = home.load(&[]);
    let answer = dispatch(
        &session,
        HandoffTrigger::Auto,
        None,
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
    );
    assert_eq!(note(&answer), Some(("san+tra", "fiber.test/aaa")));
}

#[test]
fn hooks_order_reorders_this_point_and_leaves_after_tool_alone() {
    let note_hook = |text: &str| {
        format!(
            "fiber.hook(\"before_handoff\", {{ timeout = 1000, on_failure = \"non-blocking\",\n\
               run = function(call) return {{ note = \"{text}\" }} end }})\n\
             fiber.hook(\"after_tool\", {{ timeout = 1000, on_failure = \"non-blocking\",\n\
               run = function(call) return {{ content = call.content .. \"|{text}\" }} end }})\n"
        )
    };
    let home = Home::new();
    home.install("mmm", &note_hook("mmm"));
    home.install("zzz", &note_hook("zzz"));
    let conversation = || {
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }]
    };
    // By name, `mmm` runs first and `zzz`'s note wins.
    let session = home.load(&[]);
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation());
    assert_eq!(note(&answer), Some(("zzz", "fiber.test/zzz")));
    // `hooks.order.before_handoff` runs `zzz` first, so `mmm` wins.
    let session = home.load(&[r#"hooks.order.before_handoff=["fiber.test/zzz"]"#]);
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation());
    assert_eq!(note(&answer), Some(("mmm", "fiber.test/mmm")));
    // The order names this point only: `after_tool` still runs by name.
    let changed = bounded({
        let session = Arc::clone(&session);
        move || {
            use contract::events::CallStatus;
            use contract::hook::{AfterToolCall, AfterToolOutcome};
            let arguments = serde_json::Map::new();
            let answer = session.after_tool(&AfterToolCall {
                tool: "read",
                arguments: &arguments,
                status: CallStatus::Completed,
                content: "c",
                details: None,
                process: None,
                spilled: None,

                not_regular: false,
                delivery: None,
            });
            let AfterToolOutcome::Changed { content, .. } = answer.outcome else {
                panic!(
                    "the tagging hooks change the content, not {:?}",
                    answer.outcome
                )
            };
            content
        }
    });
    assert_eq!(changed, Some("c|mmm|zzz".to_owned()));
}

#[test]
fn a_check_hook_is_not_registered() {
    let home = Home::new();
    home.install(
        "checking",
        "fiber.hook(\"before_handoff\", { phase = \"check\", timeout = 1000, on_failure = \"non-blocking\",\n\
           run = function(call) return { note = \"x\" } end })\n",
    );
    let session = home.load(&[]);
    assert!(
        session.notices().iter().any(|notice| notice.message
            == "`before_handoff` hook not registered: a `check` hook exists only at \
                `before_message`, `before_tool` and `before_model_call`"),
        "{:?}",
        session.notices()
    );
    let answer = dispatch(
        &session,
        HandoffTrigger::Auto,
        None,
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
    );
    assert_eq!(answer.outcome, BeforeHandoffOutcome::NoNote);
}

#[test]
fn tool_search_results_render_as_the_tools_answer_with_or_without_a_call() {
    let home = Home::new();
    home.install(
        "echo",
        &hook(
            "non-blocking",
            r#"return { note = json.encode(call.messages) }"#,
        ),
    );
    let session = home.load(&[]);
    let call = Input::ToolCall {
        action_id: ActionId("a_1".into()),
        model: "fake/m".into(),
        call: ToolCallRequested {
            name: "tool_search".into(),
            arguments: json!({"query":"q"}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        },
    };
    let result = Input::ToolSearchResult {
        action_id: ActionId("a_1".into()),
        loaded: vec!["a".into(), "b".into()],
    };
    for (conversation, expected) in [
        (
            vec![call, result.clone()],
            json!([
                {"role":"assistant", "content":[{"type":"text", "text":"Called the tool tool_search with arguments {\"query\":\"q\"}"}]},
                {"role":"user", "content":[{"type":"text", "text":"The tool tool_search returned:\nLoaded: a, b."}]}
            ]),
        ),
        (
            vec![result],
            json!([
                {"role":"user", "content":[{"type":"text", "text":"A tool returned:\nLoaded: a, b."}]}
            ]),
        ),
    ] {
        let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation);
        let (text, _) = note(&answer).expect("a hook wrote a note");
        assert_eq!(serde_json::from_str::<Value>(text).unwrap(), expected);
    }
}

#[test]
fn the_conversation_renders_as_neutral_messages() {
    let home = Home::new();
    home.install(
        "echo",
        &hook(
            "non-blocking",
            r#"return { note = json.encode(call.messages) }"#,
        ),
    );
    let session = home.load(&[]);
    let read = ActionId("a_read".into());
    let write = ActionId("a_write".into());
    let orphan = ActionId("a_orphan".into());
    let image = |path: &str| ImageRef {
        path: path.into(),
        mime_type: "image/png".into(),
        width: 4,
        height: 4,
    };
    let conversation = vec![
        Input::User {
            text: "Fix the bug.".into(),
            images: vec![image("artifacts/i.png")],
        },
        Input::User {
            text: String::new(),
            images: vec![image("artifacts/j.png")],
        },
        Input::Assistant {
            model: "fake/m".into(),
            text: "On it.".into(),
            provider_item: None,
        },
        Input::Assistant {
            model: "fake/m".into(),
            text: String::new(),
            provider_item: None,
        },
        Input::Reasoning {
            model: "fake/m".into(),
            text: "Pondering.".into(),
            provider_item: None,
        },
        Input::ToolCall {
            action_id: read.clone(),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.rs"}),
                provider_id: None,
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "fake/m".into(),
        },
        Input::ToolResult {
            action_id: read,
            text: "fn main() {}".into(),
            is_error: false,
            images: vec![image("artifacts/k.png")],
            pdfs: Vec::new(),
        },
        Input::ToolCall {
            action_id: write.clone(),
            call: ToolCallRequested {
                name: "write".into(),
                arguments: json!({"path": "b.rs"}),
                provider_id: None,
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "fake/m".into(),
        },
        Input::ToolResult {
            action_id: write,
            text: "denied".into(),
            is_error: true,
            images: Vec::new(),
            pdfs: Vec::new(),
        },
        Input::ToolResult {
            action_id: orphan,
            text: "loose".into(),
            is_error: false,
            images: Vec::new(),
            pdfs: Vec::new(),
        },
    ];
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation);
    let (text, _) = note(&answer).expect("a hook wrote a note");
    let messages: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        messages,
        json!([
            {"role": "user", "content": [
                {"type": "text", "text": "Fix the bug."},
                {"type": "image", "path": "artifacts/i.png", "mime_type": "image/png", "width": 4, "height": 4},
            ]},
            {"role": "user", "content": [
                {"type": "image", "path": "artifacts/j.png", "mime_type": "image/png", "width": 4, "height": 4},
            ]},
            {"role": "assistant", "content": [
                {"type": "text", "text": "On it."},
            ]},
            {"role": "assistant", "content": [
                {"type": "text", "text": "Called the tool read with arguments {\"path\":\"a.rs\"}"},
            ]},
            {"role": "user", "content": [
                {"type": "text", "text": "The tool read returned:\nfn main() {}"},
                {"type": "image", "path": "artifacts/k.png", "mime_type": "image/png", "width": 4, "height": 4},
            ]},
            {"role": "assistant", "content": [
                {"type": "text", "text": "Called the tool write with arguments {\"path\":\"b.rs\"}"},
            ]},
            {"role": "user", "content": [
                {"type": "text", "text": "The tool write failed:\ndenied"},
            ]},
            {"role": "user", "content": [
                {"type": "text", "text": "A tool returned:\nloose"},
            ]},
        ])
    );
}

#[test]
fn a_conversation_over_the_memory_cap_fails_the_hook() {
    let big = "x".repeat(2 * 1024 * 1024);
    let conversation = || {
        vec![Input::User {
            text: big.clone(),
            images: Vec::new(),
        }]
    };
    // At the default cap the conversion runs out of memory: a `blocking`
    // hook fails, and a `non-blocking` one drops its change and goes on.
    let home = Home::new();
    home.install("small", &hook("blocking", r#"return { note = "small" }"#));
    let session = home.load(&[]);
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation());
    let BeforeHandoffOutcome::Failed(failure) = &answer.outcome else {
        panic!("an over-cap hook fails, not {:?}", answer.outcome)
    };
    assert_eq!(failure.extension, "fiber.test/small");
    // The failure is the hook's error, and the VM still answers later
    // calls: a small conversation returns its note.
    let answer = dispatch(
        &session,
        HandoffTrigger::Auto,
        None,
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }],
    );
    assert_eq!(note(&answer), Some(("small", "fiber.test/small")));
    let home = Home::new();
    home.install(
        "soft",
        &hook("non-blocking", r#"return { note = "small" }"#),
    );
    let session = home.load(&[]);
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation());
    assert_eq!(answer.outcome, BeforeHandoffOutcome::NoNote);
    assert_eq!(answer.notices.len(), 1);
    assert_eq!(
        answer.notices[0].extension.as_deref(),
        Some("fiber.test/soft")
    );
    // Under a raised cap the same hook returns its note.
    let raised = Home::new();
    raised.install_with_memory("big", &hook("blocking", r#"return { note = "big" }"#), 8);
    let session = raised.load(&[]);
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation());
    assert_eq!(note(&answer), Some(("big", "fiber.test/big")));
}

/// How long a test waits for the hook to reach its gate before failing.
const GATE_WAIT: Duration = Duration::from_secs(10);

#[test]
fn a_cancel_between_two_hooks_runs_no_later_hook() {
    let server =
        ProviderServer::start([Response::status(200, "{}"), Response::status(200, "{}")]).unwrap();
    let url = server.url();
    server.hold();
    // Every request the hook sends reaches the server: the test owns
    // both waits, each under a deadline.
    let home = Home::new();
    home.install(
        "gate",
        &format!(
            "fiber.hook(\"before_handoff\", {{ timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(call)\n\
                 host.http({{ url = \"{url}/gate\" }})\n\
                 return {{ note = \"first:\" .. tostring(seen2 or 0) }}\n\
               end }})\n\
             fiber.hook(\"before_handoff\", {{ timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(call)\n\
                 seen2 = (seen2 or 0) + 1\n\
                 return {{ note = tostring(call.note) .. \"+second\" }}\n\
               end }})\n"
        ),
    );
    let session = home.load(&[]);
    let cancel: &'static Flag = Box::leak(Box::new(Flag {
        fired: AtomicBool::new(false),
    }));
    let conversation = || {
        vec![Input::User {
            text: "hi".into(),
            images: Vec::new(),
        }]
    };
    // The first run: the cancel turns on while the first hook waits at its
    // gate. The hook returns its note, but the chain stops between hooks.
    let (done_tx, done_rx) = mpsc::channel();
    let first = Arc::clone(&session);
    std::thread::spawn(move || {
        let answer = first.before_handoff(&BeforeHandoff {
            trigger: HandoffTrigger::Auto,
            instructions: None,
            conversation: &conversation(),
            cancel,
        });
        let _sent = done_tx.send(answer);
    });
    // The hook signals it reached its gate with its recorded request;
    // anything else, such as an early answer, fails the test.
    assert!(
        server.await_requests(1, Deadline::after(GATE_WAIT)),
        "waited for the hook to reach its gate"
    );
    assert!(
        done_rx.try_recv().is_err(),
        "the chain answered before its hook reached the gate"
    );
    cancel.fired.store(true, Ordering::SeqCst);
    server.release();
    let answer: BeforeHandoffAnswer = Deadline::after(GATE_WAIT)
        .recv(&done_rx)
        .expect("waited for the chain");
    assert_eq!(answer.outcome, BeforeHandoffOutcome::NoNote);
    assert!(answer.notices.is_empty());
    // The second run, with the cancel off: the first hook reports how often
    // the second ran. `first:0` proves it never ran above.
    cancel.fired.store(false, Ordering::SeqCst);
    let (done_tx, done_rx) = mpsc::channel();
    let second = Arc::clone(&session);
    std::thread::spawn(move || {
        let answer = second.before_handoff(&BeforeHandoff {
            trigger: HandoffTrigger::Auto,
            instructions: None,
            conversation: &[Input::User {
                text: "hi".into(),
                images: Vec::new(),
            }],
            cancel,
        });
        let _sent = done_tx.send(answer);
    });
    assert!(
        server.await_requests(2, Deadline::after(GATE_WAIT)),
        "waited for the second hook to reach its gate"
    );
    let answer: BeforeHandoffAnswer = Deadline::after(GATE_WAIT)
        .recv(&done_rx)
        .expect("waited for the chain");
    assert_eq!(note(&answer), Some(("first:0+second", "fiber.test/gate")));
}

#[test]
fn a_cancel_before_the_call_still_runs_the_first_hook() {
    // The chain checks the cancel between hooks only (`at > 0`): a
    // cancel that fired before the call still runs the first hook.
    // Under `>=` the chain would return `NoNote` before it.
    let home = Home::new();
    home.install("one", &hook("non-blocking", r#"return { note = "first" }"#));
    let session = home.load(&[]);
    let answer = bounded({
        let session = Arc::clone(&session);
        move || {
            let cancel = Flag {
                fired: AtomicBool::new(true),
            };
            let conversation = vec![Input::User {
                text: "hi".into(),
                images: Vec::new(),
            }];
            session.before_handoff(&BeforeHandoff {
                trigger: HandoffTrigger::Auto,
                instructions: None,
                conversation: &conversation,
                cancel: &cancel,
            })
        }
    });
    assert_eq!(note(&answer), Some(("first", "fiber.test/one")));
}

#[test]
fn pdf_parts_render_with_and_without_pages() {
    let home = Home::new();
    home.install(
        "echo",
        &hook(
            "non-blocking",
            r#"return { note = json.encode(call.messages) }"#,
        ),
    );
    let session = home.load(&[]);
    let action = ActionId("a_read".into());
    let image = ImageRef {
        path: "artifacts/i.png".into(),
        mime_type: "image/png".into(),
        width: 4,
        height: 4,
    };
    let conversation = vec![
        Input::ToolCall {
            action_id: action.clone(),
            call: ToolCallRequested {
                name: "read".into(),
                arguments: json!({"path": "a.rs"}),
                provider_id: None,
                repair: None,
                ran_by: None,
                provider_item: None,
            },
            model: "fake/m".into(),
        },
        Input::ToolResult {
            action_id: action,
            text: "body".into(),
            is_error: false,
            images: Vec::new(),
            pdfs: vec![
                PdfRef {
                    path: "artifacts/a.pdf".into(),
                    page_count: 2,
                    pages: Some(vec![image.clone()]),
                },
                PdfRef {
                    path: "artifacts/b.pdf".into(),
                    page_count: 3,
                    pages: None,
                },
            ],
        },
    ];
    let answer = dispatch(&session, HandoffTrigger::Auto, None, conversation);
    let (text, _) = note(&answer).expect("a hook wrote a note");
    let messages: Value = serde_json::from_str(text).unwrap();
    assert_eq!(
        messages,
        json!([
            {"role": "assistant", "content": [
                {"type": "text", "text": "Called the tool read with arguments {\"path\":\"a.rs\"}"},
            ]},
            {"role": "user", "content": [
                {"type": "text", "text": "The tool read returned:\nbody"},
                {"type": "pdf", "path": "artifacts/a.pdf", "page_count": 2, "pages": [
                    {"type": "image", "path": "artifacts/i.png", "mime_type": "image/png", "width": 4, "height": 4},
                ]},
                {"type": "pdf", "path": "artifacts/b.pdf", "page_count": 3},
            ]},
        ])
    );
}
