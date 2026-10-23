//! A cancelled call ends in one completion with status `cancelled`,
//! and nothing from it follows (`docs/testing.md`, "Invariants"):
//! generated call mixes, cancelled while some calls run. Results return
//! in request order whatever order the tools return in, and no accepted
//! input is lost or repeated: generated orders and generated turns.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};

use contract::Envelope;
use contract::ErrorCode;
use contract::events::{CacheLifetime, Control, Event, Progress, TurnOutcome};
use contract::inbox::{Ack, Delivery};
use contract::provider::{Input, ModelCall, ModelRequest, Provider, ToolDefinition};
use contract::shapes::Question;
use contract::tool::Tool;
use fakes::Deadline;
use fakes::ScriptedProvider;
use r#loop::{Loop, TurnCancel};
use proptest::prelude::*;
use serde_json::{Map, Value};

use support::{Gate, Script, Session, TestTool, calls_reply, delivery, message, paris, text_all};

/// The tools' names, one per call index. `TestTool.name` takes
/// `&'static str`.
const NAMES: [&str; 4] = ["t0", "t1", "t2", "t3"];

fn config() -> ProptestConfig {
    ProptestConfig {
        cases: 24,
        // A failing run is the failing case (10 s), the 6 s shrink search and
        // one final shrink attempt (10 s): about 26 s, under the 120 s stop
        // that mutation testing gives a test run.
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
/// [`MUST_SUCCEED_WITHIN`] for both ("A wait's deadline is made once"). Plain
/// `recv` inside `within` is the shape `support::read_until` uses. The
/// caller puts the returned `Loop` back as `session.looped`.
fn finish_turn(
    mut watcher: log::Watcher,
    ended: mpsc::Receiver<(Loop, Option<TurnOutcome>)>,
) -> (log::Watcher, Vec<Envelope>, Loop, Option<TurnOutcome>) {
    fakes::within(
        "the turn's end and its lines",
        Deadline::after(MUST_SUCCEED_WITHIN),
        move || {
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
        },
    )
}

/// Every line until `done` accepts the lines accumulated so far, under one
/// [`MUST_SUCCEED_WITHIN`]. The predicate runs on the lines already accumulated
/// before another is received, so results written while an earlier wait
/// ran still count; every line is kept for the final asserts.
fn read_lines_until(
    mut watcher: log::Watcher,
    mut lines: Vec<Envelope>,
    what: &str,
    done: impl Fn(&[Envelope]) -> bool + Send + 'static,
) -> (log::Watcher, Vec<Envelope>) {
    let what_owned = what.to_owned();
    fakes::within(what, Deadline::after(MUST_SUCCEED_WITHIN), move || {
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
    prop_assert_eq!(ends[0].payload["outcome"].as_str().unwrap(), "interrupted");
    prop_assert!(ends[0].payload.get("questions").is_none());
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

/// One generated call for the order property: it always returns,
/// either with its text or with an error, optionally carrying questions.
#[derive(Clone, Copy, Debug)]
enum OrderKind {
    Returns,
    Fails { asks: bool },
}

impl OrderKind {
    /// The text its tool returns.
    fn text(self, index: usize) -> String {
        match self {
            OrderKind::Returns => format!("r{index}"),
            OrderKind::Fails { .. } => "It broke.".to_owned(),
        }
    }

    fn is_error(self) -> bool {
        matches!(self, OrderKind::Fails { .. })
    }
}

fn order_kind() -> impl Strategy<Value = OrderKind> {
    prop_oneof![
        Just(OrderKind::Returns),
        any::<bool>().prop_map(|asks| OrderKind::Fails { asks }),
    ]
}

/// 1..=4 returning calls, and the order their tools return in: a
/// permutation of `0..n`.
fn order_plan() -> impl Strategy<Value = (Vec<OrderKind>, Vec<usize>)> {
    prop::collection::vec(order_kind(), 1..=4).prop_flat_map(|calls| {
        let n = calls.len();
        Just((0..n).collect::<Vec<usize>>())
            .prop_shuffle()
            .prop_map(move |perm| (calls.clone(), perm))
    })
}

#[track_caller]
fn run_order_case(calls: Vec<OrderKind>, perm: Vec<usize>) -> Result<(), TestCaseError> {
    let n = calls.len();
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut gates: Vec<Arc<Gate>> = Vec::new();
    let mut args: Vec<(&str, Value)> = Vec::new();
    for (index, kind) in calls.iter().enumerate() {
        let name = NAMES[index];
        args.push((name, paris()));
        let mut tool = match kind {
            OrderKind::Returns => TestTool::reads(name, &kind.text(index)),
            OrderKind::Fails { .. } => TestTool::failing(name, ErrorCode::ToolError),
        };
        let gate = Arc::new(Gate::default());
        tool.script = vec![
            delta(format!("a{index}")),
            Script::Wait(Arc::clone(&gate)),
            delta(format!("b{index}")),
        ];
        gates.push(gate);
        if matches!(kind, OrderKind::Fails { asks: true }) {
            tool.output.control = Some(Control {
                handoff: None,
                questions: Some(vec![question(index)]),
                skill: None,
                name: None,
            });
        }
        tools.push(Arc::new(tool));
    }

    let mut session = Session::with_tools(
        vec![calls_reply("", &args), fakes::Scripted::text("Done.")],
        None,
        tools,
    );
    session.inbox.send(delivery("hi")).unwrap();
    let _release = Release {
        gates: gates.clone(),
        cancel: Arc::clone(&session.cancel),
    };
    let watcher = session.log.watch();
    let ended = spawn_turn(&mut session);

    // Wait 1: every call started, and every first delta. A first delta
    // after idle is due at once, so each arrives without any release.
    let firsts: Vec<String> = (0..n).map(|index| format!("a{index}")).collect();
    let (mut watcher, mut lines) = read_lines_until(
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

    // Open the gates in the generated return order. Each flushed second
    // delta proves its tool returned, so the next gate opens only after
    // the previous tool did: the return order is forced, not waited for.
    for k in &perm {
        gates[*k].open();
        let second = format!("b{k}");
        let (w, l) = read_lines_until(
            watcher,
            lines,
            "the tool's flushed second delta",
            move |lines| {
                lines.iter().any(|line| {
                    line.kind == "tool_call_delta"
                        && line.payload.get("text").and_then(Value::as_str) == Some(&second)
                })
            },
        );
        watcher = w;
        lines = l;
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

    // A failed call's questions are never acted on: the turn takes its
    // second step and completes.
    prop_assert_eq!(outcome, Some(TurnOutcome::Completed));
    let ends: Vec<&Envelope> = all
        .iter()
        .filter(|line| line.kind == "turn_completed")
        .collect();
    prop_assert_eq!(ends.len(), 1);
    prop_assert_eq!(ends[0].payload["outcome"].as_str().unwrap(), "completed");
    prop_assert!(ends[0].payload.get("questions").is_none());
    prop_assert_eq!(session.requests().len(), 2);
    for (index, gate) in gates.iter().enumerate() {
        gate.check(&format!("gate {index}"));
    }

    // The return order was forced: the second deltas, in log order, are
    // the permutation.
    prop_assert_eq!(
        all.iter()
            .filter(|line| line.kind == "tool_call_delta")
            .filter_map(|line| line.payload.get("text").and_then(Value::as_str))
            .filter(|text| text.starts_with('b'))
            .map(str::to_owned)
            .collect::<Vec<_>>(),
        perm.iter().map(|k| format!("b{k}")).collect::<Vec<_>>(),
    );

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

    for (index, kind) in calls.iter().enumerate() {
        let id = requested[index].action_id.clone();
        let mine: Vec<&&Envelope> = done.iter().filter(|line| line.action_id == id).collect();
        prop_assert_eq!(mine.len(), 1);
        let mine = mine[0];
        prop_assert_eq!(
            mine.payload["status"].as_str().unwrap(),
            if kind.is_error() {
                "failed"
            } else {
                "completed"
            }
        );
        prop_assert_eq!(text_all(mine), kind.text(index));
        let deltas: Vec<&Envelope> = all
            .iter()
            .filter(|line| line.kind == "tool_call_delta" && line.action_id == id)
            .collect();
        prop_assert_eq!(deltas.len(), 2);
        prop_assert_eq!(
            deltas[0].payload["text"].as_str().unwrap(),
            format!("a{index}")
        );
        prop_assert_eq!(
            deltas[1].payload["text"].as_str().unwrap(),
            format!("b{index}")
        );
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
    }

    // The second request carries every result in request order, each
    // with its own text, errored only for `Fails`.
    let requests = session.requests();
    let results: Vec<&Input> = requests[1]
        .conversation
        .iter()
        .filter(|input| matches!(input, Input::ToolResult { .. }))
        .collect();
    prop_assert_eq!(results.len(), n);
    for (pos, result) in results.iter().enumerate() {
        let Input::ToolResult {
            action_id,
            text,
            is_error,
            ..
        } = result
        else {
            return Err(TestCaseError::fail("a tool result"));
        };
        prop_assert_eq!(&Some(action_id.clone()), &requested_ids[pos]);
        prop_assert_eq!(text, &calls[pos].text(pos));
        prop_assert_eq!(is_error, &calls[pos].is_error());
    }
    Ok(())
}

/// One generated input: how it reaches the loop.
#[derive(Clone, Copy, Debug)]
enum Sent {
    Prompt,
    Steer,
}

/// One generated turn: what waits in the inbox when it starts, what
/// arrives on its first model call, and whether that call cancels it.
#[derive(Clone, Debug)]
struct TurnPlan {
    batch: Vec<Sent>,
    during: Vec<Sent>,
    cancelled: bool,
}

/// The provider that feeds a turn its mid-turn inputs: on the call at
/// `plan`'s index it sends that turn's deliveries in order, cancels when
/// flagged, then answers from its script. All of this runs on the loop's
/// own thread, before the reply streams, so the order is fixed. The
/// `inbox` and `cancel` slots are filled from the session after
/// construction, before any turn.
struct Planned {
    inner: Arc<ScriptedProvider>,
    plan: Mutex<BTreeMap<usize, (Vec<Delivery>, bool)>>,
    inbox: Mutex<Option<mpsc::Sender<Delivery>>>,
    cancel: Mutex<Option<Arc<TurnCancel>>>,
    calls: AtomicUsize,
}

impl Planned {
    fn new(inner: Arc<ScriptedProvider>, plan: BTreeMap<usize, (Vec<Delivery>, bool)>) -> Self {
        Self {
            inner,
            plan: Mutex::new(plan),
            inbox: Mutex::new(None),
            cancel: Mutex::new(None),
            calls: AtomicUsize::new(0),
        }
    }
}

impl Provider for Planned {
    fn wire_tools(&self, tools: &[ToolDefinition]) -> Vec<Map<String, Value>> {
        fakes::wire_tools(tools)
    }

    fn call(&self, request: &ModelRequest) -> Box<dyn ModelCall> {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((deliveries, cancelled)) = self.plan.lock().unwrap().remove(&index) {
            let inbox = self
                .inbox
                .lock()
                .unwrap()
                .clone()
                .expect("the inbox is filled before any turn");
            for delivery in deliveries {
                inbox.send(delivery).unwrap();
            }
            if cancelled {
                self.cancel
                    .lock()
                    .unwrap()
                    .clone()
                    .expect("the cancel is filled before any turn")
                    .cancel();
            }
        }
        self.inner.call(request)
    }
}

/// One numbered input and how it was sent.
struct Intake {
    k: usize,
    text: String,
    sent: Sent,
    turn: usize,
    batch: bool,
}

fn sent_strategy() -> impl Strategy<Value = Sent> {
    prop_oneof![Just(Sent::Prompt), Just(Sent::Steer)]
}

fn turn_plan_strategy() -> impl Strategy<Value = TurnPlan> {
    (
        prop::collection::vec(sent_strategy(), 1..=3),
        prop::collection::vec(sent_strategy(), 0..=2),
        any::<bool>(),
    )
        .prop_map(|(batch, during, cancelled)| TurnPlan {
            batch,
            during,
            cancelled,
        })
}

/// 1..=4 turns, the last one never cancelled so nothing is left in the
/// inbox at the end.
fn intake_plan() -> impl Strategy<Value = Vec<TurnPlan>> {
    prop::collection::vec(turn_plan_strategy(), 1..=4).prop_map(|mut turns| {
        if let Some(last) = turns.last_mut() {
            last.cancelled = false;
        }
        turns
    })
}

/// One numbered input as its delivery: the ack reports its answer on `tx`,
/// and the variant follows `sent`.
fn intake_delivery(
    sent: Sent,
    text: &str,
    id: usize,
    tx: mpsc::Sender<(usize, Option<ErrorCode>)>,
) -> Delivery {
    let ack = Ack(Box::new(move |answer| {
        let _sent = tx.send((id, answer.err().map(|rejection| rejection.code)));
    }));
    match sent {
        Sent::Prompt => Delivery::Prompt(message(text), ack),
        Sent::Steer => Delivery::Steer(message(text), ack),
    }
}

#[track_caller]
fn run_intake_case(turns: Vec<TurnPlan>) -> Result<(), TestCaseError> {
    let (ack_tx, ack_rx) = mpsc::channel::<(usize, Option<ErrorCode>)>();
    let mut inputs: Vec<Intake> = Vec::new();
    let mut batches: Vec<Vec<Delivery>> = Vec::new();
    let mut plan: BTreeMap<usize, (Vec<Delivery>, bool)> = BTreeMap::new();
    let mut k = 0usize;
    let mut call = 0usize;
    for (t, turn) in turns.iter().enumerate() {
        let mut batch = Vec::new();
        for sent in &turn.batch {
            let text = format!("input-{k:02}");
            batch.push(intake_delivery(*sent, &text, k, ack_tx.clone()));
            inputs.push(Intake {
                k,
                text,
                sent: *sent,
                turn: t,
                batch: true,
            });
            k += 1;
        }
        batches.push(batch);
        let mut during = Vec::new();
        for sent in &turn.during {
            let text = format!("input-{k:02}");
            during.push(intake_delivery(*sent, &text, k, ack_tx.clone()));
            inputs.push(Intake {
                k,
                text,
                sent: *sent,
                turn: t,
                batch: false,
            });
            k += 1;
        }
        plan.insert(call, (during, turn.cancelled));
        call += 1;
        // A steer delivered mid-turn continues the same turn with another
        // step: one call plus one more when `during` holds a steer.
        if !turn.cancelled && turn.during.iter().any(|sent| matches!(sent, Sent::Steer)) {
            call += 1;
        }
    }
    drop(ack_tx);
    let expected_calls = call;

    let script: Vec<fakes::Scripted> = (0..expected_calls)
        .map(|c| fakes::Scripted::text(&format!("reply {c}")))
        .collect();
    let slot: Arc<Mutex<Option<Arc<Planned>>>> = Arc::new(Mutex::new(None));
    let mut session = Session::wrapped(script, CacheLifetime::OneHour, {
        let slot = Arc::clone(&slot);
        move |inner, _| {
            let planned = Arc::new(Planned::new(inner, plan));
            *slot.lock().unwrap() = Some(Arc::clone(&planned));
            planned as Arc<dyn Provider>
        }
    });
    let planned = slot
        .lock()
        .unwrap()
        .clone()
        .expect("the provider is built with the session");
    *planned.inbox.lock().unwrap() = Some(session.inbox.clone());
    *planned.cancel.lock().unwrap() = Some(Arc::clone(&session.cancel));

    // The watcher is subscribed before the first turn, so it sees every
    // turn. Each turn gets one `MUST_SUCCEED_WITHIN` for its outcome and its
    // lines through its `turn_completed`.
    let mut watcher = session.log.watch();
    let mut all: Vec<Envelope> = Vec::new();
    for (t, turn) in turns.iter().enumerate() {
        for delivery in std::mem::take(&mut batches[t]) {
            session.inbox.send(delivery).unwrap();
        }
        let ended = spawn_turn(&mut session);
        let (w, tail, looped, outcome) = finish_turn(watcher, ended);
        watcher = w;
        session.looped = Some(looped);
        prop_assert_eq!(
            outcome,
            Some(if turn.cancelled {
                TurnOutcome::Interrupted
            } else {
                TurnOutcome::Completed
            })
        );
        all.extend(tail);
    }

    // Every ack runs on the loop thread before the `turn_completed` the
    // test already read, so there is nothing to wait for: exactly one
    // answer per delivery, then none.
    let mut acks: BTreeMap<usize, Option<ErrorCode>> = BTreeMap::new();
    while let Ok((id, code)) = ack_rx.try_recv() {
        prop_assert!(acks.insert(id, code).is_none(), "ack {id} answered twice");
    }
    prop_assert_eq!(acks.len(), inputs.len());

    prop_assert_eq!(session.requests().len(), expected_calls);
    // The turn boundaries alternate, one pair per turn: never two starts
    // in a row.
    prop_assert_eq!(
        all.iter()
            .filter(|line| line.kind == "turn_started" || line.kind == "turn_completed")
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>(),
        turns
            .iter()
            .flat_map(|_| ["turn_started", "turn_completed"])
            .collect::<Vec<_>>(),
    );

    // Everything queued before a turn and every steer is accepted. A
    // prompt arriving mid-turn is rejected `busy`. A prompt delivered to
    // a cancelled turn may be rejected, or taken by the next turn: the
    // property accepts either and checks consistency below, not a fixed
    // answer.
    for input in &inputs {
        let code = acks.get(&input.k).unwrap();
        if input.batch || matches!(input.sent, Sent::Steer) {
            prop_assert!(code.is_none(), "{} is accepted", input.text);
        } else if !turns[input.turn].cancelled {
            prop_assert_eq!(code, &Some(ErrorCode::Busy));
        }
    }

    // The accepted texts, in log order across every `turn_started` input
    // and `steering_applied` content, equal the accepted deliveries in
    // send order: the complete ordered list, so a duplicated, missing or
    // reordered input fails.
    let mut seen: Vec<String> = Vec::new();
    for line in &all {
        if line.kind == "turn_started" {
            for item in line.payload["input"].as_array().unwrap() {
                seen.push(item["content"][0]["text"].as_str().unwrap().to_owned());
            }
        } else if line.kind == "steering_applied" {
            seen.push(
                line.payload["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
    }
    let accepted: Vec<String> = inputs
        .iter()
        .filter(|input| acks.get(&input.k).unwrap().is_none())
        .map(|input| input.text.clone())
        .collect();
    prop_assert_eq!(seen.clone(), accepted);
    for input in inputs
        .iter()
        .filter(|input| acks.get(&input.k).unwrap().is_some())
    {
        prop_assert!(
            !seen.contains(&input.text),
            "a rejected input appears nowhere"
        );
    }

    // The last request's conversation carries each accepted input as
    // exactly one user text, and no rejected one.
    let requests = session.requests();
    let last = requests.last().unwrap();
    let users: Vec<&str> = last
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.as_str()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();
    for input in inputs
        .iter()
        .filter(|input| acks.get(&input.k).unwrap().is_none())
    {
        prop_assert_eq!(
            users.iter().filter(|text| ***text == input.text).count(),
            1,
            "{} reaches the model once",
            input.text
        );
    }
    for input in inputs
        .iter()
        .filter(|input| acks.get(&input.k).unwrap().is_some())
    {
        prop_assert_eq!(
            users.iter().filter(|text| ***text == input.text).count(),
            0,
            "{} never reaches the model",
            input.text
        );
    }
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

    /// Results return in request order whatever order the tools return in
    /// (`docs/testing.md`, "Invariants"; `docs/loop.md`, "One step" 6
    /// and 7). The forced return order comes from the flushed delta, not
    /// from a timer: each tool's second delta is held by pacing until its
    /// tool returns, and the test opens the next gate only after seeing
    /// the previous flush. For the control half, a failed call's questions
    /// are never acted on (`docs/loop.md`, "Ending a turn").
    /// Faults it catches:
    /// - F4 (controlled half): the `conversation.rs` `controlled` filter
    ///   flipped or removed (a failed call's questions would end the turn
    ///   after one request).
    /// - F6: `complete_next` writing the first finished call rather than
    ///   the first unwritten one in request order, or the conversation
    ///   ordering results by completion.
    #[test]
    fn results_return_in_request_order_whatever_order_the_tools_return_in(
        (calls, perm) in order_plan(),
    ) {
        run_order_case(calls, perm)?;
    }

    /// No accepted input is lost or repeated (`docs/testing.md`,
    /// "Invariants"; `docs/loop.md`, "Starting a turn", "Ending a
    /// turn" and "Interrupt"; `docs/invocation.md`, "A prompt arriving
    /// mid-turn is rejected `busy`"). Batched inputs start one turn, a
    /// steer sent mid-turn continues it, and inputs a cancelled turn never
    /// read appear in the next one.
    /// Faults it catches:
    /// - F7: `inbox.rs` `admit_running` accepting a `Prompt` mid-turn, or
    ///   `admit_idle` dropping or duplicating a batched delivery (acks,
    ///   exactly-once and arrival order).
    /// - F8: the end-of-turn drain skipped (a steer sent during the last
    ///   reply is lost), or an unread delivery dropped when a turn is
    ///   interrupted (a steer is accepted and appears once; inputs left by
    ///   a cancelled turn appear in the next one; the request count).
    #[test]
    fn no_accepted_input_is_lost_or_repeated(turns in intake_plan()) {
        run_intake_case(turns)?;
    }
}
