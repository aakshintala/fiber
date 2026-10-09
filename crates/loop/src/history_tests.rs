//! Tests for the history fold over a session's chain: what folds from the
//! parent, what stays the own session's, and the window reader.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use contract::events::{
    CacheLifetime, CallStatus, DecidedBy, Decision, Event, HandoffCompleted, InputItem,
    ModelChanged, ModelSettings, Outcome, PermissionResolved, PreambleBuilt, PreambleReason,
    SessionStarted, SteeringApplied, SwitchSource, ToolCallCompleted, ToolCallRequested,
    TurnStarted, UsageRecorded, Variables, VariablesSource,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, Origin, Point, Sender, Tokens};
use contract::{ActionId, Envelope, GenerationId, SCHEMA_VERSION, Seq, SessionId, TurnId};
use serde_json::json;

use super::*;
use crate::resume::resumed;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";

fn envelope(
    session: &str,
    seq: u64,
    turn: Option<&str>,
    action: Option<&str>,
    event: &Event,
) -> Envelope {
    Envelope {
        kind: event.kind().to_owned(),
        session_id: SessionId(session.to_owned()),
        ts: 1_759_150_000_000 + seq,
        schema_version: SCHEMA_VERSION,
        turn_id: turn.map(|turn| TurnId(turn.to_owned())),
        action_id: action.map(|action| ActionId(action.to_owned())),
        seq: Some(Seq(seq)),
        payload: event.payload().unwrap(),
    }
}

fn started(workspace: &str, from: Option<(&str, u64)>) -> Event {
    Event::SessionStarted(SessionStarted {
        workspace: workspace.to_owned(),
        variables: Variables {
            path: "/usr/bin".to_owned(),
            names: Vec::new(),
            source: VariablesSource::Inherited,
        },
        parent: None,
        forked_from: from.map(|(session, seq)| Point {
            session_id: SessionId(session.to_owned()),
            seq: Seq(seq),
        }),
        rewind: None,
        worktree: None,
    })
}

fn usage(model: &str, generation: &str, input: u64) -> Event {
    Event::UsageRecorded(UsageRecorded {
        generation_id: GenerationId(generation.to_owned()),
        model: model.to_owned(),
        tokens: Tokens {
            input,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 0,
        },
        input_bytes: 0,
        input_media: None,
        web_searches: None,
        cost: None,
        subscription: None,
        extension: None,
        origin_session_id: None,
    })
}

fn changed(before: &str, after: &str, thinking: Option<&str>) -> Event {
    let settings = |model: &str, thinking: Option<&str>| ModelSettings {
        model: model.to_owned(),
        thinking: thinking.map(str::to_owned),
        cache_lifetime: CacheLifetime::OneHour,
        credential: None,
    };
    Event::ModelChanged(ModelChanged {
        before: settings(before, None),
        after: settings(after, thinking),
        source: SwitchSource::Driver,
    })
}

fn built(credential: Option<&str>) -> Event {
    Event::PreambleBuilt(PreambleBuilt {
        reason: PreambleReason::Start,
        model: "fake/model-1".to_owned(),
        context_window: 200_000,
        trigger_at: None,
        budget: None,
        thinking: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: CacheLifetime::OneHour,
        credential: credential.map(str::to_owned),
        system_prompt: String::new(),
        tools: Vec::new(),
        replaced: Vec::new(),
    })
}

fn turn() -> Event {
    Event::TurnStarted(TurnStarted { input: Vec::new() })
}

fn message_turn(text: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: None,
            },
            changed_by: None,
        }],
    })
}

fn handoff() -> Event {
    Event::HandoffCompleted(HandoffCompleted {
        outcome: Outcome::Completed,
        error: None,
        note: None,
        tokens_before: 0,
        instructions: None,
    })
}

fn denied() -> Event {
    Event::PermissionResolved(PermissionResolved {
        request_id: None,
        decision: Decision::Deny,
        decided_by: DecidedBy::Reviewer,
        reason: None,
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
    })
}

fn image_turn(path: &str) -> Event {
    Event::TurnStarted(TurnStarted {
        input: vec![InputItem::Message {
            content: vec![ContentPart::Image {
                path: path.to_owned(),
                mime_type: "image/png".to_owned(),
                width: 1,
                height: 1,
            }],
            sender: Sender {
                origin: Origin::Driver,
                command_id: None,
            },
            changed_by: None,
        }],
    })
}

fn write_log(sessions: &Path, session: &str, lines: &[Envelope]) {
    let dir = sessions.join(session);
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for line in lines {
        text.push_str(&serde_json::to_string(line).unwrap());
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
}

fn sessions(name: &str) -> (fakes::TempDir, PathBuf) {
    let home = fakes::TempDir::new(&format!("loop-history-{name}"));
    let sessions = home.path().join("sessions");
    (home, sessions)
}

/// The parent holds a handoff, a later usage record and a later model
/// switch; the child continues it at the handoff. The fold holds the
/// parent to the point and the child's own lines after it.
fn two_segment(sessions: &Path) {
    write_log(
        sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &built(Some("work"))),
            envelope(A, 2, None, None, &usage("fake/model-1", "fiber-1", 7)),
            envelope(A, 3, Some("t_1"), None, &turn()),
            envelope(A, 4, Some("t_1"), None, &handoff()),
            envelope(A, 5, Some("t_2"), None, &turn()),
            envelope(A, 6, None, None, &usage("fake/model-2", "fiber-2", 9)),
            envelope(
                A,
                7,
                None,
                None,
                &changed("fake/model-1", "fake/model-2", None),
            ),
        ],
    );
    write_log(
        sessions,
        B,
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 4)))),
            envelope(B, 1, Some("t_3"), None, &turn()),
        ],
    );
}

#[test]
fn a_two_segment_chain_folds_the_parent_to_the_point_and_the_child_after() {
    let (_home, sessions) = sessions("fold");
    two_segment(&sessions);
    let folded = resumed(&sessions.join(B)).unwrap();
    assert_eq!(folded.root, A);
    assert_eq!(folded.session, B);
    assert_eq!(folded.workspace, "/w");
    // The usage record and the model switch after the point change nothing.
    assert_eq!(folded.model.as_deref(), Some("fake/model-1"));
    assert_eq!(folded.credential.as_deref(), Some("work"));
    assert!(folded.thinking.is_none());
    // The handoff before the point puts the window in the parent segment.
    assert_eq!(folded.window, (0, 3));
}

#[test]
fn the_ledger_and_the_block_count_hold_only_the_own_log() {
    let (_home, sessions) = sessions("ledger");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &usage("fake/model-1", "fiber-1", 7)),
            envelope(A, 2, None, None, &denied()),
        ],
    );
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 1)))),
            envelope(B, 1, None, None, &usage("fake/model-1", "fiber-9", 5)),
            envelope(B, 2, None, None, &denied()),
        ],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    assert_eq!(folded.ledger.usage().tokens.input, 5);
    assert_eq!(folded.session_blocks, 1);
}

#[test]
fn a_handoff_after_the_point_leaves_the_window_at_the_chains_start() {
    let (_home, sessions) = sessions("handoff-after");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_1"), None, &turn()),
            envelope(A, 2, Some("t_1"), None, &handoff()),
        ],
    );
    write_log(
        &sessions,
        B,
        &[envelope(B, 0, None, None, &started("/w", Some((A, 1))))],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    assert_eq!(folded.window, (0, 0));
}

#[test]
fn forked_gives_the_parents_model_credential_and_thinking_at_the_point() {
    let (_home, sessions) = sessions("forked");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &built(Some("work"))),
            envelope(A, 2, None, None, &usage("fake/model-1", "fiber-1", 7)),
            envelope(
                A,
                3,
                None,
                None,
                &changed("fake/model-1", "fake/model-2", Some("high")),
            ),
        ],
    );
    let at_usage = forked(&sessions.join(A), Seq(2)).unwrap();
    assert_eq!(at_usage.root, A);
    assert_eq!(at_usage.session, A);
    assert_eq!(at_usage.model.as_deref(), Some("fake/model-1"));
    assert_eq!(at_usage.credential.as_deref(), Some("work"));
    assert!(at_usage.thinking.is_none());
    // The latest build at or before the point wins over the later
    // switch: the rewound session keeps the model it was built for.
    let at_switch = forked(&sessions.join(A), Seq(3)).unwrap();
    assert_eq!(at_switch.model.as_deref(), Some("fake/model-1"));
    assert_eq!(at_switch.credential.as_deref(), Some("work"));
    assert!(at_switch.thinking.is_none());
}

#[test]
fn forked_without_a_build_reads_the_model_from_the_calls_and_switches() {
    let (_home, sessions) = sessions("forked-bare");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &usage("fake/model-1", "fiber-1", 7)),
            envelope(
                A,
                2,
                None,
                None,
                &changed("fake/model-1", "fake/model-2", Some("high")),
            ),
        ],
    );
    let at_usage = forked(&sessions.join(A), Seq(1)).unwrap();
    assert_eq!(at_usage.model.as_deref(), Some("fake/model-1"));
    assert!(at_usage.thinking.is_none());
    let at_switch = forked(&sessions.join(A), Seq(2)).unwrap();
    assert_eq!(at_switch.model.as_deref(), Some("fake/model-2"));
    assert_eq!(at_switch.thinking.as_deref(), Some("high"));
}

#[test]
fn forked_leaves_a_garbage_line_past_the_point_unread() {
    // The point's own line is the last one the fold reads: a garbage
    // line right past it fails nothing.
    let (_home, sessions) = sessions("garbage");
    let dir = sessions.join(A);
    fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for line in [
        envelope(A, 0, None, None, &started("/w", None)),
        envelope(A, 1, None, None, &usage("fake/model-1", "fiber-1", 7)),
    ] {
        text.push_str(&serde_json::to_string(&line).unwrap());
        text.push('\n');
    }
    text.push_str("{\"kind\": \"usage_recorded\", broken\n");
    fs::write(dir.join("events.jsonl"), text).unwrap();
    let folded = forked(&dir, Seq(1)).unwrap();
    assert_eq!(folded.model.as_deref(), Some("fake/model-1"));
}

#[test]
fn the_window_reader_makes_a_parent_image_absolute_and_keeps_the_owns() {
    let (home, sessions) = sessions("images");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_1"), None, &image_turn("artifacts/a.png")),
        ],
    );
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 1)))),
            envelope(B, 1, Some("t_2"), None, &image_turn("artifacts/b.png")),
        ],
    );
    let _home = home;
    let folded = resumed(&sessions.join(B)).unwrap();
    let log = Log::open(
        &sessions,
        SessionId(B.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = read_window(&log, folded.window, folded.end).unwrap();
    let mut paths = Vec::new();
    for line in lines.iter().filter(|line| line.kind == "turn_started") {
        let Some(input) = line.payload.get("input").and_then(Value::as_array) else {
            continue;
        };
        for item in input {
            let Some(content) = item.get("content").and_then(Value::as_array) else {
                continue;
            };
            for part in content {
                if let Some(path) = part.get("path").and_then(Value::as_str) {
                    paths.push(path.to_owned());
                }
            }
        }
    }
    assert_eq!(
        paths,
        [
            sessions
                .join(A)
                .join("artifacts/a.png")
                .display()
                .to_string(),
            "artifacts/b.png".to_owned(),
        ]
    );
}

#[test]
fn the_window_reader_starts_mid_chain_at_a_parents_handoff() {
    // A completed handoff in the middle segment puts the window there:
    // the reader holds nothing before it, whatever segment it is on.
    const C: &str = "s_cccccccccccccccc";
    let (_home, sessions) = sessions("mid-chain");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_a1"), None, &message_turn("a-one")),
        ],
    );
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 1)))),
            envelope(B, 1, Some("t_b2"), None, &message_turn("b-two")),
            envelope(B, 2, Some("t_b2"), None, &handoff()),
            envelope(B, 3, Some("t_b3"), None, &message_turn("b-three")),
        ],
    );
    write_log(
        &sessions,
        C,
        &[envelope(C, 0, None, None, &started("/w", Some((B, 3))))],
    );
    let folded = resumed(&sessions.join(C)).unwrap();
    assert_eq!(folded.window, (1, 1));
    let log = Log::open(
        &sessions,
        SessionId(C.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = read_window(&log, folded.window, folded.end).unwrap();
    let held: Vec<(&str, u64, &str)> = lines
        .iter()
        .map(|line| {
            (
                line.session_id.0.as_str(),
                line.seq.unwrap().0,
                line.kind.as_str(),
            )
        })
        .collect();
    assert_eq!(
        held,
        [
            (B, 1, "turn_started"),
            (B, 2, "handoff_completed"),
            (B, 3, "turn_started"),
            (C, 0, "session_started"),
        ]
    );
    assert_eq!(
        crate::conversation::rebuild(&lines, "fake/model-1").unwrap(),
        [
            Input::User {
                text: "b-two".to_owned(),
                images: Vec::new(),
            },
            Input::User {
                text: String::new(),
                images: Vec::new(),
            },
            Input::User {
                text: "b-three".to_owned(),
                images: Vec::new(),
            },
        ]
    );
}

fn image_part(path: &str) -> ContentPart {
    ContentPart::Image {
        path: path.to_owned(),
        mime_type: "image/png".to_owned(),
        width: 1,
        height: 1,
    }
}

#[test]
fn the_window_reader_makes_a_parents_steering_and_tool_result_images_absolute() {
    // A steering message and a tool result carry image parts too: both are
    // made absolute against the parent that wrote them.
    let (_home, sessions) = sessions("steer-result-images");
    let steering = Event::SteeringApplied(SteeringApplied {
        content: vec![image_part("artifacts/s.png")],
        sender: Sender {
            origin: Origin::Driver,
            command_id: None,
        },
        changed_by: None,
    });
    let result = Event::ToolCallCompleted(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![image_part("artifacts/t.png")],
        details: None,
        artifact: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
    });
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_1"), None, &steering),
            envelope(A, 2, Some("t_1"), Some("a_1"), &result),
        ],
    );
    write_log(
        &sessions,
        B,
        &[envelope(B, 0, None, None, &started("/w", Some((A, 2))))],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    let log = Log::open(
        &sessions,
        SessionId(B.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = read_window(&log, folded.window, folded.end).unwrap();
    let mut paths = Vec::new();
    for line in &lines {
        if !matches!(
            line.kind.as_str(),
            "steering_applied" | "tool_call_completed"
        ) {
            continue;
        }
        let content = line.payload["content"].as_array().unwrap();
        for part in content {
            paths.push(part["path"].as_str().unwrap().to_owned());
        }
    }
    let absolute = |name: &str| sessions.join(A).join(name).display().to_string();
    assert_eq!(
        paths,
        [absolute("artifacts/s.png"), absolute("artifacts/t.png")]
    );
}

#[test]
fn the_window_reader_leaves_image_shaped_tool_arguments_alone() {
    // Only the content parts a protocol reads as images are rewritten:
    // a tool call whose arguments happen to be shaped like an image
    // replays byte for byte.
    let shaped = json!({"type": "image", "path": "diagram.png", "mime_type": "image/png"});
    let call = Event::ToolCallRequested(ToolCallRequested {
        name: "draw".to_owned(),
        arguments: shaped.clone(),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    });
    let (_home, sessions) = sessions("args");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_1"), None, &image_turn("artifacts/a.png")),
            envelope(A, 2, Some("t_1"), Some("a_1"), &call),
        ],
    );
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 2)))),
            envelope(B, 1, Some("t_2"), None, &turn()),
        ],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    let log = Log::open(
        &sessions,
        SessionId(B.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = read_window(&log, folded.window, folded.end).unwrap();
    let requested = lines
        .iter()
        .find(|line| line.kind == "tool_call_requested")
        .unwrap();
    assert_eq!(requested.payload["arguments"], shaped);
    let conversation = crate::conversation::rebuild(&lines, "fake/model-1").unwrap();
    let replayed = conversation
        .iter()
        .find_map(|input| match input {
            Input::ToolCall { call, .. } => Some(call),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolResult { .. } => None,
        })
        .unwrap();
    assert_eq!(replayed.arguments, shaped);
    // A real image part in the same window is still made absolute.
    let image = lines
        .iter()
        .find(|line| line.kind == "turn_started" && line.seq.unwrap().0 == 1)
        .unwrap();
    assert_eq!(
        image.payload["input"][0]["content"][0]["path"],
        json!(
            sessions
                .join(A)
                .join("artifacts/a.png")
                .display()
                .to_string()
        )
    );
}
