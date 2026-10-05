//! Tool calls in a step, through the loop's public API, with test tools
//! registered through the tool seam (`docs/loop.md`: "One step" 5 to 7,
//! "Tool calls that do not run", "A reply cut off by the output limit";
//! `docs/tools.md`: "Before a call runs", "What a result carries", "Bounded
//! results"; `docs/permissions.md`, "Fast paths").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::events::{
    Control, Event, FileChange, Progress, TextDelta, ToolCallStarted, TurnOutcome,
};
use contract::provider::{Finish, Input};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Process};
use contract::tool::{Bound, EffectsError, Tool};
use contract::{Envelope, ErrorCode};
use fakes::Scripted;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use support::{Gate, Script, Session, Tap, TestTool, calls_reply, delivery, kinds};

/// A session whose first reply makes `calls` and whose second says "Done.",
/// with `tools` registered. Runs one turn and returns its lines.
fn turn(tools: Vec<Arc<dyn Tool>>, calls: &[(&str, Value)]) -> (Session, Vec<Envelope>) {
    let mut session = Session::with_tools(
        vec![calls_reply("", calls), Scripted::text("Done.")],
        None,
        tools,
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    (session, lines)
}

fn paris() -> Value {
    json!({"city": "Paris"})
}

/// The durable lines about tool calls, as `kind name-or-status`.
fn calls(lines: &[Envelope]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.kind.starts_with("tool_call_") && l.seq.is_some())
        .map(|l| l.kind.clone())
        .collect()
}

fn completed(lines: &[Envelope]) -> Vec<&Envelope> {
    lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .collect()
}

fn text(line: &Envelope) -> &str {
    line.payload["content"][0]["text"].as_str().unwrap()
}

#[test]
fn a_reads_call_runs_and_its_result_goes_to_the_model() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let (session, lines) = turn(vec![tool.clone()], &[("get_weather", paris())]);
    assert_eq!(
        calls(&lines),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed"
        ]
    );
    assert!(!kinds(&lines).iter().any(|k| k.starts_with("permission_")));
    let started = lines
        .iter()
        .find(|l| l.kind == "tool_call_started")
        .unwrap();
    assert_eq!(
        Value::Object(started.payload.clone()),
        json!({"effects": ["reads"], "reversible": true})
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "completed");
    assert_eq!(text(done), "Sunny.");
    assert_eq!(tool.ran(), [paris().as_object().unwrap().clone()]);

    let requests = session.requests();
    assert_eq!(requests[0].tools.len(), 1);
    assert_eq!(requests[0].tools[0].name, "get_weather");
    assert_eq!(
        requests[1].conversation.last(),
        Some(&Input::ToolResult {
            action_id: done.action_id.clone().unwrap(),
            text: "Sunny.".into(),
            is_error: false,
            images: Vec::new()
        })
    );
}

#[test]
fn a_result_carries_what_the_tool_returned() {
    let mut tool = TestTool::reads("get_weather", "Sunny.");
    tool.output.process = Some(Process {
        exit_code: Some(0),
        signal: None,
        timed_out: false,
    });
    tool.output.details = Some(json!({"diff": "+a"}));
    tool.output.changes = Some(vec![FileChange {
        path: "/w/a".into(),
        added: 1,
        removed: 0,
    }]);
    tool.output.control = Some(Control {
        handoff: "note".into(),
    });
    let (_, lines) = turn(vec![Arc::new(tool)], &[("get_weather", paris())]);
    let done = &completed(&lines)[0].payload;
    assert_eq!(done["process"], json!({"exit_code": 0, "timed_out": false}));
    assert_eq!(done["details"], json!({"diff": "+a"}));
    assert_eq!(
        done["changes"],
        json!([{"path": "/w/a", "added": 1, "removed": 0}])
    );
    assert_eq!(done["control"], json!({"handoff": "note"}));
}

#[test]
fn a_failed_result_completes_failed_with_its_code() {
    let tool = Arc::new(TestTool::failing("get_weather", ErrorCode::ToolError));
    let (session, lines) = turn(vec![tool], &[("get_weather", paris())]);
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "failed");
    assert_eq!(done.payload["error"]["code"], "tool_error");
    assert!(matches!(
        session.requests()[1].conversation.last(),
        Some(Input::ToolResult { is_error: true, .. })
    ));
}

#[test]
fn an_unknown_tool_is_told_which_names_exist() {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(TestTool::reads("get_weather", "Sunny.")),
        Arc::new(TestTool::reads("get_time", "Noon.")),
    ];
    let (_, lines) = turn(tools, &[("get_wether", paris())]);
    assert_eq!(
        calls(&lines),
        ["tool_call_requested", "tool_call_completed"]
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["error"]["code"], "unknown_tool");
    assert_eq!(
        text(done),
        "No tool is named `get_wether`. The tools are `get_time`, `get_weather`."
    );
}

#[test]
fn bad_arguments_fail_invalid_arguments_and_never_start() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let (_, lines) = turn(
        vec![tool.clone()],
        &[
            ("get_weather", json!({"days": true, "country": "FR"})),
            ("get_weather", Value::String("{\"city\": ".into())),
        ],
    );
    assert!(!kinds(&lines).contains(&"tool_call_started"));
    assert!(tool.ran().is_empty());
    let done = completed(&lines);
    for line in &done {
        assert_eq!(line.payload["status"], "failed");
        assert_eq!(line.payload["error"]["code"], "invalid_arguments");
    }
    assert_eq!(
        text(done[0]),
        "The arguments do not match the tool's schema:\n\
         `/city`: missing\n\
         `/country`: not allowed\n\
         `/days`: expected integer, got a boolean"
    );
    assert!(text(done[1]).contains("not a JSON object"));
}

#[test]
fn repaired_arguments_are_recorded_run_and_sent_back_as_written() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let written = json!({"city": "Paris", "days": "3"});
    let (session, lines) = turn(vec![tool.clone()], &[("get_weather", written.clone())]);
    let requested = lines
        .iter()
        .find(|l| l.kind == "tool_call_requested")
        .unwrap();
    assert_eq!(requested.payload["arguments"], written);
    assert_eq!(
        requested.payload["repaired"],
        json!({"city": "Paris", "days": 3})
    );
    assert_eq!(
        requested.payload["repairs"],
        json!([{"path": "/days", "fix": "string_to_number"}])
    );
    assert_eq!(tool.ran()[0]["days"], 3);
    let requests = session.requests();
    let Some(Input::ToolCall { call, .. }) = requests[1]
        .conversation
        .iter()
        .find(|i| matches!(i, Input::ToolCall { .. }))
    else {
        panic!("the call is sent back");
    };
    assert_eq!(call.arguments, written);
}

#[test]
fn arguments_that_need_no_repair_record_none() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let (_, lines) = turn(vec![tool], &[("get_weather", paris())]);
    let requested = lines
        .iter()
        .find(|l| l.kind == "tool_call_requested")
        .unwrap();
    assert_eq!(requested.payload.get("repaired"), None);
}

#[test]
fn an_effects_function_that_errors_fails_tool_error_and_never_runs() {
    let mut tool = TestTool::reads("get_weather", "Sunny.");
    tool.effects = Err(EffectsError::Tool("The effects function broke.".into()));
    let tool = Arc::new(tool);
    let (_, lines) = turn(vec![tool.clone()], &[("get_weather", paris())]);
    assert_eq!(
        calls(&lines),
        ["tool_call_requested", "tool_call_completed"]
    );
    let done = completed(&lines)[0];
    assert_eq!(done.payload["error"]["code"], "tool_error");
    assert_eq!(text(done), "The effects function broke.");
    assert!(tool.ran().is_empty());
}

#[test]
fn every_call_is_decided_before_any_runs_and_results_return_in_request_order() {
    let trace = Arc::default();
    let mut slow = TestTool::reads("slow", "first");
    slow.after = Some("fast");
    slow.trace = Arc::clone(&trace);
    let mut fast = TestTool::reads("fast", "third");
    fast.trace = Arc::clone(&trace);
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(slow), Arc::new(fast)];
    let (session, lines) = turn(
        tools,
        &[("slow", paris()), ("nope", paris()), ("fast", paris())],
    );
    let trace = trace.lock().unwrap().clone();
    assert_eq!(&trace[..2], ["effects slow", "effects fast"]);
    // The fast call finished first, and its result still comes last.
    assert!(
        trace.iter().position(|t| t == "done fast") < trace.iter().position(|t| t == "done slow")
    );
    assert_eq!(
        calls(&lines),
        [
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
        ]
    );
    let requested: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_requested")
        .map(|l| l.action_id.clone())
        .collect();
    let done: Vec<_> = completed(&lines)
        .iter()
        .map(|l| l.action_id.clone())
        .collect();
    assert_eq!(done, requested);
    let requests = session.requests();
    let results: Vec<&str> = requests[1]
        .conversation
        .iter()
        .filter_map(|i| match i {
            Input::ToolResult { text, .. } => Some(text.as_str()),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. } => None,
        })
        .collect();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0], "first");
    assert!(results[1].starts_with("No tool is named `nope`"));
    assert_eq!(results[2], "third");
}

#[test]
fn approved_calls_run_concurrently_one_thread_each() {
    // Each call waits for the other inside its run, so calls run one after
    // the other never finish and the turn misses its deadline.
    let barrier = Arc::new(Barrier::new(2));
    let tools: Vec<Arc<dyn Tool>> = ["a", "b"]
        .into_iter()
        .map(|name| {
            let mut tool = TestTool::reads(name, name);
            tool.barrier = Some(Arc::clone(&barrier));
            Arc::new(tool) as Arc<dyn Tool>
        })
        .collect();
    let (_, lines) = turn(tools, &[("a", paris()), ("b", paris())]);
    assert_eq!(completed(&lines).len(), 2);
}

#[test]
fn a_write_inside_the_workspace_runs_and_one_under_git_or_outside_does_not() {
    let session_root = support::TempDir::new();
    let write = |name: &'static str, path: &str| -> Arc<dyn Tool> {
        Arc::new(TestTool::declaring(
            name,
            "Wrote it.",
            vec![Effect::Writes],
            Some(vec![path.into()]),
        ))
    };
    let tools = vec![
        write("inside", "src/main.rs"),
        write("git", ".git/config"),
        write("fiber", ".fiber/config.json"),
        write(
            "outside",
            &session_root.0.join("elsewhere").display().to_string(),
        ),
        Arc::new(TestTool::declaring(
            "shell",
            "Ran it.",
            vec![Effect::Executes],
            None,
        )),
        Arc::new(TestTool::declaring("pure", "Nothing.", Vec::new(), None)),
    ];
    let names = ["inside", "git", "fiber", "outside", "shell", "pure"];
    let calls: Vec<(&str, Value)> = names.iter().map(|n| (*n, paris())).collect();
    // No reviewer is configured, so the reviewed calls escalate `no_model`,
    // and no person can answer them: each ends a reviewer deny.
    let mut session = Session::with_tools(
        vec![calls_reply("", &calls), Scripted::text("Done.")],
        None,
        tools,
    )
    .answerable(false);
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let statuses: Vec<&str> = completed(&lines)
        .iter()
        .map(|l| l.payload["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        [
            "completed",
            "denied",
            "denied",
            "denied",
            "denied",
            "completed"
        ]
    );
    let denied = completed(&lines)[1];
    assert_eq!(denied.payload["reason"], "reviewer");
    assert_eq!(
        kinds(&lines)
            .iter()
            .filter(|k| **k == "tool_call_started")
            .count(),
        2
    );
}

#[test]
fn a_cut_off_reply_runs_none_of_its_calls_and_the_turn_continues() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut cut = calls_reply(
        "Let me",
        &[("get_weather", paris()), ("get_weather", paris())],
    );
    if let Ok(reply) = &mut cut.end {
        reply.finish = Finish::OutputLimit;
    }
    let mut session =
        Session::with_tools(vec![cut, Scripted::text("Done.")], None, vec![tool.clone()]);
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert!(tool.ran().is_empty());
    let done = completed(&lines);
    assert_eq!(done.len(), 2);
    for line in done {
        assert_eq!(line.payload["status"], "failed");
        assert_eq!(line.payload["error"]["code"], "output_truncated");
        assert!(text(line).contains("may be incomplete"));
    }
    let part = lines.iter().find(|l| l.kind == "text_completed").unwrap();
    assert_eq!(part.payload["text"], "Let me");
}

#[test]
fn a_cut_off_reply_with_no_call_completes_the_turn() {
    let mut cut = Scripted::text("Half a");
    if let Ok(reply) = &mut cut.end {
        reply.finish = Finish::OutputLimit;
    }
    let mut session = Session::new(vec![cut], None);
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(session.requests().len(), 1);
}

#[test]
fn a_cut_off_after_a_cut_off_with_calls_fails_the_turn_even_with_no_call() {
    let mut first = calls_reply("", &[("get_weather", paris())]);
    let mut second = Scripted::text("Half a");
    for cut in [&mut first, &mut second] {
        if let Ok(reply) = &mut cut.end {
            reply.finish = Finish::OutputLimit;
        }
    }
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::with_tools(vec![first, second], None, vec![tool]);
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
}

#[test]
fn a_second_cut_off_in_a_row_fails_the_turn() {
    let cut = || {
        let mut cut = calls_reply("Let me", &[("get_weather", paris())]);
        if let Ok(reply) = &mut cut.end {
            reply.finish = Finish::OutputLimit;
        }
        cut
    };
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::with_tools(
        vec![cut(), cut(), cut(), Scripted::text("Done.")],
        None,
        vec![tool],
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(completed(&lines).len(), 2);
    let end = lines.last().unwrap();
    assert_eq!(end.payload["error"]["code"], "output_truncated");
    assert_eq!(session.requests().len(), 2);

    // The next turn starts with no cut-off behind it.
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
}

#[test]
fn a_cut_off_between_two_whole_replies_does_not_count_twice() {
    let cut = || {
        let mut cut = calls_reply("", &[("get_weather", paris())]);
        if let Ok(reply) = &mut cut.end {
            reply.finish = Finish::OutputLimit;
        }
        cut
    };
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::with_tools(
        vec![
            cut(),
            calls_reply("", &[("get_weather", paris())]),
            cut(),
            Scripted::text("Done."),
        ],
        None,
        vec![tool],
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
}

/// A tool returning `size` bytes of text, cut to `bound`.
fn long(size: usize, bound: Bound) -> (Arc<TestTool>, String) {
    let full: String = (0..size)
        .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap()))
        .collect();
    let mut tool = TestTool::reads("cat", &full);
    tool.output.content.push(ContentPart::Image {
        path: "artifacts/i.png".into(),
        mime_type: "image/png".into(),
        width: 1,
        height: 1,
    });
    tool.bound = bound;
    (Arc::new(tool), full)
}

#[test]
fn a_result_over_its_bound_keeps_the_start_and_moves_the_rest_to_an_artifact() {
    let (tool, full) = long(20 * 1024, Bound::DEFAULT);
    let (session, lines) = turn(vec![tool], &[("cat", paris())]);
    let done = completed(&lines)[0];
    let id = done.action_id.clone().unwrap().0;
    let artifact = format!("artifacts/{id}.txt");
    assert_eq!(done.payload["artifact"], artifact.as_str());
    let path = session.dir.join(&artifact);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), full);
    let kept = text(done);
    let (head, notice) = kept.split_once('\n').unwrap();
    assert_eq!(head, &full[..16 * 1024]);
    assert_eq!(
        notice,
        format!(
            "[4096 bytes cut. The full output is in {}; read it with `read`.]",
            path.display()
        )
    );
    // Image parts are never cut.
    assert_eq!(done.payload["content"][1]["type"], "image");
}

#[test]
fn a_tool_can_keep_both_ends_with_the_notice_between() {
    let (tool, full) = long(100, Bound { start: 10, end: 20 });
    let (_, lines) = turn(vec![tool], &[("cat", paris())]);
    let kept: Vec<&str> = text(completed(&lines)[0]).split('\n').collect();
    assert_eq!(kept.len(), 3);
    assert_eq!(kept[0], &full[..10]);
    assert!(kept[1].starts_with("[70 bytes cut."), "{}", kept[1]);
    assert_eq!(kept[2], &full[80..]);
}

#[test]
fn a_result_at_its_bound_is_not_cut() {
    let (tool, full) = long(30, Bound { start: 10, end: 20 });
    let (_, lines) = turn(vec![tool], &[("cat", paris())]);
    let done = completed(&lines)[0];
    assert_eq!(text(done), full);
    assert_eq!(done.payload.get("artifact"), None);
}

#[test]
fn the_loop_passes_a_cancel_that_is_not_cancelled() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let _ = turn(
        vec![Arc::clone(&tool) as Arc<dyn Tool>],
        &[("get_weather", paris())],
    );
    assert_eq!(tool.cancelled.lock().unwrap().as_slice(), &[false]);
}

#[test]
fn two_fsyncs_per_tool_call() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::with_tools(
        vec![
            calls_reply(
                "",
                &[
                    ("get_weather", paris()),
                    ("get_weather", paris()),
                    ("get_weather", paris()),
                ],
            ),
            Scripted::text("Done."),
        ],
        None,
        vec![tool],
    );
    let before = session.log.fsyncs();
    session.inbox.send(delivery("go")).unwrap();
    session.turn();
    // Two model requests at two each, and three calls at two each.
    assert_eq!(session.log.fsyncs() - before, 2 * 2 + 3 * 2);
}

/// A script step emitting `event` through the call's emitter.
fn emit(event: Event) -> Script {
    Script::Emit(Box::new(event))
}

/// A `tool_call_delta` carrying `text` and nothing else, as a tool emits it.
fn delta(text: &str) -> Event {
    Event::ToolCallDelta(Progress {
        text: Some(text.into()),
        details: None,
    })
}

/// A session whose first reply makes `calls` and whose second says "Done.",
/// with its prompt already sent and a live tap on its log: the test runs the
/// turn on a scoped thread while reading `tap`, advancing `clock` and
/// arriving at the tools' gates.
fn streaming_turn(
    tools: Vec<Arc<dyn Tool>>,
    calls: &[(&str, Value)],
) -> (Session, Tap, Arc<FakeClock>) {
    let session = Session::with_tools(
        vec![calls_reply("", calls), Scripted::text("Done.")],
        None,
        tools,
    );
    session.inbox.send(delivery("go")).unwrap();
    let clock = Arc::clone(&session.clock);
    let tap = Tap::new(&session.log);
    (session, tap, clock)
}

/// Every `tool_call_*` lifecycle kind, requested, started, delta and
/// completed, in log order: not the model's `tool_call_arguments_delta`
/// fragments, which stream while the reply arrives.
fn tool_kinds(lines: &[Envelope]) -> Vec<&str> {
    lines
        .iter()
        .filter(|l| {
            matches!(
                l.kind.as_str(),
                "tool_call_requested"
                    | "tool_call_started"
                    | "tool_call_delta"
                    | "tool_call_completed"
            )
        })
        .map(|l| l.kind.as_str())
        .collect()
}

/// The `text` of every `tool_call_delta` in `lines`, in order.
fn delta_texts(lines: &[Envelope]) -> Vec<&str> {
    lines
        .iter()
        .filter(|l| l.kind == "tool_call_delta")
        .map(|l| l.payload["text"].as_str().unwrap())
        .collect()
}

#[test]
fn a_running_call_streams_one_delta_between_started_and_completed() {
    let mut tool = TestTool::reads("get_weather", "Sunny.");
    tool.script = vec![emit(delta("half "))];
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
    let (mut session, tap, _clock) = streaming_turn(tools, &[("get_weather", paris())]);
    thread::scope(|s| {
        s.spawn(|| assert_eq!(session.turn(), Some(TurnOutcome::Completed)));
        let started = tap.wait_for("tool_call_started");
        let first = tap.wait_for("tool_call_delta");
        assert_eq!(first.payload["text"], "half ");
        assert_eq!(first.action_id, started.action_id);
        assert_eq!(first.turn_id, started.turn_id);
        assert!(first.turn_id.is_some());
        assert_eq!(first.seq, None);
        tap.wait_for("tool_call_completed");
    });
    let lines = session.lines();
    assert_eq!(
        tool_kinds(&lines),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_completed"
        ]
    );
}

#[test]
fn a_second_delta_within_the_interval_waits_for_the_clock() {
    let gate1 = Arc::new(Gate::default());
    let gate2 = Arc::new(Gate::default());
    let mut tool = TestTool::reads("get_weather", "Sunny.");
    tool.script = vec![
        emit(delta("one")),
        Script::Wait(Arc::clone(&gate1)),
        emit(delta("two")),
        Script::Wait(Arc::clone(&gate2)),
    ];
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
    let (mut session, tap, clock) = streaming_turn(tools, &[("get_weather", paris())]);
    thread::scope(|s| {
        s.spawn(|| assert_eq!(session.turn(), Some(TurnOutcome::Completed)));
        tap.wait_for("tool_call_started");
        let first = tap.wait_for_delta("one");
        // The tool is past its first emit and blocked at its first gate, so
        // "two" is not yet emitted and the loop holds nothing.
        gate1.open();
        let due = clock.now() + Duration::from_millis(100);
        assert!(
            clock.await_parked(due, Duration::from_secs(10)),
            "the loop did not park until the interval ends"
        );
        assert!(
            tap.pending().iter().all(|l| l.kind != "tool_call_delta"),
            "a delta leaked before the clock moved"
        );
        clock.advance(Duration::from_millis(100));
        let second = tap.wait_for_delta("two");
        assert_eq!(second.action_id, first.action_id);
        gate2.open();
        tap.wait_for("tool_call_completed");
    });
    gate1.check("first emit's gate");
    gate2.check("second emit's gate");
    let lines = session.lines();
    assert_eq!(
        tool_kinds(&lines),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_delta",
            "tool_call_completed"
        ]
    );
    assert_eq!(delta_texts(&lines), ["one", "two"]);
}

#[test]
fn a_delta_emitted_just_before_the_call_returns_flushes_without_the_clock_moving() {
    let gate = Arc::new(Gate::default());
    let mut tool = TestTool::reads("get_weather", "Sunny.");
    tool.script = vec![
        emit(delta("a")),
        Script::Wait(Arc::clone(&gate)),
        emit(delta("b")),
    ];
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
    let (mut session, tap, _clock) = streaming_turn(tools, &[("get_weather", paris())]);
    thread::scope(|s| {
        s.spawn(|| assert_eq!(session.turn(), Some(TurnOutcome::Completed)));
        tap.wait_for("tool_call_started");
        tap.wait_for_delta("a");
        // "b" is not yet emitted: the tool is blocked at its gate, so no
        // second delta can have been written.
        assert!(
            tap.pending().iter().all(|l| l.kind != "tool_call_delta"),
            "a delta leaked before the gate opened"
        );
        gate.open();
        tap.wait_for("tool_call_completed");
    });
    gate.check("first delta's gate");
    // The clock never moved, so "b" reached the log only as the final flush,
    // written just before the completion.
    let lines = session.lines();
    assert_eq!(delta_texts(&lines), ["a", "b"]);
    let at: Vec<usize> = lines
        .iter()
        .position(|l| l.kind == "tool_call_delta" && l.payload["text"] == "b")
        .into_iter()
        .chain(lines.iter().position(|l| l.kind == "tool_call_completed"))
        .collect();
    assert_eq!(at.len(), 2);
    assert_eq!(at[1], at[0] + 1);
}

#[test]
fn back_to_back_deltas_collapse_to_one_with_both_texts_in_order() {
    let gate = Arc::new(Gate::default());
    let mut tool = TestTool::reads("get_weather", "Sunny.");
    tool.script = vec![
        emit(delta("a")),
        Script::Wait(Arc::clone(&gate)),
        emit(delta("b")),
        emit(delta("c")),
    ];
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
    let (mut session, tap, _clock) = streaming_turn(tools, &[("get_weather", paris())]);
    thread::scope(|s| {
        s.spawn(|| assert_eq!(session.turn(), Some(TurnOutcome::Completed)));
        tap.wait_for("tool_call_started");
        tap.wait_for_delta("a");
        gate.open();
        tap.wait_for("tool_call_completed");
    });
    gate.check("first delta's gate");
    // The clock never moved and "a" was already written, so the interval
    // held "b" and "c" for the final flush: one collapsed delta.
    let lines = session.lines();
    assert_eq!(delta_texts(&lines), ["a", "bc"]);
}

#[test]
fn events_that_are_not_deltas_write_nothing_extra() {
    let mut tool = TestTool::reads("get_weather", "Sunny.");
    tool.script = vec![
        emit(Event::ToolCallStarted(ToolCallStarted {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
            },
            arguments: None,
            changed_by: None,
        })),
        emit(Event::AssistantMessageDelta(TextDelta {
            text: "half ".into(),
        })),
    ];
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
    let (mut session, tap, _clock) = streaming_turn(tools, &[("get_weather", paris())]);
    thread::scope(|s| {
        s.spawn(|| assert_eq!(session.turn(), Some(TurnOutcome::Completed)));
        tap.wait_for("tool_call_completed");
    });
    let lines = session.lines();
    assert_eq!(
        tool_kinds(&lines),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed"
        ]
    );
}

#[test]
fn two_calls_complete_in_request_order_with_their_own_deltas() {
    let gate = Arc::new(Gate::default());
    let mut slow = TestTool::reads("slow", "first");
    slow.script = vec![emit(delta("S")), Script::Wait(Arc::clone(&gate))];
    let mut fast = TestTool::reads("fast", "second");
    fast.script = vec![emit(delta("F"))];
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(slow), Arc::new(fast)];
    let (mut session, tap, _clock) = streaming_turn(tools, &[("slow", paris()), ("fast", paris())]);
    thread::scope(|s| {
        s.spawn(|| assert_eq!(session.turn(), Some(TurnOutcome::Completed)));
        // Either delta may arrive first; each tool is past its emit once its
        // text is seen.
        let (ds, df) = (tap.wait_for_delta("S"), tap.wait_for_delta("F"));
        gate.open();
        let done_first = tap.wait_for("tool_call_completed");
        let done_last = tap.wait_for("tool_call_completed");
        assert_eq!(
            (ds.payload["text"].as_str(), df.payload["text"].as_str()),
            (Some("S"), Some("F"))
        );
        assert_ne!(ds.action_id, df.action_id);
        drop((done_first, done_last));
    });
    gate.check("slow call's gate");
    let lines = session.lines();
    let requested: Vec<(String, _)> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_requested")
        .map(|l| {
            (
                l.payload["name"].as_str().unwrap().to_owned(),
                l.action_id.clone(),
            )
        })
        .collect();
    let (slow_id, fast_id) = (
        requested
            .iter()
            .find(|(n, _)| n == "slow")
            .unwrap()
            .1
            .clone(),
        requested
            .iter()
            .find(|(n, _)| n == "fast")
            .unwrap()
            .1
            .clone(),
    );
    let done: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.action_id.clone())
        .collect();
    assert_eq!(done, [slow_id.clone(), fast_id.clone()]);
    for line in lines.iter().filter(|l| l.kind == "tool_call_delta") {
        let text = line.payload["text"].as_str().unwrap();
        let want = if text == "S" { &slow_id } else { &fast_id };
        assert_eq!(&line.action_id, want, "delta {text} rides the wrong call");
    }
}

#[test]
fn a_large_write_paces_the_next_delta_past_100_ms() {
    let gate1 = Arc::new(Gate::default());
    let gate2 = Arc::new(Gate::default());
    // 20 KiB of text: the written payload paces the next delta at
    // 20 KiB ÷ 100 KiB/s = 200 ms, past the 100 ms floor. The byte count is
    // the payload serialised as JSON, exactly what the loop measures.
    let big = "x".repeat(20 * 1024);
    let bytes = serde_json::to_vec(&Progress {
        text: Some(big.clone()),
        details: None,
    })
    .unwrap()
    .len();
    let interval = Duration::from_nanos(u64::try_from(bytes).unwrap() * 1_000_000_000 / 102_400)
        .max(Duration::from_millis(100));
    assert!(
        interval > Duration::from_millis(100),
        "the large delta does not pace past the floor"
    );
    let mut tool = TestTool::reads("big", "done.");
    tool.script = vec![
        emit(delta(&big)),
        Script::Wait(Arc::clone(&gate1)),
        emit(delta("small")),
        Script::Wait(Arc::clone(&gate2)),
    ];
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(tool)];
    let (mut session, tap, clock) = streaming_turn(tools, &[("big", paris())]);
    thread::scope(|s| {
        s.spawn(|| assert_eq!(session.turn(), Some(TurnOutcome::Completed)));
        tap.wait_for("tool_call_started");
        let first = tap.wait_for_delta(&big);
        // The tool is past its large emit and blocked at its first gate, so
        // "small" is not yet emitted and the loop holds nothing.
        gate1.open();
        let due = clock.now() + interval;
        assert!(
            clock.await_parked(due, Duration::from_secs(10)),
            "the loop did not park until the byte-paced interval ends"
        );
        assert!(
            tap.pending().iter().all(|l| l.kind != "tool_call_delta"),
            "a delta leaked before the clock moved"
        );
        clock.advance(interval);
        let second = tap.wait_for_delta("small");
        assert_eq!(second.action_id, first.action_id);
        gate2.open();
        tap.wait_for("tool_call_completed");
    });
    gate1.check("large emit's gate");
    gate2.check("second emit's gate");
    let lines = session.lines();
    assert_eq!(
        tool_kinds(&lines),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_delta",
            "tool_call_completed"
        ]
    );
    assert_eq!(delta_texts(&lines), [big.as_str(), "small"]);
}

#[test]
fn a_returned_call_flushes_while_an_earlier_call_still_runs() {
    let gate_a = Arc::new(Gate::default());
    let gate_b = Arc::new(Gate::default());
    let mut slow = TestTool::reads("a", "first");
    slow.script = vec![Script::Wait(Arc::clone(&gate_a))];
    let mut fast = TestTool::reads("b", "second");
    fast.script = vec![
        emit(delta("x")),
        Script::Wait(Arc::clone(&gate_b)),
        emit(delta("y")),
    ];
    let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(slow), Arc::new(fast)];
    let (mut session, tap, _clock) = streaming_turn(tools, &[("a", paris()), ("b", paris())]);
    thread::scope(|s| {
        s.spawn(|| assert_eq!(session.turn(), Some(TurnOutcome::Completed)));
        // "x" goes out at once; opening the gate lets the call emit "y" and
        // return while "a" is still blocked.
        tap.wait_for_delta("x");
        gate_b.open();
        // "y" is held by the interval, so only the flush on return writes
        // it: seeing it proves the call returned, still before "a" is
        // released and before any completion.
        tap.wait_for_delta("y");
        assert!(
            tap.pending()
                .iter()
                .all(|l| l.kind != "tool_call_completed"),
            "a completion preceded the earlier call's release"
        );
        gate_a.open();
        tap.wait_for("tool_call_completed");
        tap.wait_for("tool_call_completed");
    });
    gate_a.check("call a's release");
    gate_b.check("call b's second emit");
    let lines = session.lines();
    assert_eq!(delta_texts(&lines), ["x", "y"]);
    let completed: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.payload["status"].clone())
        .collect();
    assert_eq!(completed.len(), 2);
    let done: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_completed")
        .map(|l| l.action_id.clone())
        .collect();
    let requested: Vec<_> = lines
        .iter()
        .filter(|l| l.kind == "tool_call_requested")
        .map(|l| l.action_id.clone())
        .collect();
    assert_eq!(done, requested);
    let first_completed = lines
        .iter()
        .position(|l| l.kind == "tool_call_completed")
        .unwrap();
    let flushed = lines
        .iter()
        .position(|l| l.kind == "tool_call_delta" && l.payload["text"] == "y")
        .unwrap();
    assert!(
        flushed < first_completed,
        "the flush did not precede every completion"
    );
}
