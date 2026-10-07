//! Resuming what a crash left behind (`docs/testing.md`, "Invariants"):
//! generated crash and suspend histories, where a call whose fate is
//! unknown never runs again.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::collections::BTreeMap;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::events::{
    AskStep, CallStatus, Empty, Event, FiberExited, FiberStarted, InputItem, PermissionRequested,
    RuleScope, SessionStarted, StandingRule, ToolCallCompleted, ToolCallRequested, ToolCallStarted,
    TurnOutcome, TurnStarted, Variables, VariablesSource,
};
use contract::inbox::Delivery;
use contract::provider::{Input, Provider};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender, Tokens, Usage};
use contract::tool::Tool;
use contract::{ActionId, RequestId, SessionId};
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use proptest::prelude::*;
use serde_json::json;

/// One wait per case: the resumed turn runs on a thread, and the test
/// receives it with this deadline.
const CASE_DEADLINE: Duration = Duration::from_secs(2);

/// What a crash left behind for one call.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fate {
    NeverRan,
    Unknown,
    Completed,
}

/// How the generated history's process ended.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Tail {
    Crash,
    Suspended,
}

/// Each step's calls, in request order.
type Steps = Vec<Vec<Fate>>;

/// 1..=4 steps of 1..=3 calls, each cut at a crash point into the shapes
/// the live loop writes: every start before any completion, and completions
/// a request-order prefix of the started calls. At least one call is
/// uncertain.
fn steps() -> impl Strategy<Value = Steps> {
    prop::collection::vec((1usize..=3).prop_flat_map(|n| (Just(n), 0..=2 * n)), 1..=4)
        .prop_map(|shapes| {
            shapes
                .into_iter()
                .map(|(n, p)| {
                    let started = p.min(n);
                    let completed = p.saturating_sub(n);
                    (0..n)
                        .map(|j| {
                            if j < completed {
                                Fate::Completed
                            } else if j < started {
                                Fate::Unknown
                            } else {
                                Fate::NeverRan
                            }
                        })
                        .collect()
                })
                .collect()
        })
        .prop_filter("an uncertain call", |steps: &Steps| {
            steps.iter().flatten().any(|fate| *fate == Fate::Unknown)
        })
}

/// A history log with helpers, then a resumed loop on it.
struct History {
    _root: fakes::TempDir,
    dir: std::path::PathBuf,
    log: Arc<Log>,
    workspace: String,
    credentials: std::path::PathBuf,
    rules: Arc<support::FakeRules>,
    provider: Arc<ScriptedProvider>,
    inbox_tx: mpsc::Sender<Delivery>,
    inbox_rx: Option<mpsc::Receiver<Delivery>>,
    clock: Arc<fakes::clock::FakeClock>,
}

impl History {
    fn new(script: Vec<Scripted>) -> Self {
        let root = fakes::TempDir::new("fiber-resume-invariants");
        let workspace_dir = root.path().join("w");
        std::fs::create_dir_all(&workspace_dir).unwrap();
        let credentials = root.path().join("credentials");
        std::fs::create_dir_all(&credentials).unwrap();
        let workspace = workspace_dir.display().to_string();
        let clock = fakes::clock::FakeClock::new();
        let log = Arc::new(
            Log::create(
                root.path(),
                SessionId("s_1".into()),
                Arc::clone(&clock) as Arc<dyn contract::clock::Clock>,
            )
            .unwrap(),
        );
        let dir = root.path().join("s_1");
        let (tx, rx) = mpsc::channel();
        let history = Self {
            _root: root,
            dir,
            log,
            workspace: workspace.clone(),
            credentials,
            rules: Arc::new(support::FakeRules::empty()),
            provider: Arc::new(ScriptedProvider::new(script)),
            inbox_tx: tx,
            inbox_rx: Some(rx),
            clock,
        };
        history
            .log
            .append(
                &Event::SessionStarted(SessionStarted {
                    workspace,
                    variables: Variables {
                        path: String::new(),
                        names: Vec::new(),
                        source: VariablesSource::Inherited,
                    },
                    parent: None,
                    forked_from: None,
                    rewind: None,
                }),
                None,
                None,
            )
            .unwrap();
        history
    }

    fn write(&self, event: Event, turn: &str, action: Option<&str>) {
        self.log
            .append(
                &event,
                Some(contract::TurnId(turn.into())),
                action.map(|a| ActionId(a.into())),
            )
            .unwrap();
    }

    /// Resumes headless: no person can answer an approval.
    fn resume(&mut self, tools: Vec<(String, Arc<dyn Tool>)>) -> r#loop::Loop {
        let root = self._root.path().to_path_buf();
        r#loop::Loop::resume(
            Arc::clone(&self.log),
            r#loop::resumed(&self.dir).unwrap(),
            Arc::clone(&self.provider) as Arc<dyn Provider>,
            r#loop::Model {
                reference: support::MODEL.into(),
                cost: None,
                subscription: false,
            },
            r#loop::PromptInputs::new(
                root.clone(),
                "/bin/sh".into(),
                root.join("events.jsonl").display().to_string(),
                Arc::clone(&self.clock) as Arc<dyn contract::clock::Clock>,
                fakes::CONTEXT_WINDOW,
            ),
            self.inbox_rx.take().unwrap(),
            tools,
            r#loop::Permissions {
                workspace: self.workspace.clone(),
                credentials: self.credentials.clone(),
                credential_files: Vec::new(),
                rules: self.rules.clone(),
            },
        )
        .unwrap()
        .answerable(false)
    }
}

fn user_turn(text: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text { text: text.into() }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: Some(contract::CommandId("c_1".into())),
            },
            changed_by: None,
        }],
    })
}

fn requested(name: &str, city: &str) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.into(),
        arguments: json!({"city": city}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn started() -> Event {
    Event::ToolCallStarted(ToolCallStarted {
        declared: DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
        },
        arguments: None,
        changed_by: None,
    })
}

fn completed(text: &str) -> Event {
    Event::ToolCallCompleted(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text { text: text.into() }],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    })
}

fn message_started() -> Event {
    Event::AssistantMessageStarted(Empty {})
}

fn fiber_started() -> Event {
    Event::FiberStarted(FiberStarted {
        version: "0.0.0".into(),
        resumed: false,
    })
}

fn fiber_exited(suspended_on: Option<&str>) -> Event {
    Event::FiberExited(FiberExited {
        exit_code: 0,
        usage: Usage {
            tokens: Tokens {
                input: 0,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 0,
            },
            cost: Some(0.0),
            subscription_cost: 0.0,
        },
        final_message: None,
        error: None,
        suspended_on: suspended_on.map(|id| RequestId(id.into())),
        questions: None,
    })
}

fn standing_request(request_id: &str) -> Event {
    Event::PermissionRequested(PermissionRequested {
        request_id: RequestId(request_id.into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: true,
            paths: None,
        },
        step: AskStep::StandingAsk {
            standing_rule: StandingRule {
                scope: RuleScope::Project,
                prefix: "run tests".into(),
            },
        },
    })
}

/// Writes `steps` as consecutive turns, crashing wherever a step leaves a
/// call without a result, and closes with `tail`. Returns every history
/// call's action id with its fate.
fn write_history(history: &History, steps: &Steps, tail: Tail) -> Vec<(String, Fate)> {
    let mut calls: Vec<(String, Fate)> = Vec::new();
    let mut turn = 1;
    history.write(user_turn("one"), &format!("t_{turn}"), None);
    for (i, step) in steps.iter().enumerate() {
        let mid = format!("m_{i}");
        history.write(message_started(), &format!("t_{turn}"), Some(&mid));
        let base = calls.len();
        for (j, fate) in step.iter().enumerate() {
            let id = format!("a_{i}_{j}");
            history.write(requested("read", &id), &format!("t_{turn}"), Some(&id));
            calls.push((id, *fate));
        }
        // As `run_batch` writes them: every start before any completion.
        for (id, fate) in &calls[base..] {
            if *fate != Fate::NeverRan {
                history.write(started(), &format!("t_{turn}"), Some(id));
            }
        }
        for (id, fate) in &calls[base..] {
            if *fate == Fate::Completed {
                history.write(
                    completed(&format!("done {id}")),
                    &format!("t_{turn}"),
                    Some(id),
                );
            }
        }
        let crashed = step.iter().any(|fate| *fate != Fate::Completed);
        let followed = i + 1 < steps.len() || tail == Tail::Suspended;
        if crashed && followed {
            history.write(fiber_started(), &format!("t_{turn}"), None);
            turn += 1;
            history.write(user_turn("more"), &format!("t_{turn}"), None);
        }
    }
    if tail == Tail::Suspended {
        history.write(message_started(), &format!("t_{turn}"), Some("m_s"));
        history.write(requested("read", "a_s"), &format!("t_{turn}"), Some("a_s"));
        history.write(standing_request("r_9"), &format!("t_{turn}"), Some("a_s"));
        history.write(fiber_exited(Some("r_9")), &format!("t_{turn}"), None);
    }
    calls
}

fn check(steps: Steps, tail: Tail) -> Result<(), TestCaseError> {
    let tool = Arc::new(support::TestTool::reads("read", "Paris."));
    let mut history = History::new(vec![
        support::calls_reply("Again.", &[("read", json!({"city": "fresh"}))]),
        Scripted::text("Done."),
    ]);
    let calls = write_history(&history, &steps, tail);
    let unknown: Vec<&str> = calls
        .iter()
        .filter(|(_, fate)| *fate == Fate::Unknown)
        .map(|(id, _)| id.as_str())
        .collect();
    let mut history_ids: Vec<String> = calls.iter().map(|(id, _)| id.clone()).collect();
    for (i, _) in steps.iter().enumerate() {
        history_ids.push(format!("m_{i}"));
    }
    if tail == Tail::Suspended {
        history_ids.push("a_s".into());
    }
    let history_len = log::read(&history.dir).unwrap().len();

    let mut looped = history.resume(vec![("builtin".into(), tool.clone() as Arc<dyn Tool>)]);
    if tail == Tail::Crash {
        history
            .inbox_tx
            .send(Delivery::Prompt(
                support::message("next"),
                support::ignore(),
            ))
            .unwrap();
    }
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        done.send(looped.turn()).unwrap();
    });
    let outcome = finished
        .recv_timeout(CASE_DEADLINE)
        .expect("the resumed turn ended before the deadline");
    prop_assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));

    // The fresh call ran exactly once, and no uncertain call ran again.
    let ran = tool.ran();
    let fresh_runs = ran
        .iter()
        .filter(|args| args.get("city").and_then(|city| city.as_str()) == Some("fresh"))
        .count();
    prop_assert_eq!(fresh_runs, 1);
    for args in &ran {
        let city = args.get("city").and_then(|city| city.as_str()).unwrap();
        prop_assert!(!unknown.contains(&city));
    }

    let lines = log::read(&history.dir).unwrap();
    let new = &lines[history_len..];
    // No uncertain call started again, and exactly one new start names an
    // action id outside the history: the fresh call.
    let starts: Vec<&str> = new
        .iter()
        .filter(|line| line.kind == "tool_call_started")
        .map(|line| line.action_id.as_ref().unwrap().0.as_str())
        .collect();
    for id in &starts {
        prop_assert!(!unknown.contains(id));
    }
    let fresh_starts = starts
        .iter()
        .filter(|id| !history_ids.iter().any(|known| known.as_str() == **id))
        .count();
    prop_assert_eq!(fresh_starts, 1);

    // Every history call has exactly one fixed result, naming its fate.
    let requests = history.provider.requests();
    prop_assert!(!requests.is_empty());
    let conversation = &requests[0].conversation;
    for (id, fate) in &calls {
        let results: Vec<(&str, bool)> = conversation
            .iter()
            .filter_map(|input| match input {
                Input::ToolResult {
                    action_id,
                    text,
                    is_error,
                    ..
                } if action_id.0 == *id => Some((text.as_str(), *is_error)),
                Input::ToolCall { .. }
                | Input::ToolResult { .. }
                | Input::User { .. }
                | Input::Assistant { .. }
                | Input::Reasoning { .. } => None,
            })
            .collect();
        prop_assert_eq!(results.len(), 1);
        let (text, is_error) = results[0];
        match fate {
            Fate::Completed => {
                prop_assert_eq!(text, format!("done {id}"));
                prop_assert!(!is_error);
            }
            Fate::Unknown => {
                prop_assert_eq!(text, "Its outcome is unknown: it may have run.");
                prop_assert!(is_error);
            }
            Fate::NeverRan => {
                prop_assert_eq!(text, "It never ran.");
                prop_assert!(is_error);
            }
        }
    }

    // The complete, ordered list of durable kinds the resume wrote.
    let kinds: Vec<&str> = new.iter().map(|line| line.kind.as_str()).collect();
    let (expected, statuses): (&[&str], &[&str]) = match tail {
        Tail::Suspended => (
            &[
                "preamble_built",
                "opening_message",
                "permission_requested",
                "permission_resolved",
                "tool_call_completed",
                "step_started",
                "assistant_message_started",
                "text_completed",
                "tool_call_requested",
                "usage_recorded",
                "assistant_message_completed",
                "tool_call_started",
                "tool_call_completed",
                "step_started",
                "assistant_message_started",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
            ],
            &["denied", "completed"],
        ),
        Tail::Crash => (
            &[
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
                "assistant_message_started",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
            ],
            &["completed"],
        ),
    };
    prop_assert_eq!(kinds, expected);
    let ran_statuses: Vec<&str> = new
        .iter()
        .filter(|line| line.kind == "tool_call_completed")
        .map(|line| line.payload["status"].as_str().unwrap())
        .collect();
    prop_assert_eq!(ran_statuses, statuses);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 10, max_shrink_iters: 10, ..ProptestConfig::default() })]

    /// A crashed history resumes without running an uncertain call again.
    #[test]
    fn crash_resume_never_runs_an_uncertain_call_again(steps in steps()) {
        check(steps, Tail::Crash)?;
    }

    /// A suspended history's finishing turn never runs an uncertain call
    /// again.
    #[test]
    fn suspended_resume_never_runs_an_uncertain_call_again(steps in steps()) {
        check(steps, Tail::Suspended)?;
    }
}
