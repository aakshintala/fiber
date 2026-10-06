//! Tests for rendering `instruction_file` and `date_changed` lines: every
//! row of the change table, and the resume identity of the rendering.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::collections::BTreeMap;

use contract::events::{
    Event, InstructionFile, InstructionReason, InstructionSent, ToolCallRequested,
};
use contract::provider::Input;
use contract::{ActionId, Envelope, Seq, SessionId};
use serde_json::json;

use super::render;
use crate::opening;
use crate::prompt::PromptInputs;

fn line(kind: &str, event: &Event, action: Option<&str>) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_test".into()),
        ts: 1,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|action| ActionId(action.into())),
        seq: Some(Seq(1)),
        payload: event.payload().unwrap(),
    }
}

fn file(
    path: &str,
    reason: InstructionReason,
    sent: InstructionSent,
    content: Option<&str>,
) -> Event {
    Event::InstructionFile(InstructionFile {
        path: path.into(),
        reason,
        extension: None,
        content: content.map(str::to_owned),
        sent,
    })
}

fn user_text(input: &Input) -> &str {
    let Input::User { text } = input else {
        panic!("a User message, got {input:?}");
    };
    text
}

#[test]
fn changed_with_diff_renders_the_recomputed_diff() {
    let mut had = BTreeMap::from([("/w/AGENTS.md".to_owned(), "a\nb\n".to_owned())]);
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &file(
            "/w/AGENTS.md",
            InstructionReason::Changed,
            InstructionSent::Diff,
            Some("a\nc\n"),
        ),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(conversation.len(), 1);
    let text = user_text(&conversation[0]);
    assert!(text.contains("Apply this diff"), "{text}");
    assert!(text.contains("--- /w/AGENTS.md"), "{text}");
    assert!(text.contains("-b\n"), "{text}");
    assert!(text.contains("+c\n"), "{text}");
    // What the model had moves on.
    assert_eq!(had.get("/w/AGENTS.md"), Some(&"a\nc\n".to_owned()));
}

#[test]
fn changed_with_full_text_renders_the_replacement() {
    let mut had = BTreeMap::from([("/w/AGENTS.md".to_owned(), "ab\n".to_owned())]);
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &file(
            "/w/AGENTS.md",
            InstructionReason::Changed,
            InstructionSent::Full,
            Some("cd\n"),
        ),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(conversation.len(), 1);
    let text = user_text(&conversation[0]);
    assert!(text.contains("This full text replaces"), "{text}");
    assert!(text.contains("cd\n"), "{text}");
    assert_eq!(had.get("/w/AGENTS.md"), Some(&"cd\n".to_owned()));
}

#[test]
fn created_renders_the_new_file() {
    let mut had = BTreeMap::new();
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &file(
            "/w/AGENTS.md",
            InstructionReason::Created,
            InstructionSent::Full,
            Some("Leaf.\n"),
        ),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(conversation.len(), 1);
    let text = user_text(&conversation[0]);
    assert!(text.contains("a new instruction file appeared"), "{text}");
    // `{dir}` is the file's parent directory, byte for byte: neither an
    // empty nor a wrong directory reads the same.
    assert!(
        text.contains("It applies to /w and everything below it."),
        "{text}"
    );
    assert!(text.contains("### /w/AGENTS.md"), "{text}");
    assert!(text.contains("Leaf.\n"), "{text}");
    assert_eq!(had.get("/w/AGENTS.md"), Some(&"Leaf.\n".to_owned()));
}

#[test]
fn subdirectory_renders_the_reached_file() {
    let mut had = BTreeMap::new();
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &file(
            "/w/sub/AGENTS.md",
            InstructionReason::Subdirectory,
            InstructionSent::Full,
            Some("Sub.\n"),
        ),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(conversation.len(), 1);
    let text = user_text(&conversation[0]);
    assert!(text.contains("has its own instruction file"), "{text}");
    // Both `{dir}` slots name the reached directory, byte for byte.
    assert!(
        text.contains("you worked in /w/sub, which has its own instruction file."),
        "{text}"
    );
    assert!(
        text.contains("It applies to /w/sub and everything below it"),
        "{text}"
    );
    assert!(text.contains("### /w/sub/AGENTS.md"), "{text}");
    assert_eq!(had.get("/w/sub/AGENTS.md"), Some(&"Sub.\n".to_owned()));
}

#[test]
fn deleted_renders_one_line_and_forgets_the_path() {
    let mut had = BTreeMap::from([("/w/AGENTS.md".to_owned(), "Leaf.\n".to_owned())]);
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &file(
            "/w/AGENTS.md",
            InstructionReason::Deleted,
            InstructionSent::Deleted,
            None,
        ),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(conversation.len(), 1);
    let text = user_text(&conversation[0]);
    assert!(text.contains("no longer apply"), "{text}");
    assert!(!had.contains_key("/w/AGENTS.md"));
}

#[test]
fn own_edit_and_sent_none_render_nothing_but_move_what_the_model_had() {
    let mut had = BTreeMap::from([("/w/AGENTS.md".to_owned(), "Leaf.\n".to_owned())]);
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &file(
            "/w/AGENTS.md",
            InstructionReason::OwnEdit,
            InstructionSent::None,
            Some("Revised.\n"),
        ),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    render(
        &mut conversation,
        &file(
            "/w/AGENTS.md",
            InstructionReason::Changed,
            InstructionSent::None,
            Some("Revised again.\n"),
        ),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert!(conversation.is_empty());
    assert_eq!(
        had.get("/w/AGENTS.md"),
        Some(&"Revised again.\n".to_owned())
    );
}

#[test]
fn date_changed_renders_the_new_date() {
    let mut had = BTreeMap::new();
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &Event::DateChanged(contract::events::DateChanged {
            date: "2023-11-15".into(),
        }),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(conversation.len(), 1);
    assert!(user_text(&conversation[0]).contains("2023-11-15"));
}

#[test]
fn live_rendering_equals_rebuild() {
    let held = fakes::TempDir::new("fiber-conversation");
    let home = held.path();
    let workspace = home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("AGENTS.md"), "Leaf.\n").unwrap();
    let clock = fakes::clock::FakeClock::new();
    let owned = clock.clone();
    let clock: std::sync::Arc<dyn contract::clock::Clock> = owned;
    let inputs = PromptInputs::new(
        home.to_path_buf(),
        "/bin/sh".into(),
        home.join("events.jsonl").display().to_string(),
        clock,
    );
    let message = opening::collect(&inputs, &workspace).message;
    let path = message.instruction_files[0].path.clone();
    let events = vec![
        Event::OpeningMessage(message),
        file(
            &path,
            InstructionReason::Changed,
            InstructionSent::Diff,
            Some("Leaf, revised.\n"),
        ),
        file(
            &path,
            InstructionReason::OwnEdit,
            InstructionSent::None,
            Some("Leaf, mine.\n"),
        ),
        Event::DateChanged(contract::events::DateChanged {
            date: "2023-11-15".into(),
        }),
        file(
            &path,
            InstructionReason::Deleted,
            InstructionSent::Deleted,
            None,
        ),
    ];
    let mut live = Vec::new();
    let mut had = BTreeMap::new();
    for event in &events {
        render(
            &mut live,
            event,
            None,
            "fake/model-1",
            &mut had,
            &mut crate::handoff::Carry::default(),
        );
    }
    let lines: Vec<Envelope> = events
        .iter()
        .map(|event| {
            let kind = if matches!(event, Event::OpeningMessage(_)) {
                "opening_message"
            } else if matches!(event, Event::InstructionFile(_)) {
                "instruction_file"
            } else if matches!(event, Event::DateChanged(_)) {
                "date_changed"
            } else {
                unreachable!("only change lines are rendered here");
            };
            line(kind, event, None)
        })
        .collect();
    let rebuilt = super::rebuild(&lines, "fake/model-1").unwrap();
    assert_eq!(live, rebuilt);
}

#[test]
fn a_change_after_a_crash_follows_the_fixed_result() {
    // A call with no result, then a change written at the next turn start:
    // the fixed result flushes before the change message, so no message
    // separates the call from its result.
    let requested = Event::ToolCallRequested(ToolCallRequested {
        name: "read".into(),
        arguments: json!({"city": "Paris"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    });
    let changed = file(
        "/w/AGENTS.md",
        InstructionReason::Changed,
        InstructionSent::Full,
        Some("Revised.\n"),
    );
    let lines = vec![
        line("tool_call_requested", &requested, Some("a_1")),
        line("instruction_file", &changed, None),
    ];
    let conversation = super::rebuild(&lines, "fake/model-1").unwrap();
    assert_eq!(conversation.len(), 3);
    assert!(matches!(
        &conversation[0],
        Input::ToolCall { action_id, .. } if action_id.0 == "a_1"
    ));
    assert!(matches!(
        &conversation[1],
        Input::ToolResult { action_id, is_error, .. } if action_id.0 == "a_1" && *is_error
    ));
    assert!(matches!(&conversation[2], Input::User { .. }));
    assert!(user_text(&conversation[2]).contains("This full text replaces"));
}

#[test]
fn dir_of_falls_back_to_empty_without_a_parent() {
    let mut had = BTreeMap::new();
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &file(
            "AGENTS.md",
            InstructionReason::Created,
            InstructionSent::Full,
            Some("Leaf.\n"),
        ),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(conversation.len(), 1);
    assert!(user_text(&conversation[0]).contains("Leaf.\n"));
}

fn orphaned(id: &str) -> Event {
    Event::JobCompleted(contract::events::JobCompleted {
        job_id: contract::JobId(id.into()),
        status: contract::events::Outcome::Failed,
        error: Some(contract::shapes::Failure {
            code: contract::ErrorCode::Orphaned,
            message: "The process that ran this job died; it may still be running.".into(),
            retry_after: None,
            provider: None,
        }),
        process: None,
        output_tail: None,
    })
}

#[test]
fn a_job_notice_after_a_crash_follows_the_fixed_result() {
    // A call that started a job, then a crash: the resume's orphan line
    // comes after the call's fixed result, as the live loop sends it.
    let requested = Event::ToolCallRequested(ToolCallRequested {
        name: "shell".into(),
        arguments: json!({"command": "npm test"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    });
    let started = Event::ToolCallStarted(contract::events::ToolCallStarted {
        declared: contract::shapes::DeclaredEffects {
            effects: Vec::new(),
            reversible: true,
            paths: None,
        },
        arguments: None,
        changed_by: None,
    });
    let job = Event::JobStarted(contract::events::JobStarted {
        job_id: contract::JobId("j_1".into()),
        tool: Some("shell".into()),
        extension: None,
        description: "npm test".into(),
        output_path: "artifacts/j_1.log".into(),
    });
    let cut = vec![
        line("tool_call_requested", &requested, Some("a_1")),
        line("tool_call_started", &started, Some("a_1")),
        line("job_started", &job, Some("a_1")),
    ];
    // Live: the rebuild of the cut log, then the orphan line rendered as
    // the resume writes it.
    let mut live = super::rebuild(&cut, "fake/model-1").unwrap();
    render(
        &mut live,
        &orphaned("j_1"),
        None,
        "fake/model-1",
        &mut BTreeMap::new(),
        &mut crate::handoff::Carry::default(),
    );
    let mut whole = cut.clone();
    whole.push(line("job_completed", &orphaned("j_1"), None));
    let rebuilt = super::rebuild(&whole, "fake/model-1").unwrap();
    assert_eq!(live, rebuilt);
    assert_eq!(rebuilt.len(), 3);
    assert!(matches!(
        &rebuilt[1],
        Input::ToolResult { action_id, text, is_error: true, .. } if action_id.0 == "a_1" && text.contains("may have run")
    ));
    assert_eq!(
        user_text(&rebuilt[2]),
        "Fiber: background job j_1 ended: failed.\nThe process that ran this job died; it may still be running."
    );
}

#[test]
fn a_job_completed_renders_a_notice_only_without_an_action() {
    let notice = line("job_completed", &orphaned("j_1"), None);
    let record = line("job_completed", &orphaned("j_2"), Some("a_1"));
    let jobs = Event::TurnStarted(contract::events::TurnStarted {
        input: vec![contract::events::InputItem::Jobs {
            job_ids: vec![contract::JobId("j_1".into())],
        }],
    });
    let lines = vec![
        line("turn_started", &jobs, None),
        record.clone(),
        notice.clone(),
    ];
    let rebuilt = super::rebuild(&lines, "fake/model-1").unwrap();
    // The `jobs` item and the record under an action render nothing.
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(
        user_text(&rebuilt[0]),
        "Fiber: background job j_1 ended: failed.\nThe process that ran this job died; it may still be running."
    );
    let mut live = Vec::new();
    for (event, action) in [
        (jobs, None),
        (orphaned("j_2"), Some(ActionId("a_1".into()))),
        (orphaned("j_1"), None),
    ] {
        render(
            &mut live,
            &event,
            action.as_ref(),
            "fake/model-1",
            &mut BTreeMap::new(),
            &mut crate::handoff::Carry::default(),
        );
    }
    assert_eq!(live, rebuilt);
}

#[test]
fn a_job_record_under_an_action_does_not_flush_its_call() {
    // A `wait` record continues its call's batch: the call's result, not a
    // fixed one, follows it.
    let requested = Event::ToolCallRequested(ToolCallRequested {
        name: "jobs".into(),
        arguments: json!({"action": "wait"}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    });
    let completed = Event::ToolCallCompleted(contract::events::ToolCallCompleted {
        status: contract::events::CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![contract::shapes::ContentPart::Text {
            text: "Job j_1 failed.\n".into(),
        }],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    });
    let lines = vec![
        line("tool_call_requested", &requested, Some("a_1")),
        line("job_completed", &orphaned("j_1"), Some("a_1")),
        line("tool_call_completed", &completed, Some("a_1")),
    ];
    let rebuilt = super::rebuild(&lines, "fake/model-1").unwrap();
    assert_eq!(rebuilt.len(), 2);
    assert!(matches!(
        &rebuilt[1],
        Input::ToolResult { text, is_error: false, .. } if text == "Job j_1 failed.\n"
    ));
}

fn call(name: &str) -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: name.into(),
        arguments: json!({}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn result(text: &str) -> Event {
    Event::ToolCallCompleted(contract::events::ToolCallCompleted {
        status: contract::events::CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![contract::shapes::ContentPart::Text { text: text.into() }],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    })
}

/// The kinds of input `conversation` holds, with each result's action.
fn shape(conversation: &[Input]) -> Vec<String> {
    conversation
        .iter()
        .map(|input| match input {
            Input::User { .. } => "user".to_owned(),
            Input::ToolCall { action_id, .. } => format!("call {}", action_id.0),
            Input::ToolResult { action_id, .. } => format!("result {}", action_id.0),
            Input::Assistant { .. } => "assistant".to_owned(),
            Input::Reasoning { .. } => "reasoning".to_owned(),
        })
        .collect()
}

#[test]
fn a_notice_logged_inside_a_batch_renders_after_its_last_result() {
    // A resume finishing a suspended turn logs its orphan notice before
    // the open batch's results: the notice renders after them.
    let lines = vec![
        line("tool_call_requested", &call("a"), Some("a_1")),
        line("tool_call_requested", &call("b"), Some("a_2")),
        line("job_completed", &orphaned("j_1"), None),
        line("tool_call_completed", &result("one"), Some("a_1")),
        line("tool_call_completed", &result("two"), Some("a_2")),
    ];
    let rebuilt = super::rebuild(&lines, "fake/model-1").unwrap();
    assert_eq!(
        shape(&rebuilt),
        ["call a_1", "call a_2", "result a_1", "result a_2", "user"]
    );
    assert!(user_text(&rebuilt[4]).contains("j_1"));
}

#[test]
fn a_notice_behind_an_open_batch_renders_at_the_end() {
    // Rebuilt on resume with the batch still open: no fixed result, and the
    // notice waits behind the call.
    let lines = vec![
        line("tool_call_requested", &call("a"), Some("a_1")),
        line("job_completed", &orphaned("j_1"), None),
    ];
    // `rebuild` has no turn to finish: a notice still held at the end of
    // the log ends the conversation. A result logged before its call
    // leaves the call outstanding to the end.
    let mut misordered = vec![line("tool_call_completed", &result("one"), Some("a_1"))];
    misordered.extend(lines.clone());
    assert_eq!(
        shape(&super::rebuild(&misordered, "fake/model-1").unwrap()),
        ["result a_1", "call a_1", "user"]
    );
    let open = std::collections::HashSet::from([ActionId("a_1".into())]);
    let (rebuilt, _, held, _) = super::rebuild_and_sent(&lines, "fake/model-1", &open).unwrap();
    // Held apart for the finishing turn to release after the results.
    assert_eq!(shape(&rebuilt), ["call a_1"]);
    assert_eq!(shape(&held), ["user"]);
}

fn image_result() -> Event {
    Event::ToolCallCompleted(contract::events::ToolCallCompleted {
        status: contract::events::CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![
            contract::shapes::ContentPart::Text {
                text: "Image: 8x4 image/png.\n".into(),
            },
            contract::shapes::ContentPart::Image {
                path: "artifacts/i_1.png".into(),
                mime_type: "image/png".into(),
                width: 8,
                height: 4,
            },
        ],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    })
}

fn expected_image_result() -> Input {
    Input::ToolResult {
        action_id: ActionId("a_1".into()),
        text: "Image: 8x4 image/png.\n".into(),
        is_error: false,
        images: vec![contract::provider::ImageRef {
            path: "artifacts/i_1.png".into(),
            mime_type: "image/png".into(),
            width: 8,
            height: 4,
        }],
    }
}

#[test]
fn an_image_part_becomes_an_image_ref_on_the_result() {
    let lines = vec![
        line("tool_call_requested", &call("read"), Some("a_1")),
        line("tool_call_completed", &image_result(), Some("a_1")),
    ];
    let rebuilt = super::rebuild(&lines, "fake/model-1").unwrap();
    assert_eq!(rebuilt.len(), 2);
    assert_eq!(rebuilt[1], expected_image_result());
}

#[test]
fn the_free_renderer_carries_image_refs_too() {
    let mut out = Vec::new();
    render(
        &mut out,
        &image_result(),
        Some(&ActionId("a_1".into())),
        "fake/model-1",
        &mut BTreeMap::new(),
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(out, vec![expected_image_result()]);
}

#[test]
fn a_fixed_result_for_a_call_that_never_completed_holds_no_image() {
    let lines = vec![line("tool_call_requested", &call("read"), Some("a_1"))];
    let rebuilt = super::rebuild(&lines, "fake/model-1").unwrap();
    assert!(matches!(
        &rebuilt[1],
        Input::ToolResult { images, is_error: true, .. } if images.is_empty()
    ));
}
