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
    CacheLifetime, DecidedBy, Decision, Event, HandoffCompleted, InputItem, ModelChanged,
    ModelSettings, Outcome, PermissionResolved, PreambleBuilt, PreambleReason, SessionStarted,
    SwitchSource, TurnStarted, UsageRecorded, Variables, VariablesSource,
};
use contract::shapes::{ContentPart, Origin, Point, Sender, Tokens};
use contract::{ActionId, Envelope, GenerationId, SCHEMA_VERSION, Seq, SessionId, TurnId};

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
    let at_switch = forked(&sessions.join(A), Seq(3)).unwrap();
    assert_eq!(at_switch.model.as_deref(), Some("fake/model-2"));
    assert_eq!(at_switch.thinking.as_deref(), Some("high"));
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
