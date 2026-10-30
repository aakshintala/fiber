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

use support::{
    Gate, Script, Session, Tap, TestTool, calls_reply, completed, delivery, kinds, paris,
    text_first,
};

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

/// The durable lines about tool calls, as `kind name-or-status`.
fn calls(lines: &[Envelope]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.kind.starts_with("tool_call_") && l.seq.is_some())
        .map(|l| l.kind.clone())
        .collect()
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
    assert_eq!(text_first(done), "Sunny.");
    assert_eq!(tool.ran(), [paris().as_object().unwrap().clone()]);

    let requests = session.requests();
    assert_eq!(requests[0].tools.len(), 1);
    assert_eq!(requests[0].tools[0].name, "get_weather");
    assert_eq!(
        requests[1].conversation.last(),
        Some(&Input::ToolResult {
            pdfs: Vec::new(),
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
        handoff: Some("note".into()),
        ..Default::default()
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
        text_first(done),
        "No tool is named `get_wether`. The tools are `get_time`, `get_weather`."
    );
}

#[test]
fn bad_arguments_fail_invalid_arguments_and_never_start() {
    // One step makes every call before any runs (`docs/loop.md`, "One
    // step"); a call the loop cannot run fails and the turn continues
    // (`docs/loop.md`, "Tool calls that do not run"), never writing
    // `tool_call_started` (`docs/tools.md`, "Before a call runs").
    const MALFORMED: &str = "{\"command\":\"ls\"]}";
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let malformed = Value::String(MALFORMED.into());
    let empty_array = json!([]);
    let pair_array = json!([1, 2]);
    let (session, lines) = turn(
        vec![tool.clone()],
        &[
            ("get_weather", json!({"days": true, "country": "FR"})),
            ("get_weather", malformed.clone()),
            ("get_weather", empty_array.clone()),
            ("get_weather", pair_array.clone()),
        ],
    );
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
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
            "tool_call_completed",
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
    assert!(!kinds(&lines).contains(&"tool_call_started"));
    assert!(tool.ran().is_empty());
    let done = completed(&lines);
    assert_eq!(done.len(), 4);
    for line in &done {
        assert_eq!(line.payload["status"], "failed");
        assert_eq!(line.payload["error"]["code"], "invalid_arguments");
    }
    assert_eq!(
        text_first(done[0]),
        "The arguments do not match the tool's schema:\n\
         `/city`: missing\n\
         `/country`: not allowed\n\
         `/days`: expected integer, got a boolean"
    );
    assert!(text_first(done[1]).contains("not a JSON object"));
    assert!(
        text_first(done[1]).contains(MALFORMED),
        "the failure quotes the raw text: {}",
        text_first(done[1])
    );
    assert!(
        text_first(done[2]).contains("[]"),
        "the failure quotes the array: {}",
        text_first(done[2])
    );
    assert!(
        text_first(done[3]).contains("[1,2]"),
        "the failure quotes the array: {}",
        text_first(done[3])
    );
    // The recorded call keeps the arguments as the model sent them.
    let requested: Vec<&Envelope> = lines
        .iter()
        .filter(|line| line.kind == "tool_call_requested")
        .collect();
    assert_eq!(requested.len(), 4);
    assert_eq!(requested[1].payload["arguments"], malformed);
    assert_eq!(requested[2].payload["arguments"], empty_array);
    assert_eq!(requested[3].payload["arguments"], pair_array);
    // The turn continues with a second request pairing each call with its
    // failed result, which the model is sent (`docs/loop.md`, "What the
    // model is sent").
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    let replayed: Vec<&Input> = requests[1]
        .conversation
        .iter()
        .filter(|input| matches!(input, Input::ToolCall { .. }))
        .collect();
    assert_eq!(replayed.len(), 4);
    for (call, arguments) in replayed.iter().zip([
        &json!({"days": true, "country": "FR"}),
        &malformed,
        &empty_array,
        &pair_array,
    ]) {
        let Input::ToolCall { call, .. } = call else {
            unreachable!()
        };
        assert_eq!(&call.arguments, arguments);
    }
    let results: Vec<(&str, bool)> = requests[1]
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult { text, is_error, .. } => Some((text.as_str(), *is_error)),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();
    assert_eq!(results.len(), 4);
    assert!(results.iter().all(|(_, is_error)| *is_error));
    assert!(results[1].0.contains(MALFORMED));
    assert!(results[2].0.contains("[]"));
    assert!(results[3].0.contains("[1,2]"));
}

/// A `shell` tool whose schema takes a required string `command` and an
/// optional integer `limit`.
fn shell_tool() -> TestTool {
    let mut tool = TestTool::reads("shell", "shell output");
    tool.schema = Some(json!({
        "type": "object",
        "properties": {
            "command": {"type": "string"},
            "limit": {"type": "integer"}
        },
        "required": ["command"],
        "additionalProperties": false
    }));
    tool
}

/// A `read` tool named `name`, whose schema takes a required string `path`
/// and optional integers `offset` and `limit`.
fn read_tool(name: &'static str) -> TestTool {
    let mut tool = TestTool::reads(name, "file contents");
    tool.schema = Some(json!({
        "type": "object",
        "properties": {
            "path": {"type": "string"},
            "offset": {"type": "integer"},
            "limit": {"type": "integer"}
        },
        "required": ["path"],
        "additionalProperties": false
    }));
    tool
}

/// Read-shaped arguments the `shell` tool rejects: no `command`.
fn read_shaped() -> Value {
    json!({"path": "x.rs", "offset": 1, "limit": 20})
}

/// The field lines a `shell` call with [`read_shaped`] fails with, in no
/// promised order: `docs/tools.md`, "Before a call runs", promises one line
/// per bad field, not their order or count, so tests assert each line by
/// containment.
fn shell_field_lines() -> [&'static str; 3] {
    [
        "`/command`: missing",
        "`/offset`: not allowed",
        "`/path`: not allowed",
    ]
}

/// Asserts `text` is a [`read_shaped`] schema failure with no suggestion: it
/// holds each of [`shell_field_lines`] and no `These arguments fit` line.
fn assert_shell_fields_without_suggestion(text: &str) {
    for line in shell_field_lines() {
        assert!(text.contains(line), "missing field line {line}: {text}");
    }
    assert!(
        !text.contains("These arguments fit"),
        "unexpected suggestion: {text}"
    );
}

/// The event kinds of a one-call turn whose call fails its schema check: the
/// reply's one request completes as `failed` without ever starting, and the
/// turn's second reply ends it (`docs/loop.md`, "One step", "Tool calls that
/// do not run" and "Ending a turn"; `docs/tools.md`, "Before a call runs").
/// Pinning the whole list pins the one completion too (`docs/testing.md`,
/// "Event streams").
const ONE_FAILED_CALL_TURN: &[&str] = &[
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
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

#[test]
fn shell_call_with_read_shaped_arguments_names_the_matching_tool() {
    // The model chose `shell` with `read`'s arguments (`docs/tools.md`,
    // "Before a call runs"): the call still fails on `shell` and never
    // runs, and the failure's last line names the one other tool whose
    // schema fits what the model sent.
    let shell = Arc::new(shell_tool());
    let read = Arc::new(read_tool("read"));
    let (_, lines) = turn(
        vec![shell.clone(), read.clone()],
        &[("shell", read_shaped())],
    );
    assert_eq!(kinds(&lines), ONE_FAILED_CALL_TURN);
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["status"], "failed");
    assert_eq!(done[0].payload["error"]["code"], "invalid_arguments");
    let text = text_first(done[0]);
    for line in shell_field_lines() {
        assert!(text.contains(line), "missing field line {line}: {text}");
    }
    // The suggestion is a last line naming the one match (`docs/tools.md`,
    // "Before a call runs").
    assert!(
        text.ends_with("\nThese arguments fit `read`."),
        "missing suggestion: {text}"
    );
    // The recorded call keeps the model's choice of tool.
    let requested = lines
        .iter()
        .find(|l| l.kind == "tool_call_requested")
        .unwrap();
    assert_eq!(requested.payload["name"], "shell");
    assert_eq!(requested.payload["arguments"], read_shaped());
    // Neither the named tool nor the matching one runs, and the match
    // never reaches the candidate's effects function.
    assert!(shell.ran().is_empty());
    assert!(read.ran().is_empty());
    assert!(
        !read
            .trace
            .lock()
            .unwrap()
            .iter()
            .any(|note| note == "effects read")
    );
}

#[test]
fn two_matching_tools_add_no_suggestion() {
    // Two strict schemas fit what the model sent, so naming one would
    // guess: the failure keeps only its field lines.
    let shell = Arc::new(shell_tool());
    let read = Arc::new(read_tool("read"));
    let other = Arc::new(read_tool("read2"));
    let (_, lines) = turn(
        vec![shell.clone(), read.clone(), other.clone()],
        &[("shell", read_shaped())],
    );
    assert_eq!(kinds(&lines), ONE_FAILED_CALL_TURN);
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["error"]["code"], "invalid_arguments");
    assert_shell_fields_without_suggestion(text_first(done[0]));
    assert!(shell.ran().is_empty());
    assert!(read.ran().is_empty());
    assert!(other.ran().is_empty());
}

#[test]
fn only_a_permissive_schema_match_adds_no_suggestion() {
    // A schema without `properties` matches every object and tells the
    // model nothing, so it never counts as a match.
    let shell = Arc::new(shell_tool());
    let loose = Arc::new(TestTool::reads("loose", "loose output"));
    let (_, lines) = turn(
        vec![shell.clone(), loose.clone()],
        &[("shell", read_shaped())],
    );
    assert_eq!(kinds(&lines), ONE_FAILED_CALL_TURN);
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["error"]["code"], "invalid_arguments");
    assert_shell_fields_without_suggestion(text_first(done[0]));
    assert!(shell.ran().is_empty());
    assert!(loose.ran().is_empty());
}

#[test]
fn an_effectively_deferred_match_adds_no_suggestion() {
    // `read` fits what the model sent but is effectively deferred, so the
    // request never declared it in full: no suggestion names it.
    let shell = Arc::new(shell_tool());
    let read = Arc::new(read_tool("read"));
    let tools: Vec<Arc<dyn Tool>> = vec![shell.clone(), support::deferred(Arc::clone(&read))];
    let factory: r#loop::ToolSearchFactory =
        Arc::new(|_, _| Arc::new(TestTool::reads("tool_search", "found")) as Arc<dyn Tool>);
    let mut session = Session::with_deferring_tools(
        vec![
            calls_reply("", &[("shell", read_shaped())]),
            Scripted::text("Done."),
        ],
        tools,
        factory,
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    assert_eq!(kinds(&lines), ONE_FAILED_CALL_TURN);
    // The preamble really did defer `read`: the suggestion is skipped
    // because of the effective deferred set, not a schema miss.
    let built = lines.iter().find(|l| l.kind == "preamble_built").unwrap();
    let read_row = built.payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "read")
        .unwrap();
    assert_eq!(read_row["deferred"], true);
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["error"]["code"], "invalid_arguments");
    assert_shell_fields_without_suggestion(text_first(done[0]));
    assert!(shell.ran().is_empty());
    assert!(read.ran().is_empty());
}

#[test]
fn a_match_only_after_repair_adds_no_suggestion() {
    // The model sent `"20"` where `read` needs an integer, and only the
    // repair for `shell` would make it valid: candidates are checked
    // against what the model sent, not the repaired object.
    let shell = Arc::new(shell_tool());
    let read = Arc::new(read_tool("read"));
    let sent = json!({"path": "x", "limit": "20"});
    let (_, lines) = turn(
        vec![shell.clone(), read.clone()],
        &[("shell", sent.clone())],
    );
    assert_eq!(kinds(&lines), ONE_FAILED_CALL_TURN);
    let requested = lines
        .iter()
        .find(|l| l.kind == "tool_call_requested")
        .unwrap();
    assert_eq!(
        requested.payload["repaired"],
        json!({"path": "x", "limit": 20})
    );
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["error"]["code"], "invalid_arguments");
    let text = text_first(done[0]);
    for line in ["`/command`: missing", "`/path`: not allowed"] {
        assert!(text.contains(line), "missing field line {line}: {text}");
    }
    assert!(
        !text.contains("These arguments fit"),
        "unexpected suggestion: {text}"
    );
    assert!(shell.ran().is_empty());
    assert!(read.ran().is_empty());
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
fn required_nullable_arguments_are_absent_and_replayed_as_written() {
    for written in [
        json!({"city": "Paris", "days": null}),
        json!({"city": "Paris"}),
    ] {
        let mut tool = TestTool::reads("get_weather", "Sunny.");
        tool.schema = Some(json!({
            "type": "object",
            "properties": {"city": {"type": "string"}, "days": {"type": ["integer", "null"]}},
            "required": ["city", "days"], "additionalProperties": false
        }));
        let tool = Arc::new(tool);
        let (session, lines) = turn(vec![tool.clone()], &[("get_weather", written.clone())]);
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
                "tool_call_started",
                "tool_call_completed",
                "step_started",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed"
            ]
        );
        let requested = lines
            .iter()
            .find(|line| line.kind == "tool_call_requested")
            .unwrap();
        assert_eq!(requested.payload["arguments"], written);
        if written.get("days").is_some() {
            assert_eq!(requested.payload["repaired"], json!({"city": "Paris"}));
            assert_eq!(
                requested.payload["repairs"],
                json!([{"path": "/days", "fix": "null_dropped"}])
            );
        } else {
            assert_eq!(requested.payload.get("repairs"), None);
        }
        assert_eq!(completed(&lines)[0].payload["status"], "completed");
        assert_eq!(
            tool.ran(),
            [json!({"city": "Paris"}).as_object().unwrap().clone()]
        );
        let requests = session.requests();
        let Some(Input::ToolCall { call, .. }) = requests[1]
            .conversation
            .iter()
            .find(|input| matches!(input, Input::ToolCall { .. }))
        else {
            panic!("the call is sent back");
        };
        assert_eq!(call.arguments, written);
    }
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
    assert_eq!(text_first(done), "The effects function broke.");
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
            | Input::ToolCall { .. }
            | Input::ToolSearchResult { .. } => None,
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
    // and no person can answer them: each ends a `no_reviewer` deny.
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
        assert!(text_first(line).contains("may be incomplete"));
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
    let kept = text_first(done);
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
    let kept: Vec<&str> = text_first(completed(&lines)[0]).split('\n').collect();
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
    assert_eq!(text_first(done), full);
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
            clock.await_parked(due, fakes::Deadline::after(Duration::from_secs(10))),
            "the loop did not park until the interval ends"
        );
        assert!(
            tap.pending().iter().all(|l| l.kind != "tool_call_delta"),
            "a delta leaked before the clock moved"
        );
        clock.advance_when_parked(
            Some(due),
            None,
            Duration::from_millis(100),
            &fakes::Deadline::after(Duration::from_secs(10)),
        );
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
            always_reviewed: false,
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
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
            clock.await_parked(due, fakes::Deadline::after(Duration::from_secs(10))),
            "the loop did not park until the byte-paced interval ends"
        );
        assert!(
            tap.pending().iter().all(|l| l.kind != "tool_call_delta"),
            "a delta leaked before the clock moved"
        );
        clock.advance_when_parked(
            Some(due),
            None,
            interval,
            &fakes::Deadline::after(Duration::from_secs(10)),
        );
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

/// The hosted search a provider runs: it declares `network`, and fails the
/// test if the loop ever asks it to run.
struct HostedSearch;

impl Tool for HostedSearch {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "web_search".into(),
            description: String::new(),
            input_schema: json!({}),
            deferred: false,
            hosted: Some("web_search_20250305".into()),
        }
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, Value>,
    ) -> Result<contract::tool::Effects, EffectsError> {
        Ok(contract::tool::Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Network],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        panic!("a hosted search is never run by Fiber");
    }
}

/// A hosted tool with a strict schema that fits [`read_shaped`]: the
/// suggestion still skips it, since a hosted tool is sent as its type and
/// name only, never as a full function definition (`docs/tools.md`,
/// "Hosted by the provider").
struct HostedRead;

impl Tool for HostedRead {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "hosted_read".into(),
            description: String::new(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "offset": {"type": "integer"},
                    "limit": {"type": "integer"}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            deferred: false,
            hosted: Some("hosted_read_20250101".into()),
        }
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, Value>,
    ) -> Result<contract::tool::Effects, EffectsError> {
        Ok(contract::tool::Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Reads],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        panic!("a hosted search is never run by Fiber");
    }
}

#[test]
fn a_hosted_match_adds_no_suggestion() {
    // `hosted_read`'s schema fits what the model sent, but the request
    // declared it as a hosted type and name only: no suggestion names it.
    let shell = Arc::new(shell_tool());
    let (_, lines) = turn(
        vec![shell.clone(), Arc::new(HostedRead)],
        &[("shell", read_shaped())],
    );
    assert_eq!(kinds(&lines), ONE_FAILED_CALL_TURN);
    let done = completed(&lines);
    assert_eq!(done.len(), 1);
    assert_eq!(done[0].payload["error"]["code"], "invalid_arguments");
    assert_shell_fields_without_suggestion(text_first(done[0]));
    assert!(shell.ran().is_empty());
}

fn search_call_block() -> Value {
    json!({"type": "server_tool_use", "id": "srvtoolu_01", "name": "web_search",
        "input": {"query": "rust 1.90"}})
}

fn search_result_block() -> Value {
    json!({"type": "web_search_tool_result", "tool_use_id": "srvtoolu_01",
        "content": [{"type": "web_search_result", "url": "https://blog.rust-lang.org/"}]})
}

fn hosted_action() -> contract::provider::ReplyAction {
    use contract::events::{CallStatus, ToolCallCompleted, ToolCallRequested};
    contract::provider::ReplyAction::Hosted(contract::provider::HostedCall {
        call: ToolCallRequested {
            name: "web_search".into(),
            arguments: json!({"query": "rust 1.90"}),
            provider_id: Some(contract::ProviderCallId("srvtoolu_01".into())),
            repair: None,
            ran_by: None,
            provider_item: Some(search_call_block()),
        },
        completed: ToolCallCompleted {
            status: CallStatus::Completed,
            reason: None,
            error: None,
            process: None,
            content: vec![ContentPart::Text {
                text: "https://blog.rust-lang.org/".into(),
            }],
            details: None,
            artifact: None,
            kept: None,
            changes: None,
            control: None,
            changed_by: None,
            protected_paths: None,
            provider_item: Some(search_result_block()),
            hosts_surfaced: vec!["https://www.rust-lang.org/".into()],
            loaded: None,
        },
    })
}

/// A reply of `hosted_action` then `rest`'s actions.
fn hosted_reply(mut rest: Scripted) -> Scripted {
    let end = rest.end.as_mut().unwrap();
    end.actions.insert(0, hosted_action());
    rest
}

fn raw(item: Value) -> Input {
    Input::Assistant {
        model: support::MODEL.into(),
        text: String::new(),
        provider_item: Some(item),
    }
}

#[test]
fn a_hosted_search_is_logged_in_three_lines_and_never_reviewed_or_run() {
    let mut session = Session::with_tools(
        vec![hosted_reply(Scripted::text("Rust 1.90 is out."))],
        None,
        vec![Arc::new(HostedSearch)],
    );
    let reviewer = session.reviewer(Vec::new());
    session.inbox.send(delivery("search")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let at = lines
        .iter()
        .position(|l| l.kind == "tool_call_requested")
        .unwrap();
    let three = &lines[at..at + 3];
    assert_eq!(
        kinds(three),
        [
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed"
        ]
    );
    assert_eq!(three[0].action_id, three[1].action_id);
    assert_eq!(three[1].action_id, three[2].action_id);
    assert_eq!(three[0].payload["name"], "web_search");
    assert_eq!(three[0].payload["arguments"], json!({"query": "rust 1.90"}));
    assert_eq!(three[0].payload["provider_item"], search_call_block());
    assert_eq!(three[1].payload["effects"], json!(["network"]));
    assert_eq!(three[1].payload["reversible"], true);
    assert_eq!(three[2].payload["status"], "completed");
    assert_eq!(text_first(&three[2]), "https://blog.rust-lang.org/");
    assert_eq!(three[2].payload["provider_item"], search_result_block());
    let all = kinds(&lines);
    assert!(!all.contains(&"permission_requested"), "{all:?}");
    assert_eq!(
        all.iter().filter(|k| **k == "step_started").count(),
        1,
        "{all:?}"
    );
    assert_eq!(all.last(), Some(&"turn_completed"));
    assert!(reviewer.requests().is_empty());
}

#[test]
fn a_hosted_call_beside_an_ordinary_one_leaves_only_the_ordinary_one_to_run() {
    let tool = Arc::new(TestTool::reads("get_weather", "Sunny."));
    let mut session = Session::with_tools(
        vec![
            hosted_reply(calls_reply("", &[("get_weather", paris())])),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::new(HostedSearch), tool.clone()],
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(tool.ran().len(), 1);
    let lines = session.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|l| l.kind == "tool_call_completed")
            .count(),
        2,
        "the hosted call and the ordinary one"
    );
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    let sent = &requests[1].conversation;
    let first = sent
        .iter()
        .position(|i| *i == raw(search_call_block()))
        .unwrap();
    assert_eq!(sent[first + 1], raw(search_result_block()));
    let results = sent
        .iter()
        .filter(|i| matches!(i, Input::ToolResult { .. }))
        .count();
    assert_eq!(results, 1, "the hosted call is given no result of its own");
    assert!(
        sent.iter()
            .all(|i| !matches!(i, Input::ToolCall { call, .. } if call.name == "web_search"))
    );
}

#[test]
fn a_hosted_search_the_registry_does_not_know_declares_no_effects() {
    let mut session = Session::new(vec![hosted_reply(Scripted::text("Done."))], None);
    session.inbox.send(delivery("search")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    let started = lines
        .iter()
        .find(|l| l.kind == "tool_call_started")
        .unwrap();
    assert_eq!(started.payload["effects"], json!([]));
    assert_eq!(started.payload["reversible"], true);
}

#[test]
fn a_hosted_search_in_a_reply_cut_off_by_the_output_limit_is_still_logged() {
    let mut cut = hosted_reply(Scripted::text("Rust 1.9"));
    cut.end.as_mut().unwrap().finish = Finish::OutputLimit;
    let mut session = Session::with_tools(
        vec![cut, Scripted::text("Rust 1.90.")],
        None,
        vec![Arc::new(HostedSearch)],
    );
    session.inbox.send(delivery("search")).unwrap();
    session.turn();
    let lines = session.lines();
    let all = kinds(&lines);
    assert!(all.contains(&"tool_call_completed"), "{all:?}");
}

/// A hosted search whose tool declares the call always reviewed keeps the
/// key on its start.
struct AlwaysReviewedSearch;

impl Tool for AlwaysReviewedSearch {
    fn definition(&self) -> contract::provider::ToolDefinition {
        HostedSearch.definition()
    }

    fn effects(
        &self,
        _: &serde_json::Map<String, Value>,
    ) -> Result<contract::tool::Effects, EffectsError> {
        Ok(contract::tool::Effects {
            declared: DeclaredEffects {
                effects: vec![Effect::Network],
                reversible: true,
                paths: None,
                confined: false,
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: true,
            hosts: None,
        })
    }

    fn run(
        &self,
        _: &serde_json::Map<String, Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        panic!("a hosted search is never run by Fiber");
    }
}

#[test]
fn a_hosted_calls_start_keeps_the_registered_tools_always_reviewed() {
    let mut session = Session::with_tools(
        vec![hosted_reply(Scripted::text("Rust 1.90 is out."))],
        None,
        vec![Arc::new(AlwaysReviewedSearch)],
    );
    let reviewer = session.reviewer(Vec::new());
    session.inbox.send(delivery("search")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
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
            "assistant_message_delta",
            "tool_call_requested",
            "tool_call_started",
            "tool_call_completed",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let started = lines
        .iter()
        .find(|l| l.kind == "tool_call_started")
        .unwrap();
    assert_eq!(started.payload["effects"], json!(["network"]));
    assert_eq!(started.payload["always_reviewed"], true);
    assert!(reviewer.requests().is_empty());
}

#[test]
fn known_hosts_hosted_search_surfaces_hosts_before_network_calls_are_judged() {
    let mut lookup = TestTool::declaring("lookup", "Fetched.", vec![Effect::Network], None);
    lookup.hosts = Some(vec!["https://www.rust-lang.org/learn".into()]);
    let lookup = Arc::new(lookup);
    let mut session = Session::with_tools(
        vec![
            hosted_reply(calls_reply("", &[("lookup", paris())])),
            calls_reply("", &[("lookup", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::new(HostedSearch), lookup.clone()],
    );
    let reviewer = session.reviewer(vec![]);
    session.inbox.send(delivery("search")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
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
            "tool_call_started",
            "tool_call_completed",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed"
        ]
    );
    assert_eq!(
        completed(&lines)[0].payload["hosts_surfaced"],
        json!(["https://www.rust-lang.org/"])
    );
    assert_eq!(lookup.ran().len(), 2);
    assert!(reviewer.requests().is_empty());
}
