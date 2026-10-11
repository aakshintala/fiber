//! `before_message` on steering and context (`docs/extensions.md`,
//! "Hooks"): a replaced steer is applied with `changed_by` and its context
//! follows its `steering_applied`; the conversation carries a prompt's
//! replacement and its context as their own user messages.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::events::TurnOutcome;
use contract::hook::{
    BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome, BeforeToolAnswer, BeforeToolCall,
    CheckAnswer, CheckOutcome, Context, Hooks, Replaced,
};
use contract::inbox::{Ack, Answer, Delivery, Message};
use contract::provider::Input;
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{ContentPart, Origin, Sender};
use contract::{CommandId, Envelope, ErrorCode, RequestId};
use fakes::{Deadline, Scripted};
use serde_json::json;

use support::{
    DEADLINE, Session, TestTool, allow, calls_reply, delivery, kinds, paris, read_until,
    reply_delivery, shell,
};

/// A scripted `before_message` fake: seen contents and a queue of answers.
struct FakeHooks {
    seen: Mutex<Vec<String>>,
    answers: Mutex<VecDeque<BeforeMessageAnswer>>,
}

impl FakeHooks {
    fn new() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            answers: Mutex::new(VecDeque::new()),
        }
    }

    fn answer(&self, answer: BeforeMessageAnswer) {
        self.answers.lock().unwrap().push_back(answer);
    }
}

fn passed() -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: Vec::new(),
        },
        notices: Vec::new(),
    }
}

fn replaced(text: &str, by: &str, context: Vec<(&str, &str)>) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: Some(Replaced {
                content: vec![ContentPart::Text {
                    text: text.to_owned(),
                }],
                changed_by: vec![by.to_owned()],
            }),
            context: context
                .into_iter()
                .map(|(extension, text)| Context {
                    extension: extension.to_owned(),
                    text: text.to_owned(),
                })
                .collect(),
        },
        notices: Vec::new(),
    }
}

fn refused(by: &str, reason: &str) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Refused(contract::hook::Refusal {
            extension: by.to_owned(),
            reason: reason.to_owned(),
        }),
        notices: Vec::new(),
    }
}

impl Hooks for FakeHooks {
    fn after_tool(
        &self,
        _call: &contract::hook::AfterToolCall<'_>,
    ) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(&self, _call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _call: &BeforeToolCall<'_>) -> CheckAnswer {
        CheckAnswer {
            outcome: CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }

    fn before_message(&self, message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        self.seen.lock().unwrap().push(
            message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::Image { .. } | ContentPart::Pdf(_) | ContentPart::Unknown => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        );
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(passed)
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

fn hook(session: &mut Session, hooks: Arc<FakeHooks>) {
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.hooks(hooks as Arc<dyn Hooks>));
}

fn rule(decision: RuleDecision, tool: &str, prefix: &str) -> Rule {
    Rule {
        decision,
        tool: tool.into(),
        prefix: prefix.into(),
        added: None,
        session_id: None,
    }
}

/// A driver's prompt with `id` saying `text`.
fn prompt(id: &str, text: &str) -> Delivery {
    Delivery::Prompt(
        Message {
            content: vec![ContentPart::Text { text: text.into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId(id.into())),
            },
        },
        support::ignore(),
    )
}

/// A driver's steering message with `id` saying `text`, answering on the
/// returned channel.
fn steer(id: &str, text: &str) -> (Delivery, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Delivery::Steer(
            Message {
                content: vec![ContentPart::Text { text: text.into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId(id.into())),
                },
            },
            Ack(Box::new(move |answer| {
                drop(tx.send(answer));
            })),
        ),
        rx,
    )
}

type Finished = mpsc::Receiver<(r#loop::Loop, Result<Option<TurnOutcome>, r#loop::Error>)>;

fn run_turn(session: &mut Session) -> Finished {
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    let turn = thread::spawn(move || {
        let outcome = looped.turn();
        let _sent = done.send((looped, outcome));
    });
    session.turn_thread = Some(turn.thread().id());
    finished
}

fn finish_turn(session: &mut Session, finished: &Finished) -> Option<TurnOutcome> {
    let (looped, outcome) = Deadline::after(DEADLINE)
        .recv(finished)
        .expect("the turn ended in time");
    session.looped = Some(looped);
    outcome.unwrap()
}

fn line<'a>(lines: &'a [Envelope], kind: &str) -> &'a Envelope {
    lines.iter().find(|line| line.kind == kind).unwrap()
}

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

#[test]
fn a_replaced_steer_applies_hooked_with_its_context_after_it() {
    let tool = shell(Some("npm publish"), None);
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn contract::tool::Tool>],
    );
    session.rules.set(StandingRules {
        global: vec![rule(RuleDecision::Ask, "shell", "npm publish")],
        project: Vec::new(),
    });
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(passed());
    hooks.answer(replaced(
        "also redacted",
        "fiber.test/redact",
        vec![("fiber.test/redact", "ctx")],
    ));
    hooks.answer(refused("fiber.test/guard", "no way"));
    hook(&mut session, Arc::clone(&hooks));

    let watcher = session.log.watch();
    session.inbox.send(delivery("go")).unwrap();
    let finished = run_turn(&mut session);
    let (_, until) = read_until(watcher, "a permission_requested line", |line| {
        line.kind == "permission_requested"
    });
    let request_id = RequestId(
        until.last().unwrap().payload["request_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let (first, first_rx) = steer("c_s1", "also this");
    session.inbox.send(first).unwrap();
    let (second, second_rx) = steer("c_s2", "no way");
    session.inbox.send(second).unwrap();
    session
        .inbox
        .send(reply_delivery(request_id, allow()))
        .unwrap();
    assert_eq!(
        finish_turn(&mut session, &finished),
        Some(TurnOutcome::Completed)
    );

    assert!(
        Deadline::after(DEADLINE)
            .recv(&first_rx)
            .expect("the replaced steer is answered")
            .is_ok()
    );
    let rejection = match Deadline::after(DEADLINE)
        .recv(&second_rx)
        .expect("the refused steer is answered")
    {
        Ok(_) => panic!("a refused steer is rejected"),
        Err(rejection) => rejection,
    };
    assert_eq!(rejection.code, ErrorCode::MessageRefused);
    assert_eq!(
        rejection.message,
        "Extension fiber.test/guard refused the message: no way"
    );
    assert_eq!(
        hooks.seen.lock().unwrap().clone(),
        vec!["go", "also this", "no way"]
    );

    let lines = session.lines();
    let queues: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "steering_queue")
        .collect();
    assert_eq!(queues.len(), 2, "a refused steer lists no queue");
    assert_eq!(
        queues[0].payload["messages"],
        json!([{
            "content": [{"type": "text", "text": "also redacted"}],
            "command_id": "c_s1",
            "source": "driver",
        }])
    );
    assert_eq!(queues[1].payload["messages"], json!([]));
    let applied = line(&lines, "steering_applied");
    assert_eq!(applied.payload["content"][0]["text"], "also redacted");
    assert_eq!(applied.payload["changed_by"], json!(["fiber.test/redact"]));
    assert_eq!(applied.payload["command_id"], "c_s1");
    assert_eq!(applied.payload["source"], "driver");
    let at = lines
        .iter()
        .position(|line| line.kind == "steering_applied")
        .unwrap();
    assert_eq!(lines[at + 1].kind, "context_added");
    assert_eq!(lines[at + 1].payload["hook"], "before_message");
    assert_eq!(lines[at + 1].payload["extension"], "fiber.test/redact");
    assert_eq!(lines[at + 1].payload["text"], "ctx");

    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "permission_requested",
            "steering_queue",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "steering_applied",
            "context_added",
            "steering_queue",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn the_request_after_a_replaced_prompt_carries_the_replacement_then_its_context() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("get_weather", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool.clone() as Arc<dyn contract::tool::Tool>],
    );
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer(replaced(
        "say [redacted]",
        "fiber.test/redact",
        vec![("fiber.test/redact", "ctx")],
    ));
    hook(&mut session, hooks);

    session.inbox.send(prompt("c_1", "say sk-123")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(
        kinds(&lines)
            .into_iter()
            .filter(|kind| kind == &"context_added")
            .count(),
        1
    );

    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    // `conversation[0]` is the opening message.
    assert_eq!(
        requests[1].conversation[1],
        user("say [redacted]"),
        "the replacement is what the model sees"
    );
    assert_eq!(
        requests[1].conversation[2],
        user("ctx"),
        "the context is its own user message after it"
    );
    let bytes = std::fs::read(session.dir.join("events.jsonl")).unwrap();
    assert!(
        !String::from_utf8(bytes).unwrap().contains("sk-123"),
        "the original is never logged"
    );
}
