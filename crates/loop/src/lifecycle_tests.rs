//! `turn_start`, `before_model_call` and `turn_end` in the loop: what each
//! hook is shown, and how its answer reaches the turn, the request or the
//! next step.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::emit::Emit;
use contract::events::{InputItem, TextDelta, ToolCallRequested, TurnOutcome, TurnStarted};
use contract::hook::{
    AfterToolAnswer, AfterToolOutcome, BeforeMessage, BeforeMessageAnswer, BeforeMessageOutcome,
    BeforeToolAnswer, BeforeToolCall, BeforeToolOutcome, CheckAnswer, CheckOutcome, Context,
    ContextAnswer, ContextOutcome, Hooks,
};
use contract::inbox::{Ack, Answer, Delivery, Message};
use contract::provider::{Delta, ReplyAction};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender, Usage};
use contract::tool::{Bound, Cancel, Effects, EffectsError, Output, Tool};
use contract::{CommandId, Envelope, ErrorCode, SessionId, TurnId};
use fakes::{Deadline, NoRules};
use log::Log;

use crate::{Loop, Model};

/// One hang bound for every wait: the turn runs on a thread of its own, so
/// a stuck hook fails the test instead of hanging it.
const TURN_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Default)]
struct StateCases {
    before: Option<BeforeToolOutcome>,
    check: Option<CheckOutcome>,
    after: Option<AfterToolOutcome>,
    replacement_only: bool,
    reject_carried: bool,
    supersede_note: bool,
}

struct StateHooks {
    cases: StateCases,
    inner: Arc<FakeHooks>,
    watcher: Mutex<log::Watcher>,
    trace: Mutex<Vec<String>>,
    pending: Mutex<Option<(&'static str, bool)>>,
    outbox: Mutex<Vec<contract::events::Event>>,
}

fn state_attached(world: &mut World, inner: Arc<FakeHooks>, cases: StateCases) -> Arc<StateHooks> {
    let hooks = Arc::new(StateHooks {
        cases,
        inner,
        watcher: Mutex::new(world.watcher.take().unwrap()),
        trace: Mutex::new(Vec::new()),
        pending: Mutex::new(None),
        outbox: Mutex::new(Vec::new()),
    });
    world.looped = Some(world.looped.take().unwrap().hooks(hooks.clone()));
    hooks
}

impl StateHooks {
    fn flush(&self) {
        let mut watcher = self.watcher.lock().unwrap();
        while let Some(line) = watcher.try_recv().unwrap() {
            self.trace
                .lock()
                .unwrap()
                .push(format!("write:{}", line.kind));
        }
    }

    fn called(&self, point: &'static str) {
        self.flush();
        assert!(
            self.pending
                .lock()
                .unwrap()
                .replace((point, false))
                .is_none(),
            "previous hook was not drained"
        );
        self.trace.lock().unwrap().push(point.into());
        self.outbox
            .lock()
            .unwrap()
            .push(state_event(&format!("timer-{point}")));
        if self.cases.supersede_note
            && point == "session_start"
            && self
                .trace
                .lock()
                .unwrap()
                .iter()
                .any(|entry| entry == "before_handoff")
        {
            let mut event = state_event("before_handoff");
            if let contract::events::Event::ExtensionStateSet(set) = &mut event {
                set.value = serde_json::json!("B");
            }
            self.outbox.lock().unwrap().push(event);
        }
    }
}

macro_rules! state_forward {
    ($method:ident, $argument:ident, $answer:ident) => {
        fn $method(&self, argument: &contract::hook::$argument<'_>) -> contract::hook::$answer {
            self.called(stringify!($method));
            let mut answer = self.inner.$method(argument);
            answer.notices.push(state_notice());
            answer
        }
    };
}

macro_rules! state_context_forward {
    ($method:ident, $argument:ident) => {
        fn $method(&self, argument: &contract::hook::$argument<'_>) -> ContextAnswer {
            self.called(stringify!($method));
            let mut answer = self.inner.$method(argument);
            if let ContextOutcome::Added(contexts) = &mut answer.outcome {
                for context in contexts {
                    context.state.push(carried_state(stringify!($method)));
                }
            }
            answer.notices.push(state_notice());
            answer
        }
    };
}

impl Hooks for StateHooks {
    fn before_message(&self, argument: &contract::hook::BeforeMessage<'_>) -> BeforeMessageAnswer {
        self.called("before_message");
        let mut answer = self.inner.before_message(argument);
        if let BeforeMessageOutcome::Passed { context, .. } = &mut answer.outcome {
            for added in context {
                added.state.push(carried_state("before_message"));
            }
        }
        answer.notices.push(state_notice());
        answer
    }
    state_context_forward!(session_start, SessionStart);
    state_context_forward!(turn_start, TurnStart);
    state_context_forward!(turn_end, TurnEnd);
    state_forward!(before_model_call, BeforeModelCall, CheckAnswer);
    fn check_tool(&self, call: &BeforeToolCall<'_>) -> CheckAnswer {
        self.called("check_tool");
        let mut answer = self.inner.check_tool(call);
        answer.notices.push(state_notice());
        if let Some(outcome) = &self.cases.check {
            answer.outcome = outcome.clone();
        }
        answer
    }

    fn before_tool(&self, call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        self.called("before_tool");
        BeforeToolAnswer {
            outcome: self
                .cases
                .before
                .clone()
                .unwrap_or_else(|| BeforeToolOutcome::Rewritten {
                    arguments: call.arguments.clone(),
                    changed_by: vec!["state".into()],
                }),
            notices: vec![state_notice()],
        }
    }
    fn after_tool(&self, _: &contract::hook::AfterToolCall<'_>) -> AfterToolAnswer {
        self.called("after_tool");
        AfterToolAnswer {
            outcome: self
                .cases
                .after
                .clone()
                .unwrap_or_else(|| AfterToolOutcome::Changed {
                    content: Some("changed".into()),
                    details: None,
                    artifact: None,
                }),
            changed_by: vec!["state".into()],
            notices: vec![state_notice()],
        }
    }
    fn before_handoff(
        &self,
        _: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        self.called("before_handoff");
        contract::hook::BeforeHandoffAnswer {
            outcome: contract::hook::BeforeHandoffOutcome::Note {
                text: "note".into(),
                extension: "state".into(),
            },
            notices: vec![state_notice()],
        }
    }
    fn deliver_to(&self, _: mpsc::Sender<Delivery>) {}
    fn take_change_state(&self) -> Vec<contract::hook::StateLine> {
        self.flush();
        self.trace.lock().unwrap().push("take".into());
        let mut pending = self.pending.lock().unwrap();
        let (point, taken) = pending.as_mut().expect("a hook returned");
        assert!(!*taken, "taken once");
        *taken = true;
        if matches!(*point, "session_start" | "turn_start" | "turn_end")
            || (*point == "before_message" && !self.cases.replacement_only)
        {
            Vec::new()
        } else {
            vec![carried_state(point)]
        }
    }
    fn take_state_lines(&self) -> Vec<contract::events::Event> {
        self.flush();
        self.trace.lock().unwrap().push("drain".into());
        if let Some((point, taken)) = self.pending.lock().unwrap().take() {
            assert!(
                taken || matches!(point, "check_tool" | "before_model_call"),
                "take precedes drain: {point}"
            );
        }
        std::mem::take(&mut *self.outbox.lock().unwrap())
    }
    fn admit_state_lines(
        &self,
        lines: Vec<contract::hook::StateLine>,
    ) -> Vec<contract::events::Event> {
        self.flush();
        self.trace.lock().unwrap().push("admit".into());
        if self.cases.reject_carried {
            Vec::new()
        } else {
            lines
                .into_iter()
                .filter(|line| !self.cases.supersede_note || line.key != "before_handoff")
                .map(|line| line.into_event())
                .collect()
        }
    }
}

fn carried_state(key: &str) -> contract::hook::StateLine {
    contract::hook::StateLine {
        value: Some((serde_json::json!(true), contract::events::OnFork::AtPoint)),
        extension: "state".into(),
        key: key.into(),
        landing: 1,
    }
}

fn state_notice() -> contract::events::Notice {
    contract::events::Notice {
        code: ErrorCode::HookFailed,
        message: "state hook notice".into(),
        extension: Some("state".into()),
    }
}

fn state_event(key: &str) -> contract::events::Event {
    contract::events::Event::ExtensionStateSet(contract::events::ExtensionStateSet {
        extension: "state".into(),
        key: key.into(),
        value: serde_json::json!(true),
        on_fork: contract::events::OnFork::AtPoint,
    })
}

#[test]
fn state_lines_take_follows_every_hook_call() {
    let inner = Arc::new(FakeHooks::new());
    inner.answer_before_message(passed_message_with_context(vec![(
        "state",
        "message context",
    )]));
    inner.answer_session_start(added(vec![("state", "start context")]));
    inner.answer_session_start(added(vec![("state", "handoff context")]));
    inner.answer_turn_start(added(vec![("state", "turn context")]));
    inner.answer_turn_end(added(vec![("state", "continue")]));
    let mut world = started_with_tools(
        vec![
            with_tokens(
                tool_call_reply("", "probe", serde_json::json!({}), 0.0),
                999,
                0,
            ),
            fakes::Scripted::text("first"),
            fakes::Scripted::text("done"),
        ],
        None,
        vec![Arc::new(Probe)],
    );
    let hooks = state_attached(&mut world, inner, StateCases::default());
    world.looped = Some(world.looped.take().unwrap().hooks(hooks.clone()).handoff(
        crate::HandoffSettings {
            tokens: 1000,
            nudge: false,
            ..Default::default()
        },
    ));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("state", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv_or_fail(&answered, "state prompt accepted")
            .is_ok()
    );
    hooks.flush();
    let trace = hooks.trace.lock().unwrap();
    for point in [
        "before_message",
        "session_start",
        "turn_start",
        "before_tool",
        "check_tool",
        "after_tool",
        "before_handoff",
        "before_model_call",
        "turn_end",
    ] {
        let positions: Vec<_> = trace
            .iter()
            .enumerate()
            .filter_map(|(index, item)| (item == point).then_some(index))
            .collect();
        assert!(!positions.is_empty(), "{point}: {trace:?}");
        for index in positions {
            let expected = if matches!(point, "check_tool" | "before_model_call") {
                vec!["drain"]
            } else {
                vec!["take", "drain"]
            };
            assert_eq!(
                trace[index + 1..index + 1 + expected.len()],
                expected,
                "{point}: {trace:?}"
            );
        }
    }
    let all = lines(&world);
    for (index, line) in all.iter().enumerate() {
        if line.kind == "extension_state_set" {
            match line.payload["key"].as_str().unwrap() {
                "before_message" | "session_start" | "turn_start" => {
                    assert_eq!(all[index + 1].kind, "context_added")
                }
                "turn_end" => assert_eq!(all[index + 1].kind, "steering_applied"),
                "before_tool" => assert_eq!(all[index + 1].kind, "tool_call_started"),
                "after_tool" => assert_eq!(all[index + 1].kind, "tool_call_completed"),
                "before_handoff" => assert_eq!(all[index + 1].kind, "handoff_completed"),
                key => assert!(key.starts_with("timer-")),
            }
        }
    }
}

fn state_written(world: &World, key: &str) -> Vec<Envelope> {
    lines(world)
        .into_iter()
        .filter(|line| line.kind == "extension_state_set" && line.payload["key"] == key)
        .collect()
}

fn state_adjacent(world: &World, key: &str, next: &str) {
    let all = lines(world);
    let positions: Vec<_> = all
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            (line.kind == "extension_state_set" && line.payload["key"] == key).then_some(index)
        })
        .collect();
    assert!(!positions.is_empty(), "{key}");
    for index in positions {
        assert_eq!(all[index + 1].kind, next, "{key}");
    }
}

#[test]
fn state_lines_prompt_replacements_and_contexts_write_at_their_own_lines() {
    for (rewrite, count) in [(true, 0), (false, 1), (true, 1), (false, 2)] {
        let inner = Arc::new(FakeHooks::new());
        for _ in 0..2 {
            inner.answer_before_message(BeforeMessageAnswer {
                outcome: BeforeMessageOutcome::Passed {
                    replaced: rewrite.then(|| contract::hook::Replaced {
                        content: vec![ContentPart::Text {
                            text: "rewritten".into(),
                        }],
                        changed_by: vec!["state".into()],
                    }),
                    context: (0..count)
                        .map(|i| Context {
                            state: Vec::new(),
                            extension: "state".into(),
                            text: format!("context-{i}"),
                        })
                        .collect(),
                },
                notices: Vec::new(),
            });
        }
        let mut world = started(vec![fakes::Scripted::text("done")], None);
        let cases = StateCases {
            replacement_only: count == 0,
            ..Default::default()
        };
        state_attached(&mut world, inner, cases);
        for id in ["a", "b"] {
            let (ack, _) = capture();
            world
                .inbox
                .send(Delivery::Prompt(driver(id, "hello"), ack))
                .unwrap();
        }
        let done = run_turn(&mut world);
        assert_eq!(finish_turn(&mut world, &done), Some(TurnOutcome::Completed));
        let all = lines(&world);
        let turn = all
            .iter()
            .position(|line| line.kind == "turn_started")
            .unwrap();
        let state: Vec<_> = all
            .iter()
            .enumerate()
            .filter_map(|(i, line)| {
                (line.kind == "extension_state_set" && line.payload["key"] == "before_message")
                    .then_some(i)
            })
            .collect();
        assert_eq!(state.len(), 2 * count.max(1));
        if count == 0 {
            assert_eq!(*state.last().unwrap(), turn - 1);
            assert!(state.iter().all(|i| *i < turn));
        } else {
            assert!(state.iter().all(|i| *i > turn));
            state_adjacent(&world, "before_message", "context_added");
        }
    }
}

#[test]
fn state_lines_queued_steer_is_written_or_dropped_with_its_state() {
    for shutdown in [false, true] {
        let inner = Arc::new(FakeHooks::new());
        inner.answer_before_message(passed_message_with_context(vec![("state", "extra")]));
        let mut world = started(Vec::new(), None);
        state_attached(&mut world, inner, StateCases::default());
        let mut looped = world.looped.take().unwrap();
        let turn = TurnId("state".into());
        let (ack, answered) = capture();
        looped
            .take_steer_running(driver("steer", "hello"), ack, &turn)
            .unwrap();
        assert!(
            Deadline::after(TURN_DEADLINE)
                .recv_or_fail(&answered, "steer admitted")
                .is_ok()
        );
        assert!(state_written(&world, "before_message").is_empty());
        if shutdown {
            looped.cancel.shutdown(143);
            looped.settled(Ok(())).unwrap();
        } else {
            looped.write_queued(&turn).unwrap();
        }
        if shutdown {
            assert!(state_written(&world, "before_message").is_empty());
            assert!(
                !lines(&world)
                    .iter()
                    .any(|line| line.kind == "steering_applied")
            );
        } else {
            state_adjacent(&world, "before_message", "context_added");
        }
    }
}

#[test]
fn state_lines_cancelled_hook_answers_still_drain_other_writes() {
    for point in [
        "session_start",
        "turn_start",
        "before_model_call",
        "turn_end",
    ] {
        let inner = Arc::new(FakeHooks::new());
        inner.answer_session_start(added(vec![("state", "start")]));
        inner.answer_turn_start(added(vec![("state", "turn")]));
        inner.answer_turn_end(added(vec![("state", "end")]));
        let mut world = started(vec![fakes::Scripted::text("done")], None);
        let cancel = world.looped.as_ref().unwrap().cancel.clone();
        match point {
            "session_start" => {
                *inner.shutdown_in_session.lock().unwrap() = Some(cancel);
            }
            "turn_start" => {
                *inner.cancel_in_hook.lock().unwrap() = Some(cancel);
            }
            "before_model_call" => {
                *inner.cancel_in_call.lock().unwrap() = Some(cancel);
            }
            "turn_end" => {
                *inner.cancel_in_end.lock().unwrap() = Some(cancel);
            }
            _ => unreachable!(),
        }
        state_attached(&mut world, inner, StateCases::default());
        let (ack, _) = capture();
        world
            .inbox
            .send(Delivery::Prompt(driver("cancel", "hello"), ack))
            .unwrap();
        let done = run_turn(&mut world);
        let _outcome = finish_turn(&mut world, &done);
        assert!(state_written(&world, point).is_empty(), "{point}");
        assert_eq!(
            state_written(&world, &format!("timer-{point}")).len(),
            1,
            "{point}"
        );
    }
}

#[test]
fn state_lines_turn_end_contexts_keep_their_own_writes_after_held_contexts() {
    let inner = Arc::new(FakeHooks::new());
    inner.answer_turn_end(added(vec![("state", "end")]));
    let mut world = started(Vec::new(), None);
    state_attached(&mut world, inner, StateCases::default());
    let mut looped = world.looped.take().unwrap();
    for text in ["held-a", "held-b"] {
        looped.lifecycle.continued.push(Context {
            state: Vec::new(),
            extension: "state".into(),
            text: text.into(),
        });
    }
    let turn = TurnId("turn_end_index".into());
    assert!(looped.replied(&turn, "reply").unwrap().is_none());
    let held = &looped.lifecycle.continued;
    assert_eq!(
        held.iter()
            .map(|context| context.text.as_str())
            .collect::<Vec<_>>(),
        ["held-a", "held-b", "end"]
    );
    assert!(held[0].state.is_empty());
    assert!(held[1].state.is_empty());
    assert_eq!(held[2].state, vec![carried_state("turn_end")]);
}

#[test]
fn state_lines_held_contexts_and_continuations_share_their_commits() {
    for refuse in [false, true] {
        let inner = Arc::new(FakeHooks::new());
        let mut world = started(Vec::new(), None);
        state_attached(&mut world, inner, StateCases::default());
        let mut looped = world.looped.take().unwrap();
        let turn = TurnId("held".into());
        for key in ["a", "b"] {
            looped.lifecycle.start_contexts.push((
                contract::events::ContextAdded {
                    text: key.into(),
                    extension: "state".into(),
                    hook: "session_start".into(),
                },
                vec![contract::hook::StateLine {
                    value: Some((serde_json::json!(true), contract::events::OnFork::AtPoint)),
                    extension: "state".into(),
                    key: key.into(),
                    landing: 1,
                }],
            ));
            looped.lifecycle.continued.push(Context {
                state: vec![carried_state(&format!("end-{key}"))],
                extension: "state".into(),
                text: key.into(),
            });
        }
        if refuse {
            looped.cancel.shutdown(143);
        }
        looped.write_held(&turn).unwrap();
        if refuse {
            assert_eq!(looped.lifecycle.start_contexts.len(), 2);
            assert!(state_written(&world, "a").is_empty());
            assert!(state_written(&world, "end-a").is_empty());
        } else {
            assert!(looped.lifecycle.start_contexts.is_empty());
            for key in ["a", "b"] {
                state_adjacent(&world, key, "context_added");
                state_adjacent(&world, &format!("end-{key}"), "steering_applied");
            }
        }
    }
}

#[test]
fn state_lines_handoff_note_and_contexts_follow_the_completion_decision() {
    for mode in ["completed", "failed", "refused"] {
        let inner = Arc::new(FakeHooks::new());
        inner.answer_session_start(added(Vec::new()));
        inner.answer_session_start(if mode == "failed" {
            failed("state", "failure")
        } else {
            added(vec![("state", "one"), ("state", "two")])
        });
        let mut world = started_with_tools(
            vec![
                with_tokens(
                    tool_call_reply("", "probe", serde_json::json!({}), 0.0),
                    999,
                    0,
                ),
                fakes::Scripted::text("done"),
            ],
            None,
            vec![Arc::new(Probe)],
        );
        if mode == "refused" {
            *inner.shutdown_after_handoff_check.lock().unwrap() =
                Some(world.looped.as_ref().unwrap().cancel.clone());
        }
        state_attached(&mut world, inner, StateCases::default());
        world.looped = Some(
            world
                .looped
                .take()
                .unwrap()
                .handoff(crate::HandoffSettings {
                    tokens: 1000,
                    nudge: false,
                    ..Default::default()
                }),
        );
        let (ack, _) = capture();
        world
            .inbox
            .send(Delivery::Prompt(driver("handoff", "hello"), ack))
            .unwrap();
        let done = run_turn(&mut world);
        let _outcome = finish_turn(&mut world, &done);
        if mode == "completed" {
            state_adjacent(&world, "before_handoff", "handoff_completed");
            state_adjacent(&world, "session_start", "context_added");
            assert_eq!(state_written(&world, "session_start").len(), 2);
        } else {
            assert!(state_written(&world, "before_handoff").is_empty());
            assert!(state_written(&world, "session_start").is_empty());
        }
    }
}

#[test]
fn state_lines_all_output_shapes_are_carried_to_the_completion_writer() {
    for shape in ["ordinary", "spilled", "unsaved"] {
        for reject in [false, true] {
            let inner = Arc::new(FakeHooks::new());
            let mut world = started(Vec::new(), None);
            let hooks = state_attached(
                &mut world,
                inner,
                StateCases {
                    reject_carried: reject,
                    ..Default::default()
                },
            );
            let mut looped = world.looped.take().unwrap();
            let turn = TurnId("output".into());
            let id = contract::ActionId("output".into());
            let spilled = if shape == "ordinary" {
                None
            } else {
                let file = if shape == "spilled" {
                    let path = world.dir.join("artifacts/output.txt");
                    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                    std::fs::write(&path, b"raw").unwrap();
                    Ok(contract::tool::SpillFile { path, size: 3 })
                } else {
                    Err(contract::tool::Unsaved {
                        message: "unsaved".into(),
                        left: None,
                    })
                };
                Some(contract::tool::Spilled {
                    head: "raw".into(),
                    tail: String::new(),
                    head_bytes: 3,
                    tail_bytes: 0,
                    removed: 0,
                    file,
                })
            };
            let (mut done, state) = looped
                .finish(
                    Output {
                        content: vec![ContentPart::Text { text: "raw".into() }],
                        spilled,
                        control: Some(contract::events::Control {
                            name: Some("state output".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                    ("probe", &serde_json::Map::new()),
                    None,
                    Bound::DEFAULT,
                    false,
                    &id,
                    Some(&turn),
                )
                .unwrap();
            assert!(state_written(&world, "after_tool").is_empty());
            looped
                .after_completed(None, &mut done, &id, Some(&turn), state)
                .unwrap();
            hooks.flush();
            let trace = hooks.trace.lock().unwrap();
            let at = trace.iter().position(|item| item == "after_tool").unwrap();
            assert_eq!(&trace[at + 1..at + 3], ["take", "drain"]);
            assert_eq!(
                state_written(&world, "after_tool").len(),
                usize::from(!reject)
            );
            if !reject {
                state_adjacent(&world, "after_tool", "tool_call_completed");
                let all = lines(&world);
                let named = all
                    .iter()
                    .position(|line| line.kind == "session_named")
                    .unwrap();
                let carried = all
                    .iter()
                    .position(|line| {
                        line.kind == "extension_state_set" && line.payload["key"] == "after_tool"
                    })
                    .unwrap();
                assert!(named < carried);
            }
        }
    }
    for outcome in [
        AfterToolOutcome::Withheld {
            extension: "state".into(),
        },
        AfterToolOutcome::Failed(contract::hook::HookFailure {
            extension: "state".into(),
            message: "own command".into(),
        }),
    ] {
        let inner = Arc::new(FakeHooks::new());
        let mut world = started_with_tools(
            vec![
                tool_call_reply("", "probe", serde_json::json!({}), 0.0),
                fakes::Scripted::text("done"),
            ],
            None,
            vec![Arc::new(Probe)],
        );
        state_attached(
            &mut world,
            inner,
            StateCases {
                after: Some(outcome),
                ..Default::default()
            },
        );
        let (ack, _) = capture();
        world
            .inbox
            .send(Delivery::Prompt(driver("withheld", "hello"), ack))
            .unwrap();
        let done = run_turn(&mut world);
        finish_turn(&mut world, &done);
        assert!(state_written(&world, "after_tool").is_empty());
    }
}

struct StateProbe;
impl Tool for StateProbe {
    fn definition(&self) -> contract::provider::ToolDefinition {
        let mut definition = Probe.definition();
        definition.input_schema = serde_json::json!({"type":"object","required":["path"],"properties":{"path":{"type":"string"}}});
        definition
    }
    fn effects(
        &self,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Effects, EffectsError> {
        Probe.effects(args)
    }
    fn run(
        &self,
        args: &serde_json::Map<String, serde_json::Value>,
        cancel: &dyn Cancel,
        emit: &dyn Emit,
    ) -> Output {
        Probe.run(args, cancel, emit)
    }
    fn bound(&self) -> Bound {
        Probe.bound()
    }
}
struct StateRules {
    decision: Option<contract::rules::RuleDecision>,
    cancel: Option<Arc<crate::TurnCancel>>,
}
impl contract::rules::Rules for StateRules {
    fn read(&self) -> Result<contract::rules::StandingRules, contract::rules::RulesError> {
        if let Some(cancel) = &self.cancel {
            cancel.shutdown(143);
        }
        Ok(contract::rules::StandingRules {
            global: self
                .decision
                .map(|decision| contract::rules::Rule {
                    decision,
                    tool: "probe".into(),
                    prefix: String::new(),
                    added: None,
                    session_id: None,
                })
                .into_iter()
                .collect(),
            project: Vec::new(),
        })
    }
    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), contract::rules::RulesError> {
        Ok(())
    }
}

#[test]
fn state_lines_tool_rewrites_survive_only_accepted_model_and_inner_calls() {
    for inner_call in [false, true] {
        for mode in ["allow", "schema", "check", "deny", "cancel", "failed"] {
            let inner = Arc::new(FakeHooks::new());
            let mut world = started_with_tools(
                vec![
                    tool_call_reply("", "probe", serde_json::json!({"path":"x"}), 0.0),
                    fakes::Scripted::text("done"),
                ],
                None,
                vec![Arc::new(StateProbe)],
            );
            let mut cases = StateCases::default();
            match mode {
                "schema" => {
                    cases.before = Some(BeforeToolOutcome::Rewritten {
                        arguments: serde_json::Map::new(),
                        changed_by: vec!["state".into()],
                    })
                }
                "check" => {
                    cases.check = Some(CheckOutcome::Refused(contract::hook::Refusal {
                        extension: "state".into(),
                        reason: "refused".into(),
                    }))
                }
                "failed" => {
                    cases.before = Some(BeforeToolOutcome::Failed(contract::hook::HookFailure {
                        extension: "state".into(),
                        message: "own command".into(),
                    }))
                }
                _ => {}
            }
            state_attached(&mut world, inner, cases);
            if mode == "deny" || mode == "cancel" {
                let looped = world.looped.as_mut().unwrap();
                looped.rules = Arc::new(StateRules {
                    decision: (mode == "deny").then_some(contract::rules::RuleDecision::Deny),
                    cancel: (mode == "cancel").then(|| looped.cancel.clone()),
                });
            }
            if inner_call {
                let mut looped = world.looped.take().unwrap();
                let (tx, rx) = mpsc::channel();
                looped = fakes::within(
                    "state inner call returned",
                    Deadline::after(TURN_DEADLINE),
                    move || {
                        looped
                            .run_alone(contract::inbox::InnerCallRequest {
                                extension: "other".into(),
                                command: CommandId("state-inner".into()),
                                name: "probe".into(),
                                arguments: serde_json::json!({"path":"x"}),
                                cancel: Arc::new(fakes::CancelToken::new()),
                                taken: Box::new(|| {}),
                                answer: Box::new(move |done| {
                                    let _sent = tx.send(done);
                                }),
                            })
                            .unwrap();
                        looped
                    },
                );
                assert!(rx.try_recv().is_ok(), "inner completion: {mode}");
                world.looped = Some(looped);
            } else {
                let (ack, _) = capture();
                world
                    .inbox
                    .send(Delivery::Prompt(driver("call", "hello"), ack))
                    .unwrap();
                let done = run_turn(&mut world);
                finish_turn(&mut world, &done);
            }
            if mode == "allow" {
                state_adjacent(&world, "before_tool", "tool_call_started");
                state_adjacent(&world, "after_tool", "tool_call_completed");
            } else {
                assert!(state_written(&world, "before_tool").is_empty(), "{mode}");
                assert!(
                    !lines(&world)
                        .iter()
                        .any(|line| line.kind == "tool_call_started"),
                    "{mode}"
                );
            }
            assert_eq!(
                state_written(&world, "timer-before_tool").len(),
                1,
                "{mode}"
            );
            if mode == "deny" {
                assert!(
                    lines(&world)
                        .iter()
                        .any(|line| line.kind == "permission_resolved")
                );
            }
        }
    }
}

#[test]
fn state_lines_resumed_approval_carries_only_the_matching_calls_rewrite() {
    use contract::events::{AskStep, Decision, PermissionRequested};
    for allow in [false, true] {
        let inner = Arc::new(FakeHooks::new());
        let mut world = started_with_tools(
            vec![fakes::Scripted::text("done")],
            None,
            vec![Arc::new(StateProbe)],
        );
        state_attached(&mut world, inner, StateCases::default());
        let turn = TurnId("resumed".into());
        let ids = ["a_before", "a_middle", "a_after"];
        let batch: Vec<_> = ids
            .iter()
            .map(|id| {
                (
                    contract::ActionId((*id).into()),
                    ToolCallRequested {
                        name: "probe".into(),
                        arguments: serde_json::json!({"path":"x"}),
                        provider_id: None,
                        repair: None,
                        ran_by: None,
                        provider_item: None,
                    },
                )
            })
            .collect();
        let request_id = contract::RequestId("r_state".into());
        let request = PermissionRequested {
            request_id: request_id.clone(),
            always_reviewed: false,
            declared: StateProbe
                .effects(&serde_json::Map::new())
                .unwrap()
                .declared,
            step: AskStep::Review {
                escalation: None,
                rule: None,
            },
        };
        let mut looped = world.looped.take().unwrap().answerable(true);
        looped.ensure_preamble().unwrap();
        looped
            .append(
                &contract::events::Event::TurnStarted(TurnStarted { input: Vec::new() }),
                &turn,
                None,
            )
            .unwrap();
        for (id, call) in &batch {
            looped
                .append(
                    &contract::events::Event::ToolCallRequested(call.clone()),
                    &turn,
                    Some(id),
                )
                .unwrap();
        }
        looped.suspended = Some(crate::resume::Suspended {
            turn: turn.clone(),
            pending: crate::suspend::Pending::Approval(request),
            action: batch[1].0.clone(),
            batch,
            started: None,
            permission: None,
        });
        world.looped = Some(looped);
        let (ack, answered) = capture();
        world
            .inbox
            .send(Delivery::Reply(
                contract::commands::Reply {
                    request_id,
                    answer: contract::commands::ReplyAnswer::Approval {
                        decision: if allow {
                            Decision::Allow
                        } else {
                            Decision::Deny
                        },
                        feedback: None,
                        remember: None,
                    },
                },
                ack,
            ))
            .unwrap();
        let done = run_turn(&mut world);
        assert_eq!(finish_turn(&mut world, &done), Some(TurnOutcome::Completed));
        assert!(
            Deadline::after(TURN_DEADLINE)
                .recv_or_fail(&answered, "resumed approval answered")
                .is_ok()
        );
        let all = lines(&world);
        let mut actions = Vec::new();
        for (index, line) in all.iter().enumerate() {
            if line.kind == "extension_state_set" && line.payload["key"] == "before_tool" {
                assert_eq!(all[index + 1].kind, "tool_call_started");
                actions.push(all[index + 1].action_id.as_ref().unwrap().0.as_str());
            }
        }
        assert_eq!(
            actions,
            if allow {
                vec!["a_middle", "a_after"]
            } else {
                vec!["a_after"]
            }
        );
        let requested = all
            .iter()
            .position(|line| line.kind == "permission_requested")
            .unwrap();
        let resolved = all
            .iter()
            .position(|line| line.kind == "permission_resolved")
            .unwrap();
        assert!(requested < resolved);
        let first_state = all
            .iter()
            .position(|line| {
                line.kind == "extension_state_set" && line.payload["key"] == "before_tool"
            })
            .unwrap();
        assert!(resolved < first_state);
    }
}

thread_local! {static STATE_CONTEXT_COMMIT:std::cell::RefCell<Option<Box<dyn FnMut()>>>=std::cell::RefCell::new(None);}
pub(super) fn before_state_context_commit() {
    STATE_CONTEXT_COMMIT.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook();
        }
    });
}

#[test]
fn state_lines_shutdown_between_held_context_commits_keeps_the_second_pair() {
    let inner = Arc::new(FakeHooks::new());
    let mut world = started(Vec::new(), None);
    state_attached(&mut world, inner, StateCases::default());
    let mut looped = world.looped.take().unwrap();
    for key in ["a", "b"] {
        looped.lifecycle.start_contexts.push((
            contract::events::ContextAdded {
                text: key.into(),
                extension: "state".into(),
                hook: "session_start".into(),
            },
            vec![contract::hook::StateLine {
                value: Some((serde_json::json!(true), contract::events::OnFork::AtPoint)),
                extension: "state".into(),
                key: key.into(),
                landing: 1,
            }],
        ));
    }
    let cancel = looped.cancel.clone();
    let mut commits = 0;
    STATE_CONTEXT_COMMIT.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            commits += 1;
            if commits == 2 {
                cancel.shutdown(143);
            }
        }))
    });
    looped.write_held(&TurnId("held".into())).unwrap();
    STATE_CONTEXT_COMMIT.with(|hook| *hook.borrow_mut() = None);
    state_adjacent(&world, "a", "context_added");
    assert!(state_written(&world, "b").is_empty());
    assert_eq!(looped.lifecycle.start_contexts.len(), 1);
    assert_eq!(looped.lifecycle.start_contexts[0].0.text, "b");
    assert_eq!(looped.lifecycle.start_contexts[0].1.len(), 1);
}

#[test]
fn state_lines_refused_continuation_commit_drops_its_carried_lines() {
    let inner = Arc::new(FakeHooks::new());
    let mut world = started(Vec::new(), None);
    state_attached(&mut world, inner, StateCases::default());
    let mut looped = world.looped.take().unwrap();
    looped.lifecycle.continued.push(Context {
        state: vec![carried_state("end")],
        extension: "state".into(),
        text: "continue".into(),
    });
    let cancel = looped.cancel.clone();
    super::BEFORE_HELD_WRITE
        .with(|hook| *hook.borrow_mut() = Some(Box::new(move || cancel.shutdown(143))));
    looped.write_held(&TurnId("held".into())).unwrap();
    assert!(looped.lifecycle.continued.is_empty());
    assert!(state_written(&world, "end").is_empty());
}

#[test]
fn state_lines_non_hook_sender_drains_without_a_registered_hook() {
    for boundary in ["drain", "delivery", "kept"] {
        let inner = Arc::new(FakeHooks::new());
        let mut world = started(vec![fakes::Scripted::text("done")], None);
        let hooks = state_attached(&mut world, inner, StateCases::default());
        hooks.outbox.lock().unwrap().push(state_event("outside"));
        if boundary == "drain" {
            world
                .looped
                .as_mut()
                .unwrap()
                .drain(&TurnId("drain".into()))
                .unwrap();
        } else {
            if boundary == "kept" {
                world
                    .looped
                    .as_mut()
                    .unwrap()
                    .queued
                    .push_back(crate::jobs::Queued::Steer(
                        driver("kept", "hello"),
                        crate::before_message::Hooked::default(),
                    ));
            } else {
                let (ack, _) = capture();
                world
                    .inbox
                    .send(Delivery::Prompt(driver("delivery", "hello"), ack))
                    .unwrap();
            }
            let done = run_turn(&mut world);
            finish_turn(&mut world, &done);
        }
        let all = lines(&world);
        assert_eq!(all[1].kind, "extension_state_set", "{boundary}");
        assert_eq!(all[1].payload["key"], "outside", "{boundary}");
        assert_eq!(state_written(&world, "outside").len(), 1);
    }
}

#[test]
fn state_lines_slash_rejection_drops_the_prompt_and_its_carried_state() {
    let inner = Arc::new(FakeHooks::new());
    inner.answer_before_message(passed_message_with_context(vec![("state", "context")]));
    let mut world = started(Vec::new(), None);
    state_attached(&mut world, inner, StateCases::default());
    let mut looped = world
        .looped
        .take()
        .unwrap()
        .server_prompts(crate::ServerPrompts {
            rows: vec![contract::events::CommandInfo {
                extension: false,
                name: "broken".into(),
                description: String::new(),
                argument_hint: None,
                tag: "server".into(),
            }],
            fetch: Arc::new(|_, _, _, _| Output::default()),
        });
    let (ack, answered) = capture();
    let mut input = crate::inbox::TurnInput::of(Vec::new());
    looped
        .take_prompt(driver("slash", "/broken"), ack, &mut input)
        .unwrap();
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv_or_fail(&answered, "slash rejected")
            .is_err()
    );
    assert!(input.pieces.is_empty());
    assert!(state_written(&world, "before_message").is_empty());
    assert!(!lines(&world).iter().any(|line| line.kind == "turn_started"));
    assert_eq!(state_written(&world, "timer-before_message").len(), 1);
}

#[test]
fn state_lines_cap_failure_never_writes_the_changed_outputs_state() {
    let inner = Arc::new(FakeHooks::new());
    let mut world = started(Vec::new(), None);
    state_attached(
        &mut world,
        inner,
        StateCases {
            after: Some(AfterToolOutcome::Changed {
                content: Some("changed".into()),
                details: None,
                artifact: Some("artifact".into()),
            }),
            ..Default::default()
        },
    );
    let artifacts = world.dir.join("artifacts");
    if artifacts.is_dir() {
        std::fs::remove_dir(&artifacts).unwrap();
    }
    std::fs::write(&artifacts, b"not a directory").unwrap();
    let result = world.looped.as_mut().unwrap().finish(
        Output {
            content: vec![ContentPart::Text { text: "raw".into() }],
            ..Default::default()
        },
        ("probe", &serde_json::Map::new()),
        None,
        Bound::DEFAULT,
        false,
        &contract::ActionId("output".into()),
        None,
    );
    assert!(result.is_err());
    assert!(state_written(&world, "after_tool").is_empty());
    assert_eq!(state_written(&world, "timer-after_tool").len(), 1);
}

#[test]
fn state_lines_later_unchanged_start_write_supersedes_a_carried_handoff_note() {
    let inner = Arc::new(FakeHooks::new());
    let mut world = started_with_tools(
        vec![
            with_tokens(
                tool_call_reply("", "probe", serde_json::json!({}), 0.0),
                999,
                0,
            ),
            fakes::Scripted::text("done"),
        ],
        None,
        vec![Arc::new(Probe)],
    );
    state_attached(
        &mut world,
        inner,
        StateCases {
            supersede_note: true,
            ..Default::default()
        },
    );
    world.looped = Some(
        world
            .looped
            .take()
            .unwrap()
            .handoff(crate::HandoffSettings {
                tokens: 1000,
                nudge: false,
                ..Default::default()
            }),
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("handoff", "hello"), ack))
        .unwrap();
    let done = run_turn(&mut world);
    assert_eq!(finish_turn(&mut world, &done), Some(TurnOutcome::Completed));
    let state = state_written(&world, "before_handoff");
    assert_eq!(state.len(), 1);
    assert_eq!(state[0].payload["value"], "B");
    assert!(
        lines(&world)
            .iter()
            .any(|line| line.kind == "handoff_completed")
    );
}

/// A scripted `Hooks` fake: a queue of answers per lifecycle point, what
/// each call was shown, and an optional cancel or shutdown to start inside
/// `turn_start`'s own call.
struct FakeHooks {
    before_message: Mutex<VecDeque<BeforeMessageAnswer>>,
    session_start: Mutex<VecDeque<ContextAnswer>>,
    session_start_calls: AtomicUsize,
    seen_starts: Mutex<Vec<(String, String, String)>>,
    cancel_in_session: Mutex<Option<Arc<crate::TurnCancel>>>,
    shutdown_in_session: Mutex<Option<Arc<crate::TurnCancel>>>,
    pause_in_session: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
    shutdown_after_handoff_check: Mutex<Option<Arc<crate::TurnCancel>>>,
    turn_start: Mutex<VecDeque<ContextAnswer>>,
    before_model_call: Mutex<VecDeque<CheckAnswer>>,
    turn_end: Mutex<VecDeque<ContextAnswer>>,
    seen_input: Mutex<Vec<String>>,
    turn_start_calls: AtomicUsize,
    before_model_call_calls: AtomicUsize,
    seen_calls: Mutex<Vec<(String, Usage)>>,
    cancel_in_call: Mutex<Option<Arc<crate::TurnCancel>>>,
    turn_end_calls: AtomicUsize,
    seen_replies: Mutex<Vec<String>>,
    cancel_in_end: Mutex<Option<Arc<crate::TurnCancel>>>,
    steer_in_end: Mutex<Option<mpsc::Sender<Delivery>>>,
    cancel_in_hook: Mutex<Option<Arc<crate::TurnCancel>>>,
    shutdown_in_hook: Mutex<Option<Arc<crate::TurnCancel>>>,
    pause_in_start: Mutex<Option<(mpsc::Sender<()>, mpsc::Receiver<()>)>>,
}

impl FakeHooks {
    fn new() -> Self {
        Self {
            before_message: Mutex::new(VecDeque::new()),
            session_start: Mutex::new(VecDeque::new()),
            session_start_calls: AtomicUsize::new(0),
            seen_starts: Mutex::new(Vec::new()),
            cancel_in_session: Mutex::new(None),
            shutdown_in_session: Mutex::new(None),
            pause_in_session: Mutex::new(None),
            shutdown_after_handoff_check: Mutex::new(None),
            turn_start: Mutex::new(VecDeque::new()),
            before_model_call: Mutex::new(VecDeque::new()),
            turn_end: Mutex::new(VecDeque::new()),
            seen_input: Mutex::new(Vec::new()),
            turn_start_calls: AtomicUsize::new(0),
            before_model_call_calls: AtomicUsize::new(0),
            seen_calls: Mutex::new(Vec::new()),
            cancel_in_call: Mutex::new(None),
            turn_end_calls: AtomicUsize::new(0),
            seen_replies: Mutex::new(Vec::new()),
            cancel_in_end: Mutex::new(None),
            steer_in_end: Mutex::new(None),
            cancel_in_hook: Mutex::new(None),
            shutdown_in_hook: Mutex::new(None),
            pause_in_start: Mutex::new(None),
        }
    }

    fn answer_before_message(&self, answer: BeforeMessageAnswer) {
        self.before_message.lock().unwrap().push_back(answer);
    }

    fn answer_session_start(&self, answer: ContextAnswer) {
        self.session_start.lock().unwrap().push_back(answer);
    }

    /// Pause `session_start` on a point the test owns: the hook signals
    /// `started`, waits for `release`, then answers. The test shuts down
    /// while the hook is parked, forcing the shutdown to land inside the
    /// hook's call.
    fn pause_in_session(&self, started: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self.pause_in_session.lock().unwrap() = Some((started, release));
    }

    fn answer_turn_start(&self, answer: ContextAnswer) {
        self.turn_start.lock().unwrap().push_back(answer);
    }

    fn answer_before_model_call(&self, answer: CheckAnswer) {
        self.before_model_call.lock().unwrap().push_back(answer);
    }

    fn answer_turn_end(&self, answer: ContextAnswer) {
        self.turn_end.lock().unwrap().push_back(answer);
    }

    /// Pause `turn_start` on a point the test owns: the hook signals
    /// `started`, waits for `release`, then answers. The test shuts down
    /// while the hook is parked, forcing the shutdown to land inside the
    /// hook's call.
    fn pause_in_start(&self, started: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self.pause_in_start.lock().unwrap() = Some((started, release));
    }
}

fn passed_message() -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: Vec::new(),
        },
        notices: Vec::new(),
    }
}

fn passed_message_with_context(context: Vec<(&str, &str)>) -> BeforeMessageAnswer {
    BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: None,
            context: context
                .into_iter()
                .map(|(extension, text)| Context {
                    state: Vec::new(),
                    extension: extension.to_owned(),
                    text: text.to_owned(),
                })
                .collect(),
        },
        notices: Vec::new(),
    }
}

fn added(texts: Vec<(&str, &str)>) -> ContextAnswer {
    ContextAnswer {
        outcome: ContextOutcome::Added(
            texts
                .into_iter()
                .map(|(extension, text)| Context {
                    state: Vec::new(),
                    extension: extension.to_owned(),
                    text: text.to_owned(),
                })
                .collect(),
        ),
        notices: Vec::new(),
    }
}

fn failed(by: &str, message: &str) -> ContextAnswer {
    ContextAnswer {
        outcome: ContextOutcome::Failed(contract::hook::HookFailure {
            extension: by.to_owned(),
            message: message.to_owned(),
        }),
        notices: Vec::new(),
    }
}

fn noticed_soft(by: &str) -> ContextAnswer {
    ContextAnswer {
        outcome: ContextOutcome::Added(Vec::new()),
        notices: vec![contract::events::Notice {
            code: ErrorCode::HookFailed,
            message: format!("extension {by} failed softly"),
            extension: Some(by.to_owned()),
        }],
    }
}

fn passed_call() -> CheckAnswer {
    CheckAnswer {
        outcome: CheckOutcome::Passed,
        notices: Vec::new(),
    }
}

fn refused_call(by: &str, reason: &str) -> CheckAnswer {
    CheckAnswer {
        outcome: CheckOutcome::Refused(contract::hook::Refusal {
            extension: by.to_owned(),
            reason: reason.to_owned(),
        }),
        notices: Vec::new(),
    }
}

fn failed_call(by: &str, message: &str) -> CheckAnswer {
    CheckAnswer {
        outcome: CheckOutcome::Failed(contract::hook::HookFailure {
            extension: by.to_owned(),
            message: message.to_owned(),
        }),
        notices: Vec::new(),
    }
}

impl Hooks for FakeHooks {
    fn after_tool(&self, _call: &contract::hook::AfterToolCall<'_>) -> AfterToolAnswer {
        AfterToolAnswer {
            outcome: AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }

    fn before_tool(&self, _call: &BeforeToolCall<'_>) -> BeforeToolAnswer {
        BeforeToolAnswer {
            outcome: BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }

    fn check_tool(&self, _call: &BeforeToolCall<'_>) -> CheckAnswer {
        passed_call()
    }

    fn before_message(&self, _message: &BeforeMessage<'_>) -> BeforeMessageAnswer {
        self.before_message
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(passed_message)
    }

    fn session_start(&self, start: &contract::hook::SessionStart<'_>) -> ContextAnswer {
        self.session_start_calls.fetch_add(1, Ordering::SeqCst);
        self.seen_starts.lock().unwrap().push((
            format!("{:?}", start.reason),
            start.session_id.0.clone(),
            start.workspace.to_owned(),
        ));
        if let Some((started, release)) = self.pause_in_session.lock().unwrap().take() {
            started.send(()).unwrap();
            release.recv().unwrap();
        }
        if let Some(cancel) = self.cancel_in_session.lock().unwrap().clone() {
            cancel.cancel();
        }
        if let Some(cancel) = self.shutdown_in_session.lock().unwrap().clone() {
            cancel.shutdown(143);
        }
        // On the turn thread: a shutdown forced after this call's final
        // check, before the handoff's guarded completion write.
        if start.reason == contract::hook::StartReason::Handoff
            && let Some(signal) = self.shutdown_after_handoff_check.lock().unwrap().clone()
        {
            super::pause_after_handoff_start(Box::new(move || signal.shutdown(143)));
        }
        self.session_start
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| added(Vec::new()))
    }

    fn turn_start(&self, turn: &contract::hook::TurnStart<'_>) -> ContextAnswer {
        self.turn_start_calls.fetch_add(1, Ordering::SeqCst);
        self.seen_input.lock().unwrap().push(format!(
            "{:?}",
            turn.input
                .iter()
                .map(|item| match item {
                    InputItem::Message { content, .. } => crate::conversation::text(content),
                    InputItem::Jobs { .. } => "jobs".to_owned(),
                    InputItem::Handoff { .. } => "handoff".to_owned(),
                    InputItem::ShellCommand { .. }
                    | InputItem::Rewind { .. }
                    | InputItem::Unknown => "other".to_owned(),
                })
                .collect::<Vec<_>>()
        ));
        if let Some((started, release)) = self.pause_in_start.lock().unwrap().take() {
            started.send(()).unwrap();
            release.recv().unwrap();
        }
        if let Some(cancel) = self.cancel_in_hook.lock().unwrap().clone() {
            cancel.cancel();
        }
        if let Some(cancel) = self.shutdown_in_hook.lock().unwrap().clone() {
            cancel.shutdown(143);
        }
        self.turn_start
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| added(Vec::new()))
    }

    fn before_model_call(&self, call: &contract::hook::BeforeModelCall<'_>) -> CheckAnswer {
        self.before_model_call_calls.fetch_add(1, Ordering::SeqCst);
        self.seen_calls
            .lock()
            .unwrap()
            .push((call.model.to_owned(), call.usage.clone()));
        if let Some(cancel) = self.cancel_in_call.lock().unwrap().clone() {
            cancel.cancel();
        }
        self.before_model_call
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(passed_call)
    }

    fn turn_end(&self, end: &contract::hook::TurnEnd<'_>) -> ContextAnswer {
        self.turn_end_calls.fetch_add(1, Ordering::SeqCst);
        self.seen_replies.lock().unwrap().push(end.reply.to_owned());
        if let Some(cancel) = self.cancel_in_end.lock().unwrap().clone() {
            cancel.cancel();
        }
        // A steer sent here waits in the inbox while the hook runs, so the
        // next step writes it after the hook's own messages.
        if let Some(inbox) = self.steer_in_end.lock().unwrap().clone() {
            let ack = Ack(Box::new(|_| {}));
            inbox
                .send(Delivery::Steer(driver("c_steer", "late steer"), ack))
                .unwrap();
        }
        self.turn_end
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| added(Vec::new()))
    }

    fn before_handoff(
        &self,
        _handoff: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<Delivery>) {}
}

/// What a turn-level test drives: the loop, its inbox, and the handles to
/// read what it wrote and was sent.
struct World {
    looped: Option<Loop>,
    inbox: mpsc::Sender<Delivery>,
    provider: Arc<fakes::ScriptedProvider>,
    dir: std::path::PathBuf,
    watcher: Option<log::Watcher>,
    _home: fakes::TempDir,
}

/// A `Loop` with its inbox still held by the test, `hooks` set when given,
/// and `script` as its provider's replies.
fn started(script: Vec<fakes::Scripted>, hooks: Option<Arc<FakeHooks>>) -> World {
    started_with_tools(script, hooks, Vec::new())
}

/// [`started`], with `tools` registered under `builtin`.
fn started_with_tools(
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    tools: Vec<Arc<dyn Tool>>,
) -> World {
    let scripted = Arc::new(fakes::ScriptedProvider::new(script));
    let provider = Arc::clone(&scripted) as Arc<dyn contract::provider::Provider>;
    assemble(scripted, provider, hooks, tools)
}

/// [`started_with_tools`], reaching the scripted provider through `wrap`:
/// the test reads the scripted provider's requests on `World.provider`
/// while the loop runs through the wrapper.
fn started_wrapped(
    script: Vec<fakes::Scripted>,
    hooks: Option<Arc<FakeHooks>>,
    tools: Vec<Arc<dyn Tool>>,
    wrap: impl FnOnce(
        Arc<fakes::ScriptedProvider>,
        mpsc::Sender<Delivery>,
    ) -> Arc<dyn contract::provider::Provider>,
) -> World {
    let scripted = Arc::new(fakes::ScriptedProvider::new(script));
    let (inbox, rx) = mpsc::channel();
    let provider = wrap(Arc::clone(&scripted), inbox.clone());
    assemble_with(scripted, provider, inbox, rx, hooks, tools)
}

/// A `Loop` on `provider` with its inbox still held by the test.
fn assemble(
    scripted: Arc<fakes::ScriptedProvider>,
    provider: Arc<dyn contract::provider::Provider>,
    hooks: Option<Arc<FakeHooks>>,
    tools: Vec<Arc<dyn Tool>>,
) -> World {
    let (inbox, rx) = mpsc::channel();
    assemble_with(scripted, provider, inbox, rx, hooks, tools)
}

/// [`assemble`], with the test's own inbox channel.
fn assemble_with(
    scripted: Arc<fakes::ScriptedProvider>,
    provider: Arc<dyn contract::provider::Provider>,
    inbox: mpsc::Sender<Delivery>,
    rx: mpsc::Receiver<Delivery>,
    hooks: Option<Arc<FakeHooks>>,
    tools: Vec<Arc<dyn Tool>>,
) -> World {
    let home = fakes::TempDir::new("fiber-lifecycle");
    let workspace = home.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let log =
        Arc::new(Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap());
    // Subscribed before the loop starts: the stream holds every line the
    // turn writes, ephemeral notices included, which the durable read skips.
    let watcher = log.watch();
    let mut looped = Loop::start(
        crate::Session {
            log: Arc::clone(&log),
            provider,
            model: Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            prompt: crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_test/events.jsonl")
                    .display()
                    .to_string(),
                Arc::clone(&clock),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: tools
                .into_iter()
                .map(|tool| ("builtin".to_owned(), tool))
                .collect(),
            permissions: crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials: home.path().join("credentials"),
                credential_files: Vec::new(),
                rules: Arc::new(NoRules),
            },
        },
        None,
    )
    .unwrap()
    .answerable(false);
    if let Some(hooks) = hooks {
        looped = looped.hooks(hooks as Arc<dyn Hooks>);
    }
    World {
        looped: Some(looped),
        inbox,
        provider: scripted,
        dir: home.path().join("s_test"),
        watcher: Some(watcher),
        _home: home,
    }
}

/// A driver message with `id` saying `text`.
fn driver(id: &str, text: &str) -> Message {
    Message {
        content: vec![ContentPart::Text { text: text.into() }],
        sender: Sender {
            origin: Origin::Driver,
            command_id: Some(CommandId(id.into())),
        },
    }
}

/// An acknowledgement recording its answer.
fn capture() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            drop(tx.send(answer));
        })),
        rx,
    )
}

/// A test tool that only reads and returns `text`.
struct Probe;

impl Tool for Probe {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "probe".into(),
            description: "Returns a fixed output.".into(),
            input_schema: serde_json::json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn Cancel,
        _: &dyn Emit,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "tool output".into(),
            }],
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// A reply saying `text`, then calling `name` with `arguments`, billed at
/// `cost`.
fn tool_call_reply(
    text: &str,
    name: &str,
    arguments: serde_json::Value,
    cost: f64,
) -> fakes::Scripted {
    let mut end = fakes::reply(text);
    end.actions.push(ReplyAction::ToolCall(ToolCallRequested {
        name: name.into(),
        arguments,
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    }));
    end.cost = Some(cost);
    fakes::Scripted {
        deltas: vec![Delta::Text(TextDelta { text: text.into() })],
        end: Ok(end),
    }
}

/// A reply of `text` alone, billed at `cost`.
fn priced(text: &str, cost: f64) -> fakes::Scripted {
    let mut scripted = fakes::Scripted::text(text);
    if let Ok(reply) = &mut scripted.end {
        reply.cost = Some(cost);
    }
    scripted
}

/// `scripted` with its reply's `input` and `output` tokens set.
fn with_tokens(mut scripted: fakes::Scripted, input: u64, output: u64) -> fakes::Scripted {
    if let Ok(reply) = &mut scripted.end {
        reply.tokens.input = input;
        reply.tokens.output = output;
    }
    scripted
}

type Finished = mpsc::Receiver<(Loop, Result<Option<TurnOutcome>, crate::Error>)>;

/// Runs the world's turn on a thread; the inbox stays open so the turn
/// parks on it when nothing starts one.
fn run_turn(world: &mut World) -> Finished {
    let mut looped = world.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn();
        done.send((looped, outcome)).unwrap();
    });
    finished
}

fn finish_turn(world: &mut World, finished: &Finished) -> Option<TurnOutcome> {
    let (looped, outcome) = Deadline::after(TURN_DEADLINE)
        .recv(finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    outcome.unwrap()
}

/// Every durable line the world wrote.
fn lines(world: &World) -> Vec<Envelope> {
    log::read(&world.dir).unwrap()
}

/// The complete, ordered kinds of `lines`.
fn kinds(lines: &[Envelope]) -> Vec<&str> {
    lines.iter().map(|line| line.kind.as_str()).collect()
}

fn line<'a>(lines: &'a [Envelope], kind: &str) -> &'a Envelope {
    lines.iter().find(|line| line.kind == kind).unwrap()
}

/// Every line the world's watcher received up to and including
/// `turn_completed`: the durable lines and the ephemeral ones. The read
/// runs on its own thread under one [`TURN_DEADLINE`] naming the awaited
/// completion, so a missing `turn_completed` fails instead of hanging on
/// the retained log's open watcher.
fn streamed(world: &mut World) -> Vec<Envelope> {
    let watcher = world.watcher.take().unwrap();
    let (watcher, streamed) = fakes::within(
        "a turn_completed line or the log's end",
        Deadline::after(TURN_DEADLINE),
        move || {
            let mut watcher = watcher;
            let mut streamed = Vec::new();
            while let Ok(Some(line)) = watcher.recv() {
                let done = line.kind == "turn_completed";
                streamed.push(line);
                if done {
                    break;
                }
            }
            (watcher, streamed)
        },
    );
    world.watcher = Some(watcher);
    streamed
}

#[test]
fn a_turn_start_context_is_logged_before_the_first_step_and_sent_after_the_prompt() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_message(passed_message_with_context(vec![(
        "fiber.test/before",
        "earlier",
    )]));
    hooks.answer_turn_start(added(vec![("fiber.test/inbox", "3 new messages")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the prompt's answer")
            .is_ok(),
        "the prompt is accepted"
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "context_added",
            "context_added",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let contexts: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "context_added")
        .collect();
    assert_eq!(contexts.len(), 2);
    // A `before_message` context precedes the `turn_start` one.
    assert_eq!(contexts[0].payload["hook"], "before_message");
    assert_eq!(contexts[0].payload["text"], "earlier");
    assert_eq!(contexts[1].payload["hook"], "turn_start");
    assert_eq!(contexts[1].payload["extension"], "fiber.test/inbox");
    assert_eq!(contexts[1].payload["text"], "3 new messages");
    // The hook saw the turn's prompt.
    assert_eq!(
        hooks.seen_input.lock().unwrap().clone(),
        vec!["[\"hello\"]"]
    );
    // The first request ends with the context as its own user message
    // after the prompt.
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 1);
    let tail: Vec<String> = requests[0]
        .conversation
        .iter()
        .rev()
        .take(3)
        .map(|input| match input {
            contract::provider::Input::User { text, .. } => text.clone(),
            contract::provider::Input::Assistant { .. }
            | contract::provider::Input::Reasoning { .. }
            | contract::provider::Input::ToolCall { .. }
            | contract::provider::Input::ToolResult { .. }
            | contract::provider::Input::ToolSearchResult { .. } => {
                panic!("a user message: {input:?}")
            }
        })
        .collect();
    assert_eq!(tail, vec!["3 new messages", "earlier", "hello"]);
}

#[test]
fn a_blocking_turn_start_failure_fails_the_turn_before_any_request() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(failed("fiber.test/guard", "boom"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the prompt's answer")
            .is_ok(),
        "the prompt is accepted before the hook fails"
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "turn_completed",
        ]
    );
    let end = line(&all, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "hook_failed");
    assert_eq!(
        end.payload["error"]["message"],
        "The `turn_start` hook of extension fiber.test/guard failed, \
         so the turn did not run: boom"
    );
    assert!(
        world.provider.requests().is_empty(),
        "the provider got no request"
    );
}

#[test]
fn a_non_blocking_turn_start_failure_writes_a_notice_and_the_turn_goes_on() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(noticed_soft("fiber.test/soft"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    let all = streamed(&mut world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "notice",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let notice = line(&all, "notice");
    assert_eq!(notice.payload["code"], "hook_failed");
    assert_eq!(notice.payload["extension"], "fiber.test/soft");
    assert_eq!(world.provider.requests().len(), 1);
}

#[test]
fn a_cancel_or_shutdown_inside_turn_start_ends_the_turn_interrupted_with_no_context() {
    for shutdown in [false, true] {
        let hooks = Arc::new(FakeHooks::new());
        hooks.answer_turn_start(added(vec![("fiber.test/inbox", "late")]));
        let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
        {
            let looped = world.looped.take().unwrap();
            let signal = Arc::clone(&looped.cancel);
            if shutdown {
                hooks.shutdown_in_hook.lock().unwrap().replace(signal);
            } else {
                hooks.cancel_in_hook.lock().unwrap().replace(signal);
            }
            world.looped = Some(looped);
        }
        let (ack, _) = capture();
        world
            .inbox
            .send(Delivery::Prompt(driver("c_1", "hello"), ack))
            .unwrap();
        let finished = run_turn(&mut world);
        assert_eq!(
            finish_turn(&mut world, &finished),
            Some(TurnOutcome::Interrupted),
            "shutdown: {shutdown}"
        );
        drop(world.looped.take());

        let all = lines(&world);
        assert_eq!(
            kinds(&all),
            [
                "session_started",
                "preamble_built",
                "opening_message",
                "turn_started",
                "turn_completed",
            ],
            "complete ordered kinds"
        );
        assert!(
            !all.iter().any(|line| line.kind == "context_added"),
            "no context is written: {all:?}"
        );
        assert!(
            !all.iter().any(|line| line.kind == "step_started"),
            "no step starts: {all:?}"
        );
    }
}

#[test]
fn two_turn_start_contexts_are_written_in_run_order() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(added(vec![
        ("fiber.test/first", "one"),
        ("fiber.test/second", "two"),
    ]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "context_added",
            "context_added",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    let texts: Vec<&str> = all
        .iter()
        .filter(|line| line.kind == "context_added")
        .map(|line| line.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["one", "two"]);
}

#[test]
fn without_hooks_the_kinds_list_is_unchanged() {
    let mut world = started(vec![fakes::Scripted::text("Done.")], None);
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_refused_held_write_keeps_session_start_contexts_for_the_next_step() {
    use contract::events::ContextAdded;

    fn held(text: &str) -> ContextAdded {
        ContextAdded {
            text: text.into(),
            extension: "fiber.test/inbox".into(),
            hook: "session_start".into(),
        }
    }
    // A refused `Commit::Step` keeps the contexts unwritten.
    let mut world = started(vec![fakes::Scripted::text("Done.")], None);
    let turn = TurnId("t_1".into());
    {
        let mut looped = world.looped.take().unwrap();
        looped
            .lifecycle
            .start_contexts
            .push((held("kept"), Vec::new()));
        looped.cancel.shutdown(143);
        looped.write_held(&turn).unwrap();
        assert_eq!(
            looped.lifecycle.start_contexts.len(),
            1,
            "a refused commit keeps the contexts"
        );
        world.looped = Some(looped);
    }
    drop(world.looped.take());
    assert!(
        !lines(&world)
            .iter()
            .any(|line| line.kind == "context_added"),
        "nothing was written"
    );
    // The next successful step writes them oldest first.
    let mut world = started(vec![fakes::Scripted::text("Done.")], None);
    {
        let mut looped = world.looped.take().unwrap();
        looped
            .lifecycle
            .start_contexts
            .push((held("one"), Vec::new()));
        looped
            .lifecycle
            .start_contexts
            .push((held("two"), Vec::new()));
        looped.write_held(&turn).unwrap();
        assert!(
            looped.lifecycle.start_contexts.is_empty(),
            "a successful step drains them"
        );
        world.looped = Some(looped);
    }
    drop(world.looped.take());
    let all = lines(&world);
    let texts: Vec<&str> = all
        .iter()
        .filter(|line| line.kind == "context_added")
        .map(|line| line.payload["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["one", "two"]);
}

/// The turn's raw result: [`finish_turn`] unwraps it, which a hook
/// failure cannot survive.
fn turn_result(
    world: &mut World,
    finished: &Finished,
) -> Result<Option<TurnOutcome>, crate::Error> {
    let (looped, outcome) = Deadline::after(TURN_DEADLINE)
        .recv(finished)
        .expect("the turn ended");
    world.looped = Some(looped);
    outcome
}

/// The logged opening message, rendered as the model reads it.
fn opening_text(all: &[Envelope]) -> String {
    let message: contract::events::OpeningMessage = serde_json::from_value(
        serde_json::Value::Object(line(all, "opening_message").payload.clone()),
    )
    .unwrap();
    crate::opening::render(&message)
}

#[test]
fn a_session_start_context_is_logged_before_turn_started_and_sent_first() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_session_start(added(vec![("fiber.test/inbox", "starter pack")]));
    let mut world = started(
        vec![
            fakes::Scripted::text("Done."),
            fakes::Scripted::text("Done again."),
        ],
        Some(hooks.clone()),
    );
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&answered)
            .expect("the prompt's answer")
            .is_ok(),
        "the prompt is accepted"
    );
    // A second turn runs no hook: once per process.
    let (again, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_2", "again"), again))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert_eq!(
        hooks.session_start_calls.load(Ordering::SeqCst),
        1,
        "the hook runs once per process"
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "context_added",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    let contexts: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "context_added")
        .collect();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].payload["hook"], "session_start");
    assert_eq!(contexts[0].payload["extension"], "fiber.test/inbox");
    assert_eq!(contexts[0].payload["text"], "starter pack");
    // The hook saw why the session started, its id and its workspace.
    let workspace = world
        .dir
        .parent()
        .unwrap()
        .join("workspace")
        .display()
        .to_string();
    assert_eq!(
        hooks.seen_starts.lock().unwrap().clone(),
        vec![("Start".to_owned(), "s_test".to_owned(), workspace)]
    );
    // The first request is the opening message, the context, the prompt.
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 2);
    let first: Vec<String> = requests[0]
        .conversation
        .iter()
        .map(|input| match input {
            contract::provider::Input::User { text, .. } => text.clone(),
            input @ (contract::provider::Input::Assistant { .. }
            | contract::provider::Input::Reasoning { .. }
            | contract::provider::Input::ToolCall { .. }
            | contract::provider::Input::ToolResult { .. }
            | contract::provider::Input::ToolSearchResult { .. }) => {
                panic!("a user message: {input:?}")
            }
        })
        .collect();
    assert_eq!(
        first,
        vec![opening_text(&all), "starter pack".into(), "hello".into()]
    );
}

#[test]
fn a_blocking_session_start_failure_starts_no_session_and_drops_the_prompt() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_session_start(ContextAnswer {
        outcome: ContextOutcome::Failed(contract::hook::HookFailure {
            extension: "fiber.test/guard".into(),
            message: "boom".into(),
        }),
        notices: vec![contract::events::Notice {
            code: ErrorCode::HookFailed,
            message: "extension fiber.test/soft failed softly".into(),
            extension: Some("fiber.test/soft".into()),
        }],
    });
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let (ack, answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    let Err(error) = turn_result(&mut world, &finished) else {
        panic!("the turn fails");
    };
    assert_eq!(error.code(), ErrorCode::HookFailed);
    assert_eq!(
        error.to_string(),
        "The `session_start` hook of extension fiber.test/guard failed, \
         so the session did not start: boom"
    );
    assert!(
        answered.try_recv().is_err(),
        "the prompt's acknowledgement is dropped uncalled"
    );
    drop(world.looped.take());

    let all = streamed(&mut world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "notice"
        ],
        "no turn starts and the notice is written: {all:?}"
    );
    let notice = line(&all, "notice");
    assert_eq!(notice.payload["code"], "hook_failed");
    assert_eq!(notice.payload["extension"], "fiber.test/soft");
    assert!(
        world.provider.requests().is_empty(),
        "the provider got no request"
    );
}

#[test]
fn a_shutdown_around_session_start_runs_no_hook_and_starts_no_turn() {
    // A shutdown started before the turn runs no hook.
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_session_start(added(vec![("fiber.test/inbox", "early")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    {
        let looped = world.looped.take().unwrap();
        looped.cancel.shutdown(143);
        world.looped = Some(looped);
    }
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(turn_result(&mut world, &finished).unwrap(), None);
    assert_eq!(
        hooks.session_start_calls.load(Ordering::SeqCst),
        0,
        "no hook runs"
    );
    drop(world.looped.take());
    assert_eq!(kinds(&lines(&world)), ["session_started"]);

    // A shutdown started inside the hook's call writes no context.
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_session_start(added(vec![("fiber.test/inbox", "late")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    let signal = {
        let looped = world.looped.take().unwrap();
        let signal = Arc::clone(&looped.cancel);
        world.looped = Some(looped);
        signal
    };
    let (signalled, parked) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    hooks.pause_in_session(signalled, wait);
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    Deadline::after(TURN_DEADLINE)
        .recv(&parked)
        .expect("the hook parked");
    signal.shutdown(143);
    release.send(()).unwrap();
    assert_eq!(turn_result(&mut world, &finished).unwrap(), None);
    drop(world.looped.take());
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        ["session_started", "preamble_built", "opening_message"],
        "no context and no turn start: {all:?}"
    );
}

#[test]
fn a_shutdown_between_the_handoff_hook_and_its_commit_cancels_the_handoff() {
    for fails in [false, true] {
        let hooks = Arc::new(FakeHooks::new());
        hooks.answer_session_start(added(Vec::new()));
        hooks.answer_session_start(if fails {
            failed("fiber.test/guard", "boom")
        } else {
            added(vec![("fiber.test/inbox", "late")])
        });
        let mut world = started_with_tools(
            vec![
                with_tokens(
                    tool_call_reply("", "probe", serde_json::json!({"path": "a.txt"}), 0.0),
                    999,
                    0,
                ),
                fakes::Scripted::text("The note."),
                fakes::Scripted::text("Done."),
            ],
            Some(hooks.clone()),
            vec![Arc::new(Probe)],
        );
        {
            let looped = world.looped.take().unwrap();
            // After the handoff hook's final check, before the guarded
            // completion write: the shutdown applies none of the answer.
            // Armed from the hook itself, which runs on the turn thread.
            hooks
                .shutdown_after_handoff_check
                .lock()
                .unwrap()
                .replace(Arc::clone(&looped.cancel));
            world.looped = Some(looped.handoff(crate::HandoffSettings {
                tokens: 1_000,
                nudge: false,
                ..crate::HandoffSettings::default()
            }));
        }
        let (ack, _) = capture();
        world
            .inbox
            .send(Delivery::Prompt(driver("c_1", "hello"), ack))
            .unwrap();
        let finished = run_turn(&mut world);
        assert_eq!(
            finish_turn(&mut world, &finished),
            Some(TurnOutcome::Interrupted),
            "fails: {fails}"
        );
        drop(world.looped.take());
        let all = lines(&world);
        assert_eq!(
            kinds(&all),
            [
                "session_started",
                "preamble_built",
                "opening_message",
                "turn_started",
                "step_started",
                "assistant_message_started",
                "tool_call_requested",
                "usage_recorded",
                "assistant_message_completed",
                "tool_call_started",
                "tool_call_completed",
                "step_started",
                "handoff_started",
                "assistant_message_started",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "handoff_completed",
                "turn_completed",
            ],
            "fails: {fails}, {all:?}"
        );
        let done: Vec<&Envelope> = all
            .iter()
            .filter(|line| line.kind == "handoff_completed")
            .collect();
        assert_eq!(done.len(), 1);
        assert_eq!(
            done[0].payload["outcome"], "cancelled",
            "a shutdown applies none of the answer, failed or not: {:?}, fails: {fails}",
            done[0].payload
        );
        let end = line(&all, "turn_completed");
        assert_eq!(end.payload["outcome"], "interrupted");
    }
}

#[test]
fn turn_start_is_shown_the_input_without_unknown_items() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(Vec::new(), Some(hooks.clone()));
    let mut looped = world.looped.take().unwrap();
    let input = vec![
        InputItem::Message {
            content: vec![ContentPart::Text { text: "hi".into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(CommandId("c_1".into())),
            },
            changed_by: None,
        },
        InputItem::Unknown,
    ];
    let ended = looped.turn_start(&TurnId("t_test".into()), &input).unwrap();
    assert_eq!(ended, None);
    world.looped = Some(looped);
    assert_eq!(
        hooks.seen_input.lock().unwrap().clone(),
        vec!["[\"hi\"]"],
        "the Unknown item is filtered out, not an error"
    );
    assert_eq!(hooks.turn_start_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn the_hook_sees_each_request_with_the_model_and_the_spend_so_far() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started_with_tools(
        vec![
            tool_call_reply("Checking.", "probe", serde_json::json!({}), 0.5),
            priced("Done.", 0.0),
        ],
        Some(hooks.clone()),
        vec![Arc::new(Probe) as Arc<dyn Tool>],
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // Once per request: the tool call's, then the reply's.
    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 2);
    let seen = hooks.seen_calls.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0].0, "fake/model");
    assert_eq!(seen[0].1.cost, Some(0.0));
    // The second call sees the first step's cost.
    assert_eq!(seen[1].0, "fake/model");
    assert_eq!(seen[1].1.cost, Some(0.5));
    assert_eq!(world.provider.requests().len(), 2);
}

#[test]
fn a_refusal_fails_the_turn_budget_exceeded_and_stops_the_delegates() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_model_call(refused_call("fiber.test/guard", "team budget spent"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let jobs_dir = world._home.path().join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let jobs = fakes::jobs::FakeJobs::new(&jobs_dir);
    world.looped = world
        .looped
        .take()
        .map(|looped| looped.jobs(Arc::clone(&jobs) as Arc<dyn contract::jobs::Jobs>));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    let end = line(&all, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "budget_exceeded");
    assert_eq!(
        end.payload["error"]["message"],
        "Extension fiber.test/guard refused the model request: team budget spent"
    );
    assert!(
        world.provider.requests().is_empty(),
        "the provider got no request for that step"
    );
    assert_eq!(jobs.stop_delegates_calls(), 1);
}

#[test]
fn a_blocking_before_model_call_failure_fails_the_turn_without_stopping_delegates() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_model_call(failed_call("fiber.test/guard", "boom"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let jobs_dir = world._home.path().join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let jobs = fakes::jobs::FakeJobs::new(&jobs_dir);
    world.looped = world
        .looped
        .take()
        .map(|looped| looped.jobs(Arc::clone(&jobs) as Arc<dyn contract::jobs::Jobs>));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    let end = line(&all, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "hook_failed");
    assert_eq!(
        end.payload["error"]["message"],
        "The `before_model_call` hook of extension fiber.test/guard failed, \
         so the request was not sent: boom"
    );
    assert!(world.provider.requests().is_empty());
    assert_eq!(jobs.stop_delegates_calls(), 0);
}

#[test]
fn a_retried_request_runs_the_hook_once() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(
        vec![
            fakes::Scripted::failed(contract::shapes::Failure {
                code: ErrorCode::RateLimited,
                message: "slow down".into(),
                retry_after_ms: None,
                provider: None,
            }),
            priced("Done.", 0.0),
        ],
        Some(hooks.clone()),
    );
    // No backoff wait: the retry resends at once on the fake clock.
    world.looped = world.looped.take().map(|looped| {
        looped.retry(crate::Retry {
            attempts: 3,
            initial: Duration::ZERO,
            max: Duration::from_secs(60),
        })
    });
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 1);
    // The retry resent the same request.
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].conversation, requests[1].conversation);
}

#[test]
fn a_handoff_for_size_runs_the_hook_again_on_the_rebuilt_request() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(
        vec![
            fakes::Scripted::failed(contract::shapes::Failure {
                code: ErrorCode::ContextOverflow,
                message: "too big".into(),
                retry_after_ms: None,
                provider: None,
            }),
            priced("the note", 0.0),
            priced("Done.", 0.0),
        ],
        Some(hooks.clone()),
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // The request that did not fit, the note request the hook never sees,
    // and the rebuilt request: the hook ran on the first and the third.
    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 2);
    assert_eq!(world.provider.requests().len(), 3);
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
}

#[test]
fn an_over_budget_session_never_calls_the_hook() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    let jobs_dir = world._home.path().join("jobs");
    std::fs::create_dir_all(&jobs_dir).unwrap();
    let jobs = fakes::jobs::FakeJobs::new(&jobs_dir);
    world.looped = world.looped.take().map(|looped| {
        looped
            .jobs(Arc::clone(&jobs) as Arc<dyn contract::jobs::Jobs>)
            .budget(Some(0.0))
    });
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    drop(world.looped.take());

    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 0);
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    assert_eq!(
        line(&all, "turn_completed").payload["error"]["code"],
        "budget_exceeded"
    );
    assert_eq!(jobs.stop_delegates_calls(), 1);
}

#[test]
fn a_handoff_note_request_never_reaches_the_hook() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started_with_tools(
        vec![
            with_tokens(
                tool_call_reply("Checking.", "probe", serde_json::json!({}), 0.0),
                5000,
                5000,
            ),
            priced("the note", 0.0),
            priced("Done.", 0.0),
        ],
        Some(hooks.clone()),
        vec![Arc::new(Probe) as Arc<dyn Tool>],
    );
    // Any reply overflows the context, so the next step hands off first.
    world.looped = world.looped.take().map(|looped| {
        looped.handoff(crate::HandoffSettings {
            enabled: true,
            tokens: 1,
            window_fraction: 1.0,
            nudge: false,
        })
    });
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // The step's request and the rebuilt one: the note request between them
    // never reached the hook.
    assert_eq!(hooks.before_model_call_calls.load(Ordering::SeqCst), 2);
    assert_eq!(world.provider.requests().len(), 3);
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
}

#[test]
fn a_cancel_inside_the_hook_ends_the_turn_interrupted_even_on_a_refusal() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_model_call(refused_call("fiber.test/guard", "team budget spent"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    {
        let looped = world.looped.take().unwrap();
        hooks
            .cancel_in_call
            .lock()
            .unwrap()
            .replace(Arc::clone(&looped.cancel));
        world.looped = Some(looped);
    }
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Interrupted)
    );
    drop(world.looped.take());

    // The refusal was never applied: nothing was sent and the turn did not
    // fail.
    assert!(world.provider.requests().is_empty());
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    assert_eq!(
        line(&all, "turn_completed").payload["outcome"],
        "interrupted"
    );
}

/// The `turn_started` message items.
fn started_input(lines: &[Envelope]) -> Vec<InputItem> {
    let started: TurnStarted = serde_json::from_value(serde_json::Value::Object(
        line(lines, "turn_started").payload.clone(),
    ))
    .unwrap();
    started.input
}

/// A provider that sends one steer to the inbox when the first call is
/// made, so it waits at that step's end-of-turn drain, then answers from
/// its scripted provider.
struct SteerDuring {
    inner: Arc<fakes::ScriptedProvider>,
    during: Mutex<Option<mpsc::Sender<Delivery>>>,
}

impl contract::provider::Provider for SteerDuring {
    fn call(
        &self,
        request: &contract::provider::ModelRequest,
    ) -> Box<dyn contract::provider::ModelCall> {
        if let Some(inbox) = self.during.lock().unwrap().take() {
            let ack = Ack(Box::new(|_| {}));
            inbox
                .send(Delivery::Steer(driver("c_steer", "aside"), ack))
                .unwrap();
        }
        self.inner.call(request)
    }

    fn wire_tools(
        &self,
        tools: &[contract::provider::ToolDefinition],
    ) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.inner.wire_tools(tools)
    }

    fn cost_lookup(&self) -> Option<Arc<dyn contract::provider::CostLookup>> {
        self.inner.cost_lookup()
    }

    fn warms(&self, request: &contract::provider::ModelRequest) -> bool {
        self.inner.warms(request)
    }
}

/// A test tool whose calls end the turn asking one question.
struct Asking;

impl Tool for Asking {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "probe".into(),
            description: "Asks one question.".into(),
            input_schema: serde_json::json!({"type": "object"}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Effects, EffectsError> {
        Ok(Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, serde_json::Value>,
        _: &dyn Cancel,
        _: &dyn Emit,
    ) -> Output {
        Output {
            content: vec![ContentPart::Text {
                text: "a question".into(),
            }],
            control: Some(contract::events::Control {
                handoff: None,
                questions: Some(vec![contract::shapes::Question {
                    header: "Pick".into(),
                    question: "Which?".into(),
                    options: Vec::new(),
                    multi_select: None,
                }]),
                skill: None,
                name: None,
            }),
            ..Output::default()
        }
    }

    fn bound(&self) -> Bound {
        Bound::DEFAULT
    }
}

/// A reply of `text` cut off by the output limit.
fn cut_off(text: &str) -> fakes::Scripted {
    let mut end = fakes::reply(text);
    end.finish = contract::provider::Finish::OutputLimit;
    fakes::Scripted {
        deltas: Vec::new(),
        end: Ok(end),
    }
}

/// A reasoning-only reply thinking `thought`: no text, no call.
fn reasoning_only(thought: &str) -> fakes::Scripted {
    let mut end = fakes::reply("");
    end.actions = vec![contract::provider::ReplyAction::Reasoning(
        contract::events::ReasoningCompleted {
            text: thought.into(),
            provider_item: None,
        },
    )];
    fakes::Scripted {
        deltas: Vec::new(),
        end: Ok(end),
    }
}

#[test]
fn a_turn_end_message_continues_the_turn_and_the_hook_runs_again() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![("fiber.test/goal", "keep going")]));
    let mut world = started(
        vec![
            fakes::Scripted::text("Done."),
            fakes::Scripted::text("All set."),
        ],
        Some(hooks.clone()),
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // The hook saw the reply that ended the step, then the next one, which
    // ended the turn.
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 2);
    assert_eq!(
        hooks.seen_replies.lock().unwrap().clone(),
        vec!["Done.", "All set."]
    );
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "step_started",
            "steering_applied",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    // The message is the extension's own, with no command and no rewrites.
    let steering = line(&all, "steering_applied");
    assert_eq!(
        steering.payload["content"],
        serde_json::json!([{"type": "text", "text": "keep going"}])
    );
    assert_eq!(steering.payload["source"], "extension");
    assert_eq!(steering.payload["extension"], "fiber.test/goal");
    assert!(steering.payload.get("command_id").is_none());
    assert!(steering.payload.get("changed_by").is_none());
    // The next request carries it as a user message.
    let requests = world.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].conversation.last(),
        Some(&contract::provider::Input::User {
            text: "keep going".into(),
            images: Vec::new(),
        })
    );
}

#[test]
fn a_steer_waiting_at_the_end_of_turn_drain_skips_turn_end_that_time() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started_wrapped(
        vec![fakes::Scripted::text("one"), fakes::Scripted::text("two")],
        Some(hooks.clone()),
        Vec::new(),
        |inner, inbox| {
            Arc::new(SteerDuring {
                inner,
                during: Mutex::new(Some(inbox)),
            }) as Arc<dyn contract::provider::Provider>
        },
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    // The first step's end found the steer waiting and never ran the hook;
    // the second step's end did, and ended the turn.
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 1);
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "step_started",
            "steering_applied",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    let steering: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "steering_applied")
        .collect();
    assert_eq!(steering.len(), 1);
    assert_eq!(
        steering[0].payload["content"][0]["text"], "aside",
        "the waiting steer continued the turn"
    );
}

#[test]
fn a_cancel_inside_turn_end_drops_its_message_and_starts_no_later_turn_with_it() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![("fiber.test/goal", "keep going")]));
    let mut world = started(
        vec![
            fakes::Scripted::text("Done."),
            fakes::Scripted::text("All set."),
        ],
        Some(hooks.clone()),
    );
    {
        let looped = world.looped.take().unwrap();
        hooks
            .cancel_in_end
            .lock()
            .unwrap()
            .replace(Arc::clone(&looped.cancel));
        world.looped = Some(looped);
    }
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Interrupted)
    );
    // The next turn holds no extension message: it never joined `queued`.
    // The fake stops cancelling now, so the next turn runs clean.
    hooks.cancel_in_end.lock().unwrap().take();
    let (next_ack, next_answered) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_2", "again"), next_ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    assert!(
        Deadline::after(TURN_DEADLINE)
            .recv(&next_answered)
            .expect("the prompt's answer")
            .is_ok()
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    let started: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "turn_started")
        .collect();
    assert_eq!(started.len(), 2);
    let input = started_input(std::slice::from_ref(started[1]));
    assert_eq!(input.len(), 1);
    let InputItem::Message { content, .. } = &input[0] else {
        panic!("a message item: {input:?}");
    };
    assert_eq!(crate::conversation::text(content), "again");
}

#[test]
fn replied_runs_no_hook_once_a_cancel_landed() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(Vec::new(), Some(hooks.clone()));
    let mut looped = world.looped.take().unwrap();
    assert!(looped.cancel.arm());
    looped.cancel.cancel();
    let ended = looped.replied(&TurnId("t_test".into()), "Done.").unwrap();
    world.looped = Some(looped);
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 0);
    let Some(end) = ended else {
        panic!("a cancelled turn ends");
    };
    assert_eq!(end.outcome, TurnOutcome::Interrupted);
}

#[test]
fn a_turn_that_fails_or_asks_questions_never_calls_turn_end() {
    // A failed model call ends the turn with no reply to show the hook.
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(
        vec![fakes::Scripted::failed(contract::shapes::Failure {
            code: ErrorCode::QuotaExceeded,
            message: "no more".into(),
            retry_after_ms: None,
            provider: None,
        })],
        Some(hooks.clone()),
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    drop(world.looped.take());
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        kinds(&lines(&world)),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );

    // A call asking questions ends the turn at the step boundary instead.
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started_with_tools(
        vec![
            tool_call_reply("Checking.", "probe", serde_json::json!({}), 0.0),
            fakes::Scripted::text("Done."),
        ],
        Some(hooks.clone()),
        vec![Arc::new(Asking) as Arc<dyn Tool>],
    );
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 0);
    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    let end = line(&all, "turn_completed");
    assert!(end.payload.get("questions").is_some());
}

#[test]
fn a_blocking_turn_end_failure_writes_nothing_and_fails_the_turn() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(failed("fiber.test/goal", "boom"));
    let mut world = started(vec![fakes::Scripted::text("one")], Some(hooks.clone()));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Failed)
    );
    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 1);
    // The failed chain holds nothing for a later turn either.
    assert!(
        world
            .looped
            .as_ref()
            .unwrap()
            .lifecycle
            .continued
            .is_empty()
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    assert!(
        !all.iter().any(|line| line.kind == "steering_applied"),
        "the failure wrote no message"
    );
    let end = line(&all, "turn_completed");
    assert_eq!(end.payload["outcome"], "failed");
    assert_eq!(end.payload["error"]["code"], "hook_failed");
    assert_eq!(
        end.payload["error"]["message"],
        "The `turn_end` hook of extension fiber.test/goal failed: boom"
    );
}

#[test]
fn two_turn_end_messages_are_written_in_run_order_after_a_late_steer() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![
        ("fiber.test/first", "one"),
        ("fiber.test/second", "two"),
    ]));
    let mut world = started(
        vec![
            fakes::Scripted::text("Done."),
            fakes::Scripted::text("All set."),
        ],
        Some(hooks.clone()),
    );
    // A steer sent while the hook runs waits in the inbox behind it.
    hooks
        .steer_in_end
        .lock()
        .unwrap()
        .replace(world.inbox.clone());
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "step_started",
            "steering_applied",
            "steering_applied",
            "steering_applied",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    let texts: Vec<&str> = all
        .iter()
        .filter(|line| line.kind == "steering_applied")
        .map(|line| line.payload["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        vec!["one", "two", "late steer"],
        "the hook's messages precede the steer that arrived meanwhile"
    );
}

#[test]
fn an_output_limit_reply_runs_turn_end_with_its_kept_text() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(vec![cut_off("partial")], Some(hooks.clone()));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.seen_replies.lock().unwrap().clone(), vec!["partial"]);
}

#[test]
fn a_reasoning_only_reply_runs_turn_end_with_no_text() {
    let hooks = Arc::new(FakeHooks::new());
    let mut world = started(vec![reasoning_only("hmm")], Some(hooks.clone()));
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Completed)
    );
    drop(world.looped.take());

    assert_eq!(hooks.turn_end_calls.load(Ordering::SeqCst), 1);
    assert_eq!(hooks.seen_replies.lock().unwrap().clone(), vec![""]);
}

#[test]
fn a_cancel_before_the_next_steps_commit_drops_held_messages() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![("fiber.test/goal", "keep going")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let mut looped = world.looped.take().unwrap();
    let turn = TurnId("t_test".into());
    // The reply ends the step with nothing waiting: its message is held for
    // the next step, not written yet.
    assert_eq!(looped.replied(&turn, "Done.").unwrap(), None);
    assert_eq!(looped.lifecycle.continued.len(), 1);
    // A cancel lands before the next step's commit: the commit is refused
    // and the held message is dropped unwritten.
    assert!(looped.cancel.arm());
    assert!(looped.cancel.cancel());
    let step = looped.step(&turn).unwrap();
    assert!(
        matches!(step, crate::Step::Ended(_)),
        "the turn ends without another request"
    );
    // A later turn is clean: nothing held over.
    assert!(looped.lifecycle.continued.is_empty());
    world.looped = Some(looped);
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(kinds(&all), ["session_started"], "complete ordered kinds");
    assert!(
        !all.iter().any(|line| line.kind == "steering_applied"),
        "no held message was written: {:?}",
        kinds(&all)
    );
}

#[test]
fn a_shutdown_before_the_held_write_applies_no_held_message() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![("fiber.test/goal", "keep going")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let mut looped = world.looped.take().unwrap();
    let turn = TurnId("t_test".into());
    // The reply holds one message for the next step, written by
    // `write_held`, not yet.
    assert_eq!(looped.replied(&turn, "Done.").unwrap(), None);
    assert_eq!(looped.lifecycle.continued.len(), 1);
    // A shutdown lands after the hold but before the guarded write: the
    // write is refused and the message is dropped unwritten.
    assert!(looped.cancel.arm());
    looped.cancel.shutdown(143);
    looped.write_held(&turn).unwrap();
    assert!(looped.lifecycle.continued.is_empty());
    world.looped = Some(looped);
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(kinds(&all), ["session_started"], "complete ordered kinds");
    assert!(
        !all.iter().any(|line| line.kind == "steering_applied"),
        "no held message was written after shutdown: {all:?}"
    );
}

#[test]
fn a_shutdown_during_turn_start_from_another_thread_writes_no_context() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(added(vec![("fiber.test/inbox", "late")]));
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    hooks.pause_in_start(started_tx, release_rx);
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks.clone()));
    let signal = Arc::clone(&world.looped.as_ref().unwrap().cancel);
    let (ack, _) = capture();
    world
        .inbox
        .send(Delivery::Prompt(driver("c_1", "hello"), ack))
        .unwrap();
    let finished = run_turn(&mut world);
    // The hook is parked inside its call: force the shutdown to land
    // there, then release it.
    Deadline::after(TURN_DEADLINE)
        .recv(&started_rx)
        .expect("the hook parked");
    signal.shutdown(143);
    release_tx.send(()).unwrap();
    assert_eq!(
        finish_turn(&mut world, &finished),
        Some(TurnOutcome::Interrupted)
    );
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "turn_completed",
        ],
        "complete ordered kinds"
    );
    assert!(
        !all.iter().any(|line| line.kind == "context_added"),
        "no context is written: {all:?}"
    );
}

#[test]
fn a_shutdown_after_the_turn_start_check_writes_no_context() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(added(vec![("fiber.test/inbox", "late")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let mut looped = world.looped.take().unwrap();
    let turn = TurnId("t_test".into());
    assert!(looped.cancel.arm());
    // Forced after the post-hook `turn_cancelled` check, before the
    // guarded answer write: without the guard the contexts below would be
    // written despite the shutdown.
    let signal = Arc::clone(&looped.cancel);
    super::AFTER_TURN_START_CHECK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || signal.shutdown(143)) as Box<dyn FnOnce()>);
    });
    let ended = looped.turn_start(&turn, &[]).unwrap();
    let Some(completed) = ended else {
        panic!("a shutdown after the check ends the turn");
    };
    assert_eq!(completed.outcome, TurnOutcome::Interrupted);
    world.looped = Some(looped);
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(kinds(&all), ["session_started"], "complete ordered kinds");
    assert!(
        !all.iter().any(|line| line.kind == "context_added"),
        "no context is written after the shutdown: {all:?}"
    );
}

#[test]
fn a_shutdown_after_step_started_writes_no_held_message() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(added(vec![("fiber.test/goal", "keep going")]));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let mut looped = world.looped.take().unwrap();
    let turn = TurnId("t_test".into());
    // The reply holds one message for the next step, written by
    // `write_held`, not yet.
    assert_eq!(looped.replied(&turn, "Done.").unwrap(), None);
    assert_eq!(looped.lifecycle.continued.len(), 1);
    assert!(looped.cancel.arm());
    // The next step's `step_started`, as `step` writes it before the held
    // messages.
    let cancel = Arc::clone(&looped.cancel);
    let wrote = cancel.commit(crate::cancel::Commit::Step, || {
        looped.append(
            &contract::events::Event::StepStarted(contract::events::Empty {}),
            &turn,
            None,
        )
    });
    assert!(wrote.is_some());
    wrote.unwrap().unwrap();
    // Forced after `step_started`, before the held messages' guarded
    // write: without the guard the message below would be written despite
    // the shutdown.
    let signal = Arc::clone(&looped.cancel);
    super::BEFORE_HELD_WRITE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || signal.shutdown(143)) as Box<dyn FnOnce()>);
    });
    looped.write_held(&turn).unwrap();
    assert!(looped.lifecycle.continued.is_empty());
    world.looped = Some(looped);
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        ["session_started", "step_started"],
        "complete ordered kinds"
    );
    assert!(
        !all.iter().any(|line| line.kind == "steering_applied"),
        "no held message was written after shutdown: {all:?}"
    );
}

#[test]
fn a_cancel_between_a_turn_start_failure_and_end_turn_ends_interrupted() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_start(failed("fiber.test/guard", "boom"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let mut looped = world.looped.take().unwrap();
    let turn = TurnId("t_test".into());
    assert!(looped.cancel.arm());
    // The hook fails while the turn is live: its outcome is `failed`.
    let completed = looped
        .turn_start(&turn, &[])
        .unwrap()
        .expect("the hook failed the turn");
    assert_eq!(completed.outcome, TurnOutcome::Failed);
    // A cancel lands after the post-hook check but before finalisation:
    // cancellation wins.
    assert!(looped.cancel.cancel());
    let outcome = looped.end_turn(&turn, completed).unwrap();
    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    world.looped = Some(looped);
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        ["session_started", "turn_completed"],
        "complete ordered kinds"
    );
    assert_eq!(
        line(&all, "turn_completed").payload["outcome"],
        "interrupted"
    );
}

#[test]
fn a_cancel_between_a_refusal_and_end_turn_ends_interrupted() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_before_model_call(refused_call("fiber.test/guard", "team budget spent"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let mut looped = world.looped.take().unwrap();
    let turn = TurnId("t_test".into());
    assert!(looped.cancel.arm());
    let completed = looped
        .model_call_allowed(&turn)
        .unwrap()
        .expect("the hook refused the request");
    assert_eq!(completed.outcome, TurnOutcome::Failed);
    assert!(looped.cancel.cancel());
    let outcome = looped.end_turn(&turn, completed).unwrap();
    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    world.looped = Some(looped);
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        ["session_started", "turn_completed"],
        "complete ordered kinds"
    );
    assert_eq!(
        line(&all, "turn_completed").payload["outcome"],
        "interrupted"
    );
}

#[test]
fn a_cancel_between_a_turn_end_failure_and_end_turn_ends_interrupted() {
    let hooks = Arc::new(FakeHooks::new());
    hooks.answer_turn_end(failed("fiber.test/goal", "boom"));
    let mut world = started(vec![fakes::Scripted::text("Done.")], Some(hooks));
    let mut looped = world.looped.take().unwrap();
    let turn = TurnId("t_test".into());
    assert!(looped.cancel.arm());
    let completed = looped
        .replied(&turn, "Done.")
        .unwrap()
        .expect("the hook failed the turn");
    assert_eq!(completed.outcome, TurnOutcome::Failed);
    assert!(looped.cancel.cancel());
    let outcome = looped.end_turn(&turn, completed).unwrap();
    assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    world.looped = Some(looped);
    drop(world.looped.take());

    let all = lines(&world);
    assert_eq!(
        kinds(&all),
        ["session_started", "turn_completed"],
        "complete ordered kinds"
    );
    assert_eq!(
        line(&all, "turn_completed").payload["outcome"],
        "interrupted"
    );
}
