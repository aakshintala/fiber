//! The person's messages that still bind survive a handoff in the agent's
//! context (`docs/handoff.md`, "What the model sees after a handoff"), through the
//! built `fiber`: a session on a fake vendor server, reviewed by a scripted
//! model whose selection keeps turn 1's message.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use fakes::ProviderServer;
use serde_json::json;
use support::{Run, Setup, function_call, kinds, run_to_exit, text_reply, write_json};

/// `fiber <args>` run from the workspace to its exit.
fn fiber(setup: &Setup, args: &[&str]) -> Run {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    Run {
        code: output.status.code(),
        lines: stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

#[test]
fn a_handoff_carries_turn_ones_order_word_for_word_ahead_of_the_note() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        text_reply("Understood."),
        stream_call(),
        text_reply("The note."),
        text_reply("Done."),
    ])
    .unwrap();
    setup.provider(&server);
    // The reviewer decides the call, then keeps message 1 at the handoff.
    write_json(
        &setup.workspace().join("r.json"),
        &json!({"steps": [{"text": "allow"}, {"text": "1"}]}),
    );
    // A trigger of 5 tokens: turn 2's second request is over it.
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "handoff": {"tokens": 5},
                "reviewer": {"model": "scripted/r.json"}}),
    );

    let first = fiber(&setup, &["ask", "Do not modify files in this task."]);
    assert_eq!(first.code, Some(0), "{}", first.stderr);
    let id = first.session_id().to_owned();
    let second = fiber(&setup, &["ask", "--resume", &id, "List the workspace."]);
    assert_eq!(second.code, Some(0), "{}", second.stderr);
    // The whole resumed run in order; `session_status` is set aside
    // (`docs/testing.md`, "Event streams").
    assert_eq!(
        kinds(&second.lines),
        [
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_delta",
            "tool_call_completed",
            "step_started",
            "handoff_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "handoff_completed",
            "opening_message",
            "usage_recorded",
            "usage_recorded",
            "reviewer_kept",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );

    // Requests: turn 1, turn 2's call, the note request, then the first
    // request after the handoff.
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    let after = String::from_utf8(requests[3].body.clone()).unwrap();
    let order = after
        .find("Do not modify files in this task.")
        .expect("turn 1's message is in the first request after the handoff");
    let note = after.find("The note.").expect("the note");
    assert!(order < note, "the kept message comes ahead of the note");
    assert_eq!(
        after.matches("Do not modify files in this task.").count(),
        1
    );
    // The note request, before the handoff, held it once too: the old
    // context, which the handoff replaced.
    assert!(
        String::from_utf8(requests[2].body.clone())
            .unwrap()
            .contains("Do not modify files in this task.")
    );
}

/// A reply calling the shell, which the scripted reviewer allows.
fn stream_call() -> fakes::Response {
    support::stream(&[function_call(
        "c1",
        "shell",
        &json!({"command": "echo ran"}),
    )])
}
