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
    let Input::User { text, .. } = input else {
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
fn created_section_file_names_the_section() {
    let mut had = BTreeMap::new();
    let mut conversation = Vec::new();
    render(
        &mut conversation,
        &Event::InstructionFile(InstructionFile {
            path: "/h/notes/a.md".into(),
            reason: InstructionReason::Created,
            extension: Some("fiber.test/notes".into()),
            content: Some("New notes.\n".into()),
            sent: InstructionSent::Full,
        }),
        None,
        "fake/model-1",
        &mut had,
        &mut crate::handoff::Carry::default(),
    );
    assert_eq!(conversation.len(), 1);
    let text = user_text(&conversation[0]);
    assert!(
        text.contains("a new file appeared in the fiber.test/notes extension's section."),
        "{text}"
    );
    assert!(!text.contains("applies to"), "{text}");
    assert!(text.contains("### /h/notes/a.md"), "{text}");
    assert!(text.contains("New notes.\n"), "{text}");
    assert_eq!(had.get("/h/notes/a.md"), Some(&"New notes.\n".to_owned()));
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
            retry_after_ms: None,
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
    let (rebuilt, _, held, _) = super::rebuild_and_sent(
        &lines,
        "fake/model-1",
        &open,
        crate::handoff::Carry::default(),
    )
    .unwrap();
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

#[test]
fn rebuild_stamps_a_tool_call_with_the_model_in_force() {
    // The push path: a call the log completes.
    let lines = vec![
        line("tool_call_requested", &call("read"), Some("a_1")),
        line("tool_call_completed", &result("hello"), Some("a_1")),
    ];
    let rebuilt = super::rebuild(&lines, "p/m").unwrap();
    let Input::ToolCall {
        model, call: made, ..
    } = &rebuilt[0]
    else {
        panic!("{rebuilt:?}");
    };
    assert_eq!(model, "p/m");
    assert_eq!(made.name, "read");
    // The crash-open path: a call with no result gets its fixed result,
    // and the call still carries the model.
    let open = vec![line("tool_call_requested", &call("read"), Some("a_1"))];
    let rebuilt = super::rebuild(&open, "p/m").unwrap();
    let Input::ToolCall { model, .. } = &rebuilt[0] else {
        panic!("{rebuilt:?}");
    };
    assert_eq!(model, "p/m");
    // The live path renders the same model.
    let mut live = Vec::new();
    render(
        &mut live,
        &call("read"),
        Some(&ActionId("a_1".into())),
        "p/m",
        &mut BTreeMap::new(),
        &mut crate::handoff::Carry::default(),
    );
    let Input::ToolCall { model, .. } = &live[0] else {
        panic!("{live:?}");
    };
    assert_eq!(model, "p/m");
}

// The resume window (`docs/events.md`, "Resume"; `docs/handoff.md`,
// "Resume"): one pass folds the session-wide facts and finds the window, and
// the window's rebuild equals the whole log's.

mod window {
    use std::collections::{BTreeMap, HashSet};
    use std::path::PathBuf;

    use contract::events::{
        AskStep, DecidedBy, Decision, Empty, Environment, Event, FiberExited, FiberStarted, Grant,
        HandoffCompleted, HandoffStarted, HandoffTrigger, InputItem, JobCompleted, JobStarted,
        Note, OpeningMessage, Outcome, PermissionRequested, PermissionResolved, ReviewerRef,
        RuleScope, SessionStarted, StandingRule, TextCompleted, TurnStarted, UsageRecorded,
        Variables, VariablesSource,
    };
    use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender, Tokens, Usage};
    use contract::{ActionId, Envelope, ErrorCode, GenerationId, JobId, RequestId, SessionId};

    use crate::handoff::Carry;
    use crate::resume::{Resumed, resumed, suspended};
    use crate::reviewer::render_reviewed;

    const MODEL: &str = "fake/model-1";

    /// A session log in a temporary directory, written through the log so
    /// every line has its `seq`.
    struct Session {
        root: fakes::TempDir,
        log: log::Log,
    }

    impl Session {
        fn new() -> Self {
            let root = fakes::TempDir::new("fiber-window");
            let log = log::Log::create(
                root.path(),
                SessionId("s_1".into()),
                fakes::clock::FakeClock::new(),
            )
            .unwrap();
            let session = Self { root, log };
            session.write(
                &Event::SessionStarted(SessionStarted {
                    workspace: "/w".into(),
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
            );
            session
        }

        fn dir(&self) -> PathBuf {
            self.root.path().join("s_1")
        }

        /// Appends `event` under `turn` and `action`; returns its `seq`.
        fn write(&self, event: &Event, turn: Option<&str>, action: Option<&str>) -> u64 {
            self.log
                .append(
                    event,
                    turn.map(|turn| contract::TurnId(turn.into())),
                    action.map(|action| ActionId(action.into())),
                )
                .unwrap()
                .seq
                .unwrap()
                .0
        }

        /// Appends a raw line past the writer.
        fn raw(&self, line: &str) {
            use std::io::Write as _;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(self.dir().join("events.jsonl"))
                .unwrap();
            writeln!(file, "{line}").unwrap();
        }

        fn lines(&self) -> Vec<Envelope> {
            log::read(&self.dir()).unwrap()
        }
    }

    fn turn(text: &str) -> Event {
        Event::TurnStarted(TurnStarted {
            input: vec![InputItem::Message {
                content: vec![ContentPart::Text { text: text.into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: None,
                },
                changed_by: None,
            }],
        })
    }

    fn reply() -> Event {
        Event::AssistantMessageStarted(Empty {})
    }

    fn text(text: &str) -> Event {
        Event::TextCompleted(TextCompleted {
            text: text.into(),
            provider_item: None,
        })
    }

    fn opening(session_log: &str) -> Event {
        Event::OpeningMessage(OpeningMessage {
            environment: Environment {
                date: "2023-11-14".into(),
                os: "test-os".into(),
                arch: "test-arch".into(),
                shell: "/bin/sh".into(),
                workspace: "/w".into(),
                git: None,
                session_log: session_log.into(),
            },
            instruction_files: Vec::new(),
            extension_sections: Vec::new(),
            skills: Vec::new(),
        })
    }

    fn handoff_started() -> Event {
        Event::HandoffStarted(HandoffStarted {
            trigger: HandoffTrigger::Auto,
        })
    }

    fn handoff_done(outcome: Outcome, note: &str) -> Event {
        Event::HandoffCompleted(HandoffCompleted {
            outcome,
            error: None,
            note: (outcome == Outcome::Completed).then(|| Note::Actions {
                note: vec![ActionId(note.into())],
            }),
            tokens_before: 1000,
            instructions: None,
        })
    }

    /// One handoff in `turn`: its window, the note under `note`, and its
    /// end with `outcome`.
    fn handoff(session: &Session, turn: &str, note: &str, outcome: Outcome) {
        session.write(&handoff_started(), Some(turn), None);
        session.write(&reply(), Some(turn), Some(note));
        session.write(&text(note), Some(turn), Some(note));
        session.write(&handoff_done(outcome, note), Some(turn), None);
    }

    fn job_started(id: &str) -> Event {
        Event::JobStarted(JobStarted {
            job_id: JobId(id.into()),
            tool: None,
            extension: None,
            description: format!("job {id}"),
            output_path: format!("artifacts/{id}"),
        })
    }

    fn job_completed(id: &str) -> Event {
        Event::JobCompleted(JobCompleted {
            job_id: JobId(id.into()),
            status: Outcome::Completed,
            error: None,
            process: None,
            output_tail: None,
        })
    }

    fn call() -> Event {
        Event::ToolCallRequested(contract::events::ToolCallRequested {
            name: "read".into(),
            arguments: serde_json::json!({"path": "a"}),
            provider_id: None,
            repair: None,
            ran_by: None,
            provider_item: None,
        })
    }

    fn result(artifact: Option<&str>) -> Event {
        Event::ToolCallCompleted(contract::events::ToolCallCompleted {
            status: contract::events::CallStatus::Completed,
            reason: None,
            error: None,
            process: None,
            content: vec![ContentPart::Text { text: "ok".into() }],
            details: None,
            artifact: artifact.map(str::to_owned),
            changes: None,
            control: None,
            changed_by: None,
            provider_item: None,
        })
    }

    fn request(id: &str) -> Event {
        Event::PermissionRequested(PermissionRequested {
            request_id: RequestId(id.into()),
            declared: DeclaredEffects {
                effects: vec![Effect::Executes],
                reversible: true,
                paths: None,
            },
            step: AskStep::StandingAsk {
                standing_rule: StandingRule {
                    scope: RuleScope::Project,
                    prefix: "run".into(),
                },
            },
        })
    }

    fn resolved(decided_by: DecidedBy, grant: Option<&str>, reviewer: bool) -> Event {
        Event::PermissionResolved(PermissionResolved {
            request_id: None,
            decision: if grant.is_some() {
                Decision::Allow
            } else {
                Decision::Deny
            },
            decided_by,
            reason: None,
            feedback: None,
            grant: grant.map(|prefix| Grant {
                tool: "shell".into(),
                prefix: prefix.into(),
            }),
            rule: None,
            reviewer: reviewer.then(|| ReviewerRef {
                model: MODEL.into(),
                stage: 1,
            }),
        })
    }

    fn usage(id: &str, model: &str) -> Event {
        Event::UsageRecorded(UsageRecorded {
            generation_id: GenerationId(id.into()),
            model: model.into(),
            tokens: Tokens {
                input: 10,
                cache_read: 0,
                cache_write: BTreeMap::new(),
                output: 3,
            },
            web_searches: None,
            cost: Some(1.0),
            subscription: None,
            extension: None,
            origin_session_id: None,
        })
    }

    fn fiber_started() -> Event {
        Event::FiberStarted(FiberStarted {
            version: "0.0.0".into(),
            resumed: true,
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

    fn nudged() -> Event {
        Event::ContextNudged(contract::events::ContextNudged {
            tokens: 900,
            trigger_at: 1000,
        })
    }

    /// The first context: an opening message, a job still running, one
    /// ended, one that ends in a later context; a turn with a call whose
    /// result has an artifact; a grant, a reviewer block and two refusals
    /// that are not blocks; usage; and a nudge. Every fact the pass folds
    /// is set before any later window.
    fn first_context(session: &Session) {
        session.write(&opening("old-log"), Some("t_1"), None);
        session.write(&job_started("j_run"), None, None);
        session.write(&job_started("j_done"), None, None);
        session.write(&job_completed("j_done"), None, None);
        session.write(&job_started("j_late"), None, None);
        session.write(&turn("one"), Some("t_1"), None);
        session.write(&reply(), Some("t_1"), Some("a_0"));
        session.write(&call(), Some("t_1"), Some("a_1"));
        session.write(
            &resolved(DecidedBy::Person, Some("cargo"), false),
            Some("t_1"),
            Some("a_1"),
        );
        session.write(
            &resolved(DecidedBy::Reviewer, None, true),
            Some("t_1"),
            Some("a_1"),
        );
        session.write(
            &resolved(DecidedBy::Reviewer, None, false),
            Some("t_1"),
            Some("a_1"),
        );
        session.write(
            &resolved(DecidedBy::StandingRule, None, true),
            Some("t_1"),
            Some("a_1"),
        );
        session.write(&result(Some("artifacts/a_1")), Some("t_1"), Some("a_1"));
        session.write(&usage("g_1", "fake/first"), Some("t_1"), Some("a_0"));
        session.write(&nudged(), Some("t_1"), None);
        session.write(&reply(), Some("t_1"), Some("a_2"));
        session.write(&text("answer"), Some("t_1"), Some("a_2"));
        session.write(&usage("g_2", "fake/second"), Some("t_1"), Some("a_2"));
    }

    /// What the whole log rebuilds, and what `resumed`'s window rebuilds,
    /// asserted equal; returns the pass.
    fn same_as_whole(session: &Session) -> Resumed {
        let folded = resumed(&session.dir()).unwrap();
        let all = session.lines();
        assert_eq!(folded.end, u64::try_from(all.len()).unwrap());
        let start = usize::try_from(folded.window).unwrap();
        let window = &all[start..];

        let whole_halt = suspended(&all).unwrap();
        let window_halt = suspended(window).unwrap();
        let halt = |halted: &Option<crate::resume::Suspended>| {
            halted.as_ref().map(|halted| {
                (
                    halted.turn.clone(),
                    halted.action.clone(),
                    halted.batch.clone(),
                    halted.request.clone(),
                )
            })
        };
        assert_eq!(halt(&window_halt), halt(&whole_halt));
        let open: HashSet<ActionId> = whole_halt
            .map(|halted| halted.batch.into_iter().map(|(id, _)| id).collect())
            .unwrap_or_default();

        let whole = super::super::rebuild_and_sent(&all, MODEL, &open, Carry::default()).unwrap();
        let rebuilt =
            super::super::rebuild_and_sent(window, MODEL, &open, folded.seed.clone()).unwrap();
        assert_eq!(rebuilt, whole);

        // The session-wide folds equal today's fold over the whole log.
        let mut reviewed = Vec::new();
        for line in &all {
            if let Some(event) = Event::from_envelope(line).unwrap() {
                render_reviewed(&mut reviewed, &event, line.action_id.as_ref());
            }
        }
        assert_eq!(folded.reviewed, reviewed);
        folded
    }

    #[test]
    fn with_no_handoff_the_window_is_the_whole_log() {
        let session = Session::new();
        first_context(&session);
        session.write(&job_completed("j_late"), None, None);

        let folded = same_as_whole(&session);

        assert_eq!(folded.window, 0);
        assert_eq!(folded.session, "s_1");
        assert_eq!(folded.workspace, "/w");
        assert_eq!(folded.model.as_deref(), Some("fake/second"));
    }

    #[test]
    fn the_window_starts_at_the_completed_handoffs_turn() {
        let session = Session::new();
        first_context(&session);
        let start = session.write(&turn("two"), Some("t_2"), None);
        // A call requested before the window and completed in it, and a
        // job started before it and ended in it.
        session.write(&job_completed("j_late"), None, None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);
        session.write(&turn("three"), Some("t_3"), None);
        session.write(&reply(), Some("t_3"), Some("a_3"));
        session.write(&text("after"), Some("t_3"), Some("a_3"));

        let folded = same_as_whole(&session);

        assert_eq!(folded.window, start);
        // The jobs running at the window start, and the path before it.
        assert_eq!(
            folded.seed.jobs,
            [
                ("j_run".to_owned(), "job j_run".to_owned()),
                ("j_late".to_owned(), "job j_late".to_owned()),
            ]
        );
        assert_eq!(folded.seed.session_log, "old-log");
        // The session-wide facts from before the window are kept.
        assert_eq!(
            folded.grants,
            [Grant {
                tool: "shell".into(),
                prefix: "cargo".into(),
            }]
        );
        assert_eq!(folded.session_blocks, 1);
        assert_eq!(folded.ledger.usage().tokens.input, 20);
        assert_eq!(folded.model.as_deref(), Some("fake/second"));
        // Only the job never ended is orphaned.
        let orphans: Vec<&str> = folded
            .orphans
            .iter()
            .map(|job| job.job_id.0.as_str())
            .collect();
        assert_eq!(orphans, ["j_run"]);
        assert!(folded.orphans.iter().all(|job| {
            job.status == Outcome::Failed
                && job.error.as_ref().map(|error| &error.code) == Some(&ErrorCode::Orphaned)
        }));
    }

    #[test]
    fn a_call_requested_before_the_window_and_completed_in_it_rebuilds_the_same() {
        let session = Session::new();
        first_context(&session);
        session.write(&reply(), Some("t_1"), Some("a_8"));
        session.write(&call(), Some("t_1"), Some("a_9"));
        let start = session.write(&turn("two"), Some("t_2"), None);
        session.write(&result(Some("artifacts/a_9")), Some("t_2"), Some("a_9"));
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);

        assert_eq!(same_as_whole(&session).window, start);
    }

    #[test]
    fn a_failed_handoff_after_a_completed_one_keeps_the_completed_window() {
        let session = Session::new();
        first_context(&session);
        let start = session.write(&turn("two"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);
        session.write(&turn("three"), Some("t_3"), None);
        handoff(&session, "t_3", "a_note2", Outcome::Failed);
        // A handoff that never completed changes nothing either.
        session.write(&turn("four"), Some("t_4"), None);
        session.write(&handoff_started(), Some("t_4"), None);
        session.write(&reply(), Some("t_4"), Some("a_note3"));

        assert_eq!(same_as_whole(&session).window, start);
    }

    #[test]
    fn only_a_failed_handoff_leaves_the_whole_log() {
        let session = Session::new();
        first_context(&session);
        session.write(&turn("two"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note", Outcome::Failed);

        assert_eq!(same_as_whole(&session).window, 0);
    }

    #[test]
    fn the_latest_of_two_completed_handoffs_starts_the_window() {
        let session = Session::new();
        first_context(&session);
        session.write(&turn("two"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("mid-log"), Some("t_2"), None);
        session.write(&job_started("j_mid"), None, None);
        let start = session.write(&turn("three"), Some("t_3"), None);
        handoff(&session, "t_3", "a_note2", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_3"), None);

        let folded = same_as_whole(&session);

        assert_eq!(folded.window, start);
        assert_eq!(folded.seed.session_log, "mid-log");
        assert_eq!(folded.seed.jobs.len(), 3);
    }

    #[test]
    fn a_second_handoff_in_the_same_turn_starts_at_that_turn() {
        let session = Session::new();
        first_context(&session);
        let start = session.write(&turn("two"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("mid-log"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note2", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);

        assert_eq!(same_as_whole(&session).window, start);
    }

    #[test]
    fn the_latest_turn_started_of_the_handoffs_turn_wins() {
        let session = Session::new();
        first_context(&session);
        session.write(&turn("two"), Some("t_2"), None);
        session.write(&turn("three"), Some("t_3"), None);
        let start = session.write(&turn("two again"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);

        assert_eq!(same_as_whole(&session).window, start);
    }

    #[test]
    fn an_earlier_turn_named_by_the_handoff_still_starts_the_window() {
        // Not the latest `turn_started`: the one whose turn the handoff names.
        let session = Session::new();
        first_context(&session);
        let start = session.write(&turn("two"), Some("t_2"), None);
        session.write(&turn("three"), Some("t_3"), None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);

        assert_eq!(same_as_whole(&session).window, start);
    }

    #[test]
    fn a_completed_handoff_with_no_turn_started_reads_the_whole_log() {
        let session = Session::new();
        first_context(&session);
        handoff(&session, "t_x", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_x"), None);

        let folded = same_as_whole(&session);

        assert_eq!(folded.window, 0);
        assert_eq!(folded.seed, Carry::default());
    }

    #[test]
    fn a_handoff_turn_finished_in_a_later_process_starts_at_its_turn() {
        let session = Session::new();
        first_context(&session);
        let start = session.write(&turn("two"), Some("t_2"), None);
        session.write(&reply(), Some("t_2"), Some("a_5"));
        session.write(&call(), Some("t_2"), Some("a_6"));
        session.write(&request("r_1"), Some("t_2"), Some("a_6"));
        session.write(&fiber_exited(Some("r_1")), None, None);
        session.write(&fiber_started(), None, None);
        session.write(&request("r_1"), Some("t_2"), Some("a_6"));
        session.write(
            &resolved(DecidedBy::Person, Some("make"), false),
            Some("t_2"),
            Some("a_6"),
        );
        session.write(&result(None), Some("t_2"), Some("a_6"));
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);

        let folded = same_as_whole(&session);

        assert_eq!(folded.window, start);
        assert_eq!(folded.grants.len(), 2);
    }

    #[test]
    fn a_turn_suspended_after_a_completed_handoff_suspends_the_same() {
        let session = Session::new();
        first_context(&session);
        session.write(&turn("two"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);
        session.write(&turn("three"), Some("t_3"), None);
        session.write(&reply(), Some("t_3"), Some("a_5"));
        session.write(&call(), Some("t_3"), Some("a_6"));
        session.write(&call(), Some("t_3"), Some("a_7"));
        session.write(&request("r_9"), Some("t_3"), Some("a_6"));
        session.write(&fiber_exited(Some("r_9")), None, None);

        same_as_whole(&session);
        assert!(suspended(&session.lines()).unwrap().is_some());
    }

    #[test]
    fn a_turn_suspended_in_its_own_handoff_turn_suspends_the_same() {
        let session = Session::new();
        first_context(&session);
        session.write(&turn("two"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);
        session.write(&opening("new-log"), Some("t_2"), None);
        session.write(&reply(), Some("t_2"), Some("a_5"));
        session.write(&call(), Some("t_2"), Some("a_6"));
        session.write(&request("r_9"), Some("t_2"), Some("a_6"));
        session.write(&fiber_exited(Some("r_9")), None, None);

        same_as_whole(&session);
        assert!(suspended(&session.lines()).unwrap().is_some());
    }

    #[test]
    fn a_log_ending_at_a_completed_handoff_keeps_the_old_path() {
        let session = Session::new();
        first_context(&session);
        session.write(&turn("two"), Some("t_2"), None);
        handoff(&session, "t_2", "a_note", Outcome::Completed);

        let folded = same_as_whole(&session);

        assert_eq!(folded.seed.session_log, "old-log");
    }

    #[test]
    fn a_job_a_rewind_handed_on_or_started_twice_is_orphaned_at_most_once() {
        let session = Session::new();
        session.write(&job_started("j_1"), None, None);
        session.write(&job_started("j_1"), None, None);
        session.write(&job_started("j_2"), None, None);
        session.write(
            &Event::Rewound(contract::events::Rewound {
                new_session_id: SessionId("s_2".into()),
                seq: contract::Seq(0),
                jobs: vec![JobId("j_2".into())],
            }),
            None,
            None,
        );

        let folded = resumed(&session.dir()).unwrap();

        let orphans: Vec<&str> = folded
            .orphans
            .iter()
            .map(|job| job.job_id.0.as_str())
            .collect();
        assert_eq!(orphans, ["j_1"]);
    }

    /// A line whose envelope reads and whose payload does not.
    fn unreadable(kind: &str, seq: u64) -> String {
        format!(
            r#"{{"kind":"{kind}","session_id":"s_1","ts":1,"schema_version":{},"seq":{seq},"payload":{{"text":7}}}}"#,
            contract::SCHEMA_VERSION
        )
    }

    #[test]
    fn an_unreadable_payload_the_pass_does_not_use_is_never_parsed() {
        let session = Session::new();
        first_context(&session);
        let bad = session.lines().len();
        session.raw(&unreadable("text_completed", u64::try_from(bad).unwrap()));
        drop(session.log);
        let log = log::Log::open(
            session.root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        let after = Session {
            root: session.root,
            log,
        };
        let start = after.write(&turn("two"), Some("t_2"), None);
        handoff(&after, "t_2", "a_note", Outcome::Completed);

        let folded = resumed(&after.dir()).unwrap();

        assert_eq!(folded.window, start);
        assert!(start > u64::try_from(bad).unwrap());
    }

    #[test]
    fn an_unreadable_payload_the_pass_uses_fails_log_corrupt() {
        let session = Session::new();
        session.raw(&unreadable("turn_started", 1));

        let error = match resumed(&session.dir()) {
            Ok(_) => panic!("an unreadable turn_started resumes"),
            Err(error) => error,
        };
        assert_eq!(error.code(), ErrorCode::LogCorrupt);
    }

    #[test]
    fn a_line_that_is_not_an_envelope_fails_log_corrupt() {
        let session = Session::new();
        first_context(&session);
        session.raw("not an envelope");

        let error = match resumed(&session.dir()) {
            Ok(_) => panic!("a bad envelope resumes"),
            Err(error) => error,
        };
        assert_eq!(error.code(), ErrorCode::LogCorrupt);
    }

    #[test]
    fn a_log_with_no_session_started_fails_log_corrupt() {
        let root = fakes::TempDir::new("fiber-window");
        let log = log::Log::create(
            root.path(),
            SessionId("s_1".into()),
            fakes::clock::FakeClock::new(),
        )
        .unwrap();
        log.append(&turn("one"), None, None).unwrap();

        let error = match resumed(&root.path().join("s_1")) {
            Ok(_) => panic!("a log with no session_started resumes"),
            Err(error) => error,
        };
        assert_eq!(error.code(), ErrorCode::LogCorrupt);
    }
}

#[test]
fn a_prompt_image_renders_on_the_user_message_and_the_carry() {
    let content = vec![
        contract::shapes::ContentPart::Text {
            text: "look".into(),
        },
        contract::shapes::ContentPart::Image {
            path: "artifacts/i_1.png".into(),
            mime_type: "image/png".into(),
            width: 1,
            height: 1,
        },
    ];
    let started = contract::events::Event::TurnStarted(contract::events::TurnStarted {
        input: vec![contract::events::InputItem::Message {
            content: content.clone(),
            sender: contract::shapes::Sender {
                origin: contract::shapes::Origin::Driver,
                command_id: None,
            },
            changed_by: None,
        }],
    });
    let mut conversation = Vec::new();
    let mut carry = crate::handoff::Carry::default();
    render(
        &mut conversation,
        &started,
        None,
        "fake/model-1",
        &mut BTreeMap::new(),
        &mut carry,
    );
    let expected = Input::User {
        text: "look".into(),
        images: vec![contract::provider::ImageRef {
            path: "artifacts/i_1.png".into(),
            mime_type: "image/png".into(),
            width: 1,
            height: 1,
        }],
    };
    assert_eq!(conversation, vec![expected.clone()]);
    assert_eq!(carry.input, vec![expected]);
}

#[test]
fn a_steered_image_renders_on_the_user_message_and_the_carry() {
    let content = vec![contract::shapes::ContentPart::Image {
        path: "artifacts/i_2.png".into(),
        mime_type: "image/png".into(),
        width: 2,
        height: 2,
    }];
    let steering = contract::events::Event::SteeringApplied(contract::events::SteeringApplied {
        content: content.clone(),
        sender: contract::shapes::Sender {
            origin: contract::shapes::Origin::Driver,
            command_id: None,
        },
        changed_by: None,
    });
    let mut conversation = Vec::new();
    let mut carry = crate::handoff::Carry::default();
    render(
        &mut conversation,
        &steering,
        None,
        "fake/model-1",
        &mut BTreeMap::new(),
        &mut carry,
    );
    let expected = Input::User {
        text: String::new(),
        images: vec![contract::provider::ImageRef {
            path: "artifacts/i_2.png".into(),
            mime_type: "image/png".into(),
            width: 2,
            height: 2,
        }],
    };
    assert_eq!(conversation, vec![expected.clone()]);
    assert_eq!(carry.input, vec![expected]);
}
