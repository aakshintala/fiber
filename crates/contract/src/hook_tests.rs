use std::sync::{Mutex, Weak};

use crate::events::{Event, ExtensionStateUnset};
use crate::hook::{Hooks, StateLine};

use crate::SessionId;
use crate::clock::Wake;
use crate::hook::{BeforeModelCall, JobDelivery, SessionStart, StartReason, TurnEnd, TurnStart};
use crate::shapes::{Tokens, Usage};
use crate::tool::Cancel;

/// A cancel that never fires.
struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: Weak<dyn Wake>) {}
}

struct StateHooks {
    outbox: Mutex<Vec<Event>>,
    carried: Mutex<Vec<StateLine>>,
}

macro_rules! unused_hook {
    ($method:ident, $argument:ident, $answer:ident) => {
        fn $method(&self, _: &crate::hook::$argument<'_>) -> crate::hook::$answer {
            panic!("unexpected hook call")
        }
    };
}

impl Hooks for Never {
    unused_hook!(after_tool, AfterToolCall, AfterToolAnswer);
    unused_hook!(before_tool, BeforeToolCall, BeforeToolAnswer);
    unused_hook!(check_tool, BeforeToolCall, CheckAnswer);
    unused_hook!(before_message, BeforeMessage, BeforeMessageAnswer);
    unused_hook!(session_start, SessionStart, ContextAnswer);
    unused_hook!(turn_start, TurnStart, ContextAnswer);
    unused_hook!(before_model_call, BeforeModelCall, CheckAnswer);
    unused_hook!(turn_end, TurnEnd, ContextAnswer);
    unused_hook!(before_handoff, BeforeHandoff, BeforeHandoffAnswer);

    fn deliver_to(&self, _: std::sync::mpsc::Sender<crate::inbox::Delivery>) {}
}

impl Hooks for StateHooks {
    unused_hook!(after_tool, AfterToolCall, AfterToolAnswer);
    unused_hook!(before_tool, BeforeToolCall, BeforeToolAnswer);
    unused_hook!(check_tool, BeforeToolCall, CheckAnswer);
    unused_hook!(before_message, BeforeMessage, BeforeMessageAnswer);
    unused_hook!(session_start, SessionStart, ContextAnswer);
    unused_hook!(turn_start, TurnStart, ContextAnswer);
    unused_hook!(before_model_call, BeforeModelCall, CheckAnswer);
    unused_hook!(turn_end, TurnEnd, ContextAnswer);
    unused_hook!(before_handoff, BeforeHandoff, BeforeHandoffAnswer);

    fn deliver_to(&self, _: std::sync::mpsc::Sender<crate::inbox::Delivery>) {}

    fn take_state_lines(&self) -> Vec<Event> {
        std::mem::take(&mut *self.outbox.lock().unwrap())
    }

    fn take_change_state(&self) -> Vec<StateLine> {
        std::mem::take(&mut *self.carried.lock().unwrap())
    }

    fn admit_state_lines(&self, lines: Vec<StateLine>) -> Vec<Event> {
        let mut events = self.take_state_lines();
        events.extend(lines.into_iter().map(|line| line.into_event()));
        events
    }
}

fn state_line(key: &str, landing: u64) -> StateLine {
    StateLine {
        value: None,
        extension: "goal".into(),
        key: key.into(),
        landing,
    }
}

#[test]
fn default_wait_for_runs_returns_without_dispatching_a_hook() {
    assert!(matches!(Never.wait_for_runs(), ()));
}

#[test]
fn hook_state_line_builds_set_and_unset_events_from_its_only_identity() {
    let unset = state_line("removed", 7);
    assert_eq!(
        unset.into_event(),
        Event::ExtensionStateUnset(ExtensionStateUnset {
            extension: "goal".into(),
            key: "removed".into(),
        })
    );
    for rule in [
        crate::events::OnFork::AtPoint,
        crate::events::OnFork::Latest,
        crate::events::OnFork::Fresh,
    ] {
        let mut set = state_line("written", 8);
        set.extension = "other".into();
        set.value = Some((serde_json::json!({"n": 3}), rule));
        assert_eq!(
            set.into_event(),
            Event::ExtensionStateSet(crate::events::ExtensionStateSet {
                extension: "other".into(),
                key: "written".into(),
                value: serde_json::json!({"n": 3}),
                on_fork: rule,
            })
        );
    }
}

#[test]
fn hook_state_defaults_leave_existing_implementors_unchanged() {
    assert!(Never.take_state_lines().is_empty());
    assert!(Never.take_change_state().is_empty());
    let lines = vec![state_line("first", 1), state_line("second", 2)];
    let expected: Vec<_> = lines.iter().map(|line| line.clone().into_event()).collect();
    assert_eq!(Never.admit_state_lines(lines), expected);
    assert!(Never.admit_state_lines(Vec::new()).is_empty());
}

#[test]
fn hook_state_overrides_drain_each_buffer_once_and_admit_in_order() {
    let first = state_line("first", 1);
    let second = state_line("second", 2);
    let hooks = StateHooks {
        outbox: Mutex::new(vec![first.clone().into_event()]),
        carried: Mutex::new(vec![second.clone()]),
    };
    assert_eq!(hooks.take_state_lines(), vec![first.clone().into_event()]);
    assert!(hooks.take_state_lines().is_empty());
    assert_eq!(hooks.take_change_state(), vec![second.clone()]);
    assert!(hooks.take_change_state().is_empty());
    *hooks.outbox.lock().unwrap() = vec![first.clone().into_event()];
    assert_eq!(
        hooks.admit_state_lines(vec![second.clone()]),
        vec![first.into_event(), second.into_event()]
    );
    assert!(hooks.take_state_lines().is_empty());
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 1,
            cache_read: 0,
            cache_write: std::collections::BTreeMap::new(),
            output: 1,
        },
        cost: None,
        subscription_cost: 0.0,
    }
}

#[test]
fn session_start_debug_names_the_type() {
    let id = SessionId("s_1".into());
    let start = SessionStart {
        reason: StartReason::Start,
        session_id: &id,
        workspace: "/work",
        cancel: &Never,
    };
    let shown = format!("{start:?}");
    assert!(shown.contains("SessionStart"), "{shown}");
    assert!(shown.contains("s_1"), "{shown}");
}

#[test]
fn turn_start_debug_names_the_type() {
    let turn = TurnStart {
        input: &[],
        cancel: &Never,
    };
    let shown = format!("{turn:?}");
    assert!(shown.contains("TurnStart"), "{shown}");
}

#[test]
fn before_model_call_debug_names_the_type() {
    let used = usage();
    let call = BeforeModelCall {
        model: "anthropic/claude-x",
        usage: &used,
        cancel: &Never,
    };
    let shown = format!("{call:?}");
    assert!(shown.contains("BeforeModelCall"), "{shown}");
    assert!(shown.contains("anthropic/claude-x"), "{shown}");
}

#[test]
fn turn_end_debug_names_the_type() {
    let end = TurnEnd {
        reply: "Done.",
        cancel: &Never,
    };
    let shown = format!("{end:?}");
    assert!(shown.contains("TurnEnd"), "{shown}");
    assert!(shown.contains("Done."), "{shown}");
}

#[test]
fn job_delivery_names_each_wire_word() {
    let cases = [
        (JobDelivery::Completion, "completion"),
        (JobDelivery::Monitor, "monitor"),
        (JobDelivery::Delegate, "delegate"),
        (JobDelivery::OutputFile, "output_file"),
    ];
    for (delivery, word) in cases {
        assert_eq!(delivery.as_str(), word);
    }
}

#[test]
fn a_not_regular_chain_without_hooks_is_no_pass_to_fail() {
    let answer = crate::hook::not_regular_answer(&[]);
    assert_eq!(
        answer,
        crate::hook::AfterToolAnswer {
            outcome: crate::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    );
}

#[test]
fn a_not_regular_chain_withholds_on_its_first_blocking_hook() {
    let answer = crate::hook::not_regular_answer(&[
        ("alpha".into(), false),
        ("beta".into(), true),
        ("gamma".into(), true),
    ]);
    assert_eq!(
        answer.outcome,
        crate::hook::AfterToolOutcome::Withheld {
            extension: "beta".into(),
        }
    );
    assert!(answer.notices.is_empty());
}

#[test]
fn a_not_regular_chain_of_non_blocking_hooks_names_each_in_a_notice() {
    let answer =
        crate::hook::not_regular_answer(&[("alpha".into(), false), ("beta".into(), false)]);
    assert_eq!(answer.outcome, crate::hook::AfterToolOutcome::Unchanged);
    assert!(answer.changed_by.is_empty());
    let extensions: Vec<_> = answer
        .notices
        .iter()
        .map(|notice| {
            notice
                .extension
                .clone()
                .expect("each notice names its extension")
        })
        .collect();
    assert_eq!(extensions, vec!["alpha".to_owned(), "beta".to_owned()]);
    for notice in &answer.notices {
        assert_eq!(notice.code, crate::ErrorCode::HookFailed);
        assert!(
            notice.message.contains("not a regular file"),
            "unexpected notice: {}",
            notice.message
        );
    }
}
