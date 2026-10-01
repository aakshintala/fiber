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
use std::time::Duration;

use contract::events::TurnOutcome;
use contract::provider::{Finish, Input};
use contract::shapes::{ContentPart, Effect};
use contract::tool::{Bound, Tool};
use contract::{Envelope, ErrorCode};
use fakes::Scripted;
use serde_json::{Value, json};

use support::{Session, TestTool, calls_reply, kinds, message};

/// A session whose first reply makes `calls` and whose second says "Done.",
/// with `tools` registered. Runs one turn and returns its lines.
fn turn(tools: Vec<Arc<dyn Tool>>, calls: &[(&str, Value)]) -> (Session, Vec<Envelope>) {
    let mut session = Session::with_tools(
        vec![calls_reply("", calls), Scripted::text("Done.")],
        None,
        tools,
    );
    session.inbox.send(message("go")).unwrap();
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
        })
    );
}

#[test]
fn a_failed_result_completes_failed_with_its_code() {
    let tool = Arc::new(TestTool::failing("get_weather", ErrorCode::ToolError));
    let (_, lines) = turn(vec![tool], &[("get_weather", paris())]);
    let done = completed(&lines)[0];
    assert_eq!(done.payload["status"], "failed");
    assert_eq!(done.payload["error"]["code"], "tool_error");
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
    tool.effects = Err("The effects function broke.".into());
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
    slow.delay = Duration::from_millis(100);
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
    let session_root = std::env::temp_dir();
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
            &session_root.join("elsewhere").display().to_string(),
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
    let (_, lines) = turn(tools, &calls);
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
    session.inbox.send(message("go")).unwrap();
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
    let message = lines
        .iter()
        .find(|l| l.kind == "assistant_message_completed")
        .unwrap();
    assert_eq!(message.payload["text"], "Let me");
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
    session.inbox.send(message("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Failed));
    let lines = session.lines();
    assert_eq!(completed(&lines).len(), 2);
    let end = lines.last().unwrap();
    assert_eq!(end.payload["error"]["code"], "output_truncated");
    assert_eq!(session.requests().len(), 2);

    // The next turn starts with no cut-off behind it.
    session.inbox.send(message("again")).unwrap();
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
    session.inbox.send(message("go")).unwrap();
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
    let (tool, full) = long(20 * 1024, Bound::default());
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
    session.inbox.send(message("go")).unwrap();
    session.turn();
    // Two model requests at two each, and three calls at two each.
    assert_eq!(session.log.fsyncs() - before, 2 * 2 + 3 * 2);
}
