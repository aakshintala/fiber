//! Configured `tools."<name>".max_result_bytes` as the bound the loop
//! applies (`docs/tools.md`, "Bounded results"): through the loop's public
//! API, with tools passed through `capped` before they are registered.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code, helpers included"
)]

mod support;

use std::sync::Arc;

use contract::Envelope;
use contract::events::TurnOutcome;
use contract::tool::{Bound, Tool};
use fakes::Scripted;
use r#loop::{ResultCaps, capped};
use serde_json::{Value, json};

use support::{Session, TestTool, calls_reply, delivery, kinds};

/// A session whose first reply makes `calls` and whose second says "Done.",
/// with `tools` passed through `capped` with `caps` before they are
/// registered. Runs one turn and returns its lines.
fn turn(
    tools: Vec<Arc<dyn Tool>>,
    caps: &ResultCaps,
    calls: &[(&str, Value)],
) -> (Session, Vec<Envelope>) {
    let wrapped = capped(
        tools
            .into_iter()
            .map(|tool| ("builtin".to_owned(), tool))
            .collect(),
        caps,
    );
    let tools: Vec<Arc<dyn Tool>> = wrapped.into_iter().map(|(_, tool)| tool).collect();
    let mut session = Session::with_tools(
        vec![calls_reply("", calls), Scripted::text("Done.")],
        None,
        tools,
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = session.lines();
    // The complete turn, in order: a `tool_call_delta` line is ephemeral,
    // so it is dropped before comparing (`docs/testing.md`, "Event
    // streams"). The `cat` tool only reads, so no `permission_` line is
    // written (`docs/permissions.md`, "Fast paths").
    assert_eq!(
        kinds(&lines)
            .into_iter()
            .filter(|kind| *kind != "tool_call_delta")
            .collect::<Vec<_>>(),
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
            // "Done." streams as two deltas.
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    (session, lines)
}

fn paris() -> Value {
    json!({"city": "Paris"})
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

/// A tool returning `size` bytes of text, cut to `bound`, plus one image
/// part.
fn long(size: usize, bound: Bound) -> (Arc<TestTool>, String) {
    let full: String = (0..size)
        .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap()))
        .collect();
    let mut tool = TestTool::reads("cat", &full);
    tool.output
        .content
        .push(contract::shapes::ContentPart::Image {
            path: "artifacts/i.png".into(),
            mime_type: "image/png".into(),
            width: 1,
            height: 1,
        });
    tool.bound = bound;
    (Arc::new(tool), full)
}

fn caps(entries: &[(&str, u64)]) -> ResultCaps {
    entries
        .iter()
        .map(|(name, cap)| ((*name).to_owned(), *cap))
        .collect()
}

#[test]
fn a_cap_smaller_than_the_tools_own_keeps_that_many_bytes_and_moves_the_rest_to_an_artifact() {
    let (tool, full) = long(20 * 1024, Bound::DEFAULT);
    let (session, lines) = turn(
        vec![tool as Arc<dyn Tool>],
        &caps(&[("cat", 1000)]),
        &[("cat", paris())],
    );
    let done = completed(&lines)[0];
    let id = done.action_id.clone().unwrap().0;
    let artifact = format!("artifacts/{id}.txt");
    assert_eq!(done.payload["artifact"], artifact.as_str());
    let path = session.dir.join(&artifact);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), full);
    let kept = text(done);
    let (head, notice) = kept.split_once('\n').unwrap();
    assert_eq!(head, &full[..1000]);
    assert_eq!(
        notice,
        format!(
            "[19480 bytes cut. The full output is in {}; read it with `read`.]",
            path.display()
        )
    );
    assert_eq!(done.payload["content"][1]["type"], "image");
}

#[test]
fn a_cap_larger_than_the_tools_own_leaves_a_result_uncut() {
    let (tool, full) = long(100, Bound { start: 10, end: 0 });
    let (_, lines) = turn(
        vec![tool as Arc<dyn Tool>],
        &caps(&[("cat", 200)]),
        &[("cat", paris())],
    );
    let done = completed(&lines)[0];
    assert_eq!(text(done), full);
    assert_eq!(done.payload.get("artifact"), None);
}

#[test]
fn a_cap_on_a_both_ends_tool_keeps_both_ends_in_proportion() {
    let (tool, full) = long(100, Bound { start: 10, end: 20 });
    let (_, lines) = turn(
        vec![tool as Arc<dyn Tool>],
        &caps(&[("cat", 16)]),
        &[("cat", paris())],
    );
    let kept: Vec<&str> = text(completed(&lines)[0]).split('\n').collect();
    assert_eq!(kept.len(), 3);
    assert_eq!(kept[0], &full[..6]);
    assert!(kept[1].starts_with("[84 bytes cut."), "{}", kept[1]);
    assert_eq!(kept[2], &full[90..]);
}

#[test]
fn a_zero_cap_keeps_only_the_notice() {
    let (tool, full) = long(100, Bound::DEFAULT);
    let (session, lines) = turn(
        vec![tool as Arc<dyn Tool>],
        &caps(&[("cat", 0)]),
        &[("cat", paris())],
    );
    let done = completed(&lines)[0];
    let kept = text(done);
    let (head, notice) = kept.split_once('\n').unwrap();
    assert_eq!(head, "");
    assert!(notice.starts_with("[100 bytes cut."), "{notice}");
    let artifact = done.payload["artifact"].as_str().unwrap();
    assert_eq!(
        std::fs::read_to_string(session.dir.join(artifact)).unwrap(),
        full
    );
}
