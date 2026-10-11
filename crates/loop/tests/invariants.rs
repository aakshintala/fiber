//! A cancelled call ends in one completion with status `cancelled`,
//! and nothing from it follows (`docs/testing.md`, "Invariants"):
//! generated call mixes, cancelled while some calls run.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::Envelope;
use contract::ErrorCode;
use contract::events::{Control, Event, Progress, TurnOutcome};
use contract::shapes::Question;
use contract::tool::Tool;
use r#loop::{Loop, TurnCancel};
use proptest::prelude::*;
use serde_json::Value;

use support::{Gate, Script, Session, TestTool, calls_reply, delivery, paris, text_all};

/// One wait per case: the turn runs on a thread, and the test receives
/// it with this deadline.
const CASE_DEADLINE: Duration = Duration::from_secs(2);

/// The tools' names, one per call index. `TestTool.name` takes
/// `&'static str`.
const NAMES: [&str; 4] = ["t0", "t1", "t2", "t3"];

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 24,
        max_shrink_time: 6_000,
        ..ProptestConfig::default()
    }
}

/// Runs the session's turn on its own thread, sending the loop and its
/// outcome back. It has no wait of its own.
fn spawn_turn(session: &mut Session) -> mpsc::Receiver<(Loop, Option<TurnOutcome>)> {
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    std::thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    finished
}

/// The turn's outcome and every line through its `turn_completed`: one
/// [`CASE_DEADLINE`] for both ("A wait's deadline is made once"). Plain
/// `recv` inside `within` is the shape `support::read_until` uses. The
/// caller puts the returned `Loop` back as `session.looped`.
fn finish_turn(
    mut watcher: log::Watcher,
    ended: mpsc::Receiver<(Loop, Option<TurnOutcome>)>,
) -> (log::Watcher, Vec<Envelope>, Loop, Option<TurnOutcome>) {
    fakes::within("the turn's end and its lines", CASE_DEADLINE, move || {
        let (looped, outcome) = ended.recv().unwrap();
        let mut lines = Vec::new();
        loop {
            match watcher.recv() {
                Ok(Some(line)) => {
                    let last = line.kind == "turn_completed";
                    lines.push(line);
                    if last {
                        return (watcher, lines, looped, outcome);
                    }
                }
                Ok(None) => panic!("the log ended before turn_completed"),
                Err(err) => panic!("the log failed before turn_completed: {err}"),
            }
        }
    })
}

/// Every line until `done` accepts the lines accumulated so far, under one
/// [`CASE_DEADLINE`]. The predicate runs on the lines already accumulated
/// before another is received, so results written while an earlier wait
/// ran still count; every line is kept for the final asserts.
fn read_lines_until(
    mut watcher: log::Watcher,
    mut lines: Vec<Envelope>,
    what: &str,
    done: impl Fn(&[Envelope]) -> bool + Send + 'static,
) -> (log::Watcher, Vec<Envelope>) {
    let what_owned = what.to_owned();
    fakes::within(what, CASE_DEADLINE, move || {
        loop {
            if done(&lines) {
                return (watcher, lines);
            }
            match watcher.recv() {
                Ok(Some(line)) => lines.push(line),
                Ok(None) => panic!("the log ended before {what_owned}"),
                Err(err) => panic!("the log failed before {what_owned}: {err}"),
            }
        }
    })
}

/// The lines after `turn_completed`, without blocking.
fn drain(watcher: &mut log::Watcher) -> Vec<Envelope> {
    let mut lines = Vec::new();
    loop {
        match watcher.try_recv() {
            Ok(Some(line)) => lines.push(line),
            Ok(None) => return lines,
            Err(err) => panic!("the watcher's queue failed: {err}"),
        }
    }
}

/// One generated call: what its tool does when the turn is cancelled.
#[derive(Clone, Copy, Debug)]
enum CallKind {
    /// Runs its script, then returns.
    Returns { delta: bool, control: bool },
    /// Fails at once.
    Fails,
    /// Runs its script, then stops on the cancel.
    Stops { delta: bool, control: bool },
    /// Runs its script, then waits for the test to release it.
    Held { delta: bool, control: bool },
}

impl CallKind {
    fn has_delta(self) -> bool {
        match self {
            CallKind::Returns { delta, .. }
            | CallKind::Stops { delta, .. }
            | CallKind::Held { delta, .. } => delta,
            CallKind::Fails => false,
        }
    }

    fn has_control(self) -> bool {
        match self {
            CallKind::Returns { control, .. }
            | CallKind::Stops { control, .. }
            | CallKind::Held { control, .. } => control,
            CallKind::Fails => false,
        }
    }

    /// The text its tool returns.
    fn text(self, index: usize) -> String {
        match self {
            CallKind::Fails => "It broke.".to_owned(),
            CallKind::Returns { .. } | CallKind::Stops { .. } | CallKind::Held { .. } => {
                format!("r{index}")
            }
        }
    }
}

/// Opens every held tool when the case panics, so a `Wait`/`WaitCancel`
/// tool is freed at once instead of at the support bound: the "test ending
/// or panicking drops the sender, which releases it too" rule. Dropped at
/// the end of the case, after the asserts.
struct Release {
    gates: Vec<Arc<Gate>>,
    cancel: Arc<TurnCancel>,
}

impl Drop for Release {
    fn drop(&mut self) {
        for gate in &self.gates {
            gate.open();
        }
        self.cancel.cancel();
    }
}

fn call_kind() -> impl Strategy<Value = CallKind> {
    use CallKind::{Fails, Held, Returns, Stops};
    prop_oneof![
        (any::<bool>(), any::<bool>()).prop_map(|(delta, control)| Returns { delta, control }),
        Just(Fails),
        (any::<bool>(), any::<bool>()).prop_map(|(delta, control)| Stops { delta, control }),
        (any::<bool>(), any::<bool>()).prop_map(|(delta, control)| Held { delta, control }),
    ]
}

/// 1..=4 calls, at least one `Stops` or `Held`: a call the cancel must
/// reach mid-run.
fn plan() -> impl Strategy<Value = Vec<CallKind>> {
    prop::collection::vec(call_kind(), 1..=4).prop_filter(
        "a Stops or Held call to cancel",
        |kinds: &Vec<CallKind>| {
            kinds
                .iter()
                .any(|kind| matches!(kind, CallKind::Stops { .. } | CallKind::Held { .. }))
        },
    )
}

fn delta(text: String) -> Script {
    Script::Emit(Box::new(Event::ToolCallDelta(Progress {
        text: Some(text),
        details: None,
    })))
}

fn question(index: usize) -> Question {
    Question {
        header: format!("q{index}"),
        question: format!("May {index} continue?"),
        options: Vec::new(),
        multi_select: None,
    }
}

#[track_caller]
fn run_case(plan: Vec<CallKind>) -> Result<(), TestCaseError> {
    let n = plan.len();
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut step_gates: Vec<Option<Arc<Gate>>> = Vec::new();
    let mut held_gates: Vec<Option<Arc<Gate>>> = Vec::new();
    let mut calls: Vec<(&str, Value)> = Vec::new();
    for (index, kind) in plan.iter().enumerate() {
        let name = NAMES[index];
        calls.push((name, paris()));
        let mut tool = match kind {
            CallKind::Fails => TestTool::failing(name, ErrorCode::ToolError),
            CallKind::Returns { .. } | CallKind::Stops { .. } | CallKind::Held { .. } => {
                TestTool::reads(name, &kind.text(index))
            }
        };
        let mut script = Vec::new();
        if kind.has_delta() {
            let step = Arc::new(Gate::default());
            script.push(delta(format!("a{index}")));
            script.push(Script::Wait(Arc::clone(&step)));
            script.push(delta(format!("b{index}")));
            step_gates.push(Some(step));
        } else {
            step_gates.push(None);
        }
        match kind {
            CallKind::Returns { .. } | CallKind::Fails => held_gates.push(None),
            CallKind::Stops { .. } => {
                script.push(Script::WaitCancel);
                held_gates.push(None);
            }
            CallKind::Held { .. } => {
                let held = Arc::new(Gate::default());
                script.push(Script::Wait(Arc::clone(&held)));
                held_gates.push(Some(held));
            }
        }
        tool.script = script;
        if kind.has_control() {
            tool.output.control = Some(Control {
                handoff: None,
                questions: Some(vec![question(index)]),
                skill: None,
                name: Some(format!("named {index}")),
            });
        }
        tools.push(Arc::new(tool));
    }

    let mut session = Session::with_tools(
        vec![calls_reply("", &calls), fakes::Scripted::text("Done.")],
        None,
        tools,
    );
    session.inbox.send(delivery("hi")).unwrap();
    let _release = Release {
        gates: step_gates
            .iter()
            .flatten()
            .chain(held_gates.iter().flatten())
            .cloned()
            .collect(),
        cancel: Arc::clone(&session.cancel),
    };
    let watcher = session.log.watch();
    let ended = spawn_turn(&mut session);

    // Wait 1: every call started, and the first delta of every call with
    // one. A first delta after idle is due at once, so each arrives
    // without any release.
    let firsts: Vec<String> = plan
        .iter()
        .enumerate()
        .filter(|(_, kind)| kind.has_delta())
        .map(|(index, _)| format!("a{index}"))
        .collect();
    let (watcher, lines) = read_lines_until(
        watcher,
        Vec::new(),
        "the calls' starts and first deltas",
        move |lines| {
            lines
                .iter()
                .filter(|line| line.kind == "tool_call_started")
                .count()
                == n
                && firsts.iter().all(|first| {
                    lines.iter().any(|line| {
                        line.kind == "tool_call_delta"
                            && line.payload.get("text").and_then(Value::as_str) == Some(first)
                    })
                })
        },
    );

    // Every tool runs to the end of its script: a `Returns` tool that
    // returns flushes its held delta at once, which proves it returned.
    for gate in step_gates.iter().flatten() {
        gate.open();
    }

    // Wait 2: the leading `Returns`/`Fails` run completed, and every
    // returned `Returns` call's second delta flushed. A later `Returns`
    // with no delta gives no signal that it returned; its status is the
    // same either way.
    let p = plan
        .iter()
        .take_while(|kind| matches!(kind, CallKind::Returns { .. } | CallKind::Fails))
        .count();
    let flushed: Vec<String> = plan
        .iter()
        .enumerate()
        .filter(|(_, kind)| matches!(kind, CallKind::Returns { delta: true, .. }))
        .map(|(index, _)| format!("b{index}"))
        .collect();
    let (watcher, lines) = read_lines_until(
        watcher,
        lines,
        "the prefix completions and flushed deltas",
        move |lines| {
            lines
                .iter()
                .filter(|line| line.kind == "tool_call_completed")
                .count()
                == p
                && flushed.iter().all(|second| {
                    lines.iter().any(|line| {
                        line.kind == "tool_call_delta"
                            && line.payload.get("text").and_then(Value::as_str) == Some(second)
                    })
                })
        },
    );

    prop_assert!(session.cancel.cancel());
    for gate in held_gates.iter().flatten() {
        gate.open();
    }

    let (mut watcher, tail, looped, outcome) = finish_turn(watcher, ended);
    session.looped = Some(looped);
    let mut all = lines;
    all.extend(tail);
    all.extend(drain(&mut watcher));
    let all: Vec<Envelope> = all
        .into_iter()
        .filter(|line| line.kind != "session_status")
        .collect();

    prop_assert_eq!(outcome, Some(TurnOutcome::Interrupted));
    let ends: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "turn_completed")
        .collect();
    prop_assert_eq!(ends.len(), 1);
    prop_assert_eq!(ends[0].payload.len(), 1);
    prop_assert_eq!(ends[0].payload["outcome"].as_str().unwrap(), "interrupted");
    prop_assert_eq!(
        all.iter()
            .filter(|line| line.kind == "turn_started")
            .count(),
        1
    );
    prop_assert_eq!(session.requests().len(), 1);
    for (index, gate) in step_gates.iter().flatten().enumerate() {
        gate.check(&format!("step {index}"));
    }
    for (index, gate) in held_gates.iter().flatten().enumerate() {
        gate.check(&format!("held {index}"));
    }

    let requested: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "tool_call_requested")
        .collect();
    prop_assert_eq!(requested.len(), n);
    let done: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "tool_call_completed")
        .collect();
    prop_assert_eq!(done.len(), n);
    let requested_ids: Vec<Option<contract::ActionId>> = requested
        .iter()
        .map(|line| line.action_id.clone())
        .collect();
    let done_ids: Vec<Option<contract::ActionId>> =
        done.iter().map(|line| line.action_id.clone()).collect();
    prop_assert_eq!(&done_ids, &requested_ids);

    for (index, kind) in plan.iter().enumerate() {
        let id = requested[index].action_id.clone();
        let mine: Vec<&&Envelope> = done.iter().filter(|line| line.action_id == id).collect();
        prop_assert_eq!(mine.len(), 1);
        let mine = mine[0];
        let expected = match kind {
            CallKind::Returns { .. } if index < p => "completed",
            CallKind::Fails => "failed",
            CallKind::Returns { .. } | CallKind::Stops { .. } | CallKind::Held { .. } => {
                "cancelled"
            }
        };
        prop_assert_eq!(mine.payload["status"].as_str().unwrap(), expected);
        prop_assert_eq!(text_all(mine), kind.text(index));
        let deltas: Vec<&Envelope> = all
            .iter()
            .filter(|line| line.kind == "tool_call_delta" && line.action_id == id)
            .collect();
        if kind.has_delta() {
            prop_assert_eq!(deltas.len(), 2);
            prop_assert_eq!(
                deltas[0].payload["text"].as_str().unwrap(),
                format!("a{index}")
            );
            prop_assert_eq!(
                deltas[1].payload["text"].as_str().unwrap(),
                format!("b{index}")
            );
        } else {
            prop_assert!(deltas.is_empty());
        }
        let at = all
            .iter()
            .position(|line| line.kind == "tool_call_completed" && line.action_id == id)
            .unwrap();
        prop_assert!(
            all[at + 1..]
                .iter()
                .filter(|line| line.kind == "tool_call_delta")
                .all(|line| line.action_id != id)
        );
        let named: Vec<&Envelope> = all
            .iter()
            .filter(|line| line.kind == "session_named" && line.action_id == id)
            .collect();
        if kind.has_control() && expected == "completed" {
            prop_assert_eq!(named.len(), 1);
            prop_assert_eq!(
                named[0].payload["name"].as_str().unwrap(),
                format!("named {index}")
            );
        } else {
            prop_assert!(named.is_empty());
        }
    }

    let end = all
        .iter()
        .position(|line| line.kind == "turn_completed")
        .unwrap();
    let silent = all[end + 1..].iter().all(|line| match &line.action_id {
        Some(id) => !requested_ids.contains(&Some(id.clone())),
        None => true,
    });
    prop_assert!(silent);
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    /// A cancelled call ends in exactly one completion with status
    /// `cancelled`, and nothing from it follows (`docs/testing.md`,
    /// "Invariants"; ruling issuecomment-6113964257). The property covers
    /// the running-call and held-delta orders; the as-the-next-request-is-made
    /// order is covered by `cancel.rs` only.
    /// Faults it catches:
    /// - F1: `complete_next` rewriting or adding a completion for a call
    ///   already done, or `finish` stamping `cancelled` on a result written
    ///   before the cancel (a prefix call stays `completed`, one completion
    ///   per call).
    /// - F2: `finish` dropping content on cancel, or the status chosen when
    ///   the tool returns rather than when its result is written (a call that
    ///   returned behind a running call completes `cancelled` with its own
    ///   text).
    /// - F3: a held final delta lost, or written after its call's completion
    ///   (two deltas, in order, both before the completion).
    /// - F4: `naming.rs` `Naming::judge` accepting any status (a
    ///   `session_named` only for a `completed` call).
    /// - F5: a line for a call written after `turn_completed`.
    #[test]
    fn a_cancelled_call_ends_in_one_cancelled_completion_and_nothing_follows(
        plan in plan(),
    ) {
        run_case(plan)?;
    }
}
