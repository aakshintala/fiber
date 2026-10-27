//! Tests for the history fold over a session's chain: what folds from the
//! parent, what stays the own session's, and the window reader.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::conversation::{rebuild_and_sent, summary_conversation};
use contract::events::{
    Anchor, CacheLifetime, CallStatus, Control, DecidedBy, Decision, Environment, Event, Grant,
    HandoffCompleted, HandoffStarted, HandoffTrigger, InputItem, ModelChanged, ModelSettings,
    Moved, OfferDecision, OfferedItem, OfferedKind, OpeningMessage, Outcome, Parent,
    PermissionResolved, PreambleBuilt, PreambleReason, RanBy, RepositoryCodeOffered,
    RepositoryCodeResolved, Rewound, SessionStarted, SkillLoad, SkillSent, SkillsResent,
    SteeringApplied, SwitchSource, ToolCallCompleted, ToolCallRequested, TurnStarted,
    UsageRecorded, Variables, VariablesSource,
};
use contract::provider::Input;
use contract::shapes::{ContentPart, Origin, Point, Sender, Tokens};
use contract::{ActionId, Envelope, GenerationId, SCHEMA_VERSION, Seq, SessionId, TurnId};
use serde_json::json;

use super::*;
use crate::resume::resumed;

const A: &str = "s_aaaaaaaaaaaaaaaa";
const B: &str = "s_bbbbbbbbbbbbbbbb";
const C: &str = "s_cccccccccccccccc";

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
            project: None,
        }),
        rewind: None,
        worktree: None,
        supervised: false,
    })
}

fn supervised_started(workspace: &str) -> Event {
    Event::SessionStarted(SessionStarted {
        workspace: workspace.to_owned(),
        variables: Variables {
            path: "/usr/bin".to_owned(),
            names: Vec::new(),
            source: VariablesSource::Inherited,
        },
        parent: None,
        forked_from: None,
        rewind: None,
        worktree: None,
        supervised: true,
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
        reviewer: None,
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
        call: None,
        request_id: None,
        decision: Decision::Deny,
        decided_by: DecidedBy::Reviewer,
        reason: None,
        feedback: None,
        grant: None,
        rule: None,
        reviewer: None,
        hosts: None,
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
fn the_fold_restores_supervised_from_the_first_line_and_defaults_it() {
    let (_home, sessions) = sessions("supervised");
    write_log(
        &sessions,
        A,
        &[envelope(A, 0, None, None, &supervised_started("/w"))],
    );
    let folded = resumed(&sessions.join(A)).unwrap();
    assert!(folded.supervised);
    write_log(
        &sessions,
        B,
        &[envelope(B, 0, None, None, &started("/w", None))],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    assert!(!folded.supervised);
}

fn delegate_started(job: &str, session: &str) -> Event {
    Event::DelegateStarted(contract::events::DelegateStarted {
        job_id: contract::JobId(job.to_owned()),
        delegate_session_id: SessionId(session.to_owned()),
        harness: "fiber".to_owned(),
        model: "fake/model-1".to_owned(),
        workspace: "/w".to_owned(),
        worktree: None,
        forked_from: None,
    })
}

#[test]
fn the_delegate_session_map_holds_only_the_own_segments_starts() {
    let (_home, sessions) = sessions("delegate-sessions");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &delegate_started("j_parent", "s_parent")),
        ],
    );
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 1)))),
            envelope(B, 1, None, None, &delegate_started("j_own", "s_own")),
        ],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    assert_eq!(folded.delegate_sessions.len(), 1);
    assert_eq!(
        folded
            .delegate_sessions
            .get(&contract::JobId("j_own".to_owned())),
        Some(&SessionId("s_own".to_owned()))
    );
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

fn requested_permission(id: &str) -> Event {
    Event::PermissionRequested(contract::events::PermissionRequested {
        always_reviewed: false,
        request_id: contract::RequestId(id.to_owned()),
        declared: contract::shapes::DeclaredEffects {
            effects: vec![contract::shapes::Effect::Executes],
            reversible: false,
            paths: None,
        },
        step: contract::events::AskStep::StandingAsk {
            standing_rule: contract::events::StandingRule {
                scope: contract::events::RuleScope::Project,
                prefix: "npm".to_owned(),
            },
        },
    })
}

fn asked(id: &str, extension: Option<&str>) -> Event {
    Event::InteractionRequested(contract::events::InteractionRequested {
        request_id: contract::RequestId(id.to_owned()),
        interaction: contract::events::Interaction::Confirm {
            prompt: "go?".to_owned(),
        },
        action_ids: None,
        extension: extension.map(str::to_owned),
        resumes: false,
    })
}

fn answered(id: &str) -> Event {
    Event::InteractionResolved(contract::events::InteractionResolved {
        request_id: contract::RequestId(id.to_owned()),
        by: contract::events::ResolvedBy::Person,
        answer: contract::events::Answer::Confirmed { confirmed: true },
    })
}

fn offered(id: &str) -> Event {
    Event::RepositoryCodeOffered(contract::events::RepositoryCodeOffered {
        request_id: contract::RequestId(id.to_owned()),
        items: Vec::new(),
    })
}

fn process_started() -> Event {
    Event::FiberStarted(contract::events::FiberStarted {
        version: "1.2.3".to_owned(),
        resumed: false,
    })
}

/// The lines `fiber_exited` writes over the seed `folded` holds, under
/// `signal`: the ask declines it appends, then the exit itself.
fn exit_over(folded: &Resumed, signal: Option<i32>) -> Vec<Envelope> {
    let home = fakes::TempDir::new("loop-history-exit");
    let log = Log::create(
        home.path(),
        SessionId("s_exit".to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    crate::fiber_exited(&log, &folded.exit_seed(), Ok(()), false, signal).unwrap();
    log::read(&home.path().join("s_exit")).unwrap()
}

/// Folds a one-segment log holding `events`, one per line from seq 1 after
/// `session_started`, and returns what its exit reports.
fn seeded(name: &str, events: &[Event], signal: Option<i32>) -> Vec<Envelope> {
    let (_home, sessions) = sessions(name);
    let mut lines = vec![envelope(A, 0, None, None, &started("/w", None))];
    for (index, event) in events.iter().enumerate() {
        lines.push(envelope(A, index as u64 + 1, None, None, event));
    }
    write_log(&sessions, A, &lines);
    exit_over(&resumed(&sessions.join(A)).unwrap(), signal)
}

fn suspended_on(exit: &[Envelope]) -> Option<String> {
    exit.last()
        .unwrap()
        .payload
        .get("suspended_on")
        .and_then(|id| id.as_str())
        .map(str::to_owned)
}

#[test]
fn the_seed_names_an_unresolved_approval_under_a_signal_only() {
    let log = [
        process_started(),
        requested_permission("r_old"),
        process_started(),
    ];
    assert_eq!(
        suspended_on(&seeded("seed-signal", &log, Some(143))),
        Some("r_old".to_owned())
    );
    assert_eq!(suspended_on(&seeded("seed-quiet", &log, None)), None);
}

#[test]
fn a_resolved_approval_is_not_seeded() {
    let resolved = Event::PermissionResolved(
        serde_json::from_value(json!({
            "request_id": "r_old", "decision": "allow", "decided_by": "person",
        }))
        .unwrap(),
    );
    let log = [requested_permission("r_old"), resolved, process_started()];
    assert_eq!(
        suspended_on(&seeded("seed-resolved", &log, Some(143))),
        None
    );
}

#[test]
fn an_unresolved_extension_ask_is_seeded_and_a_resolved_one_is_not() {
    let open = [asked("r_ext", Some("ext")), process_started()];
    let exit = seeded("seed-ask", &open, None);
    let kinds: Vec<&str> = exit.iter().map(|line| line.kind.as_str()).collect();
    assert_eq!(kinds, ["interaction_resolved", "fiber_exited"]);
    assert_eq!(exit[0].payload["request_id"], "r_ext");
    let done = [
        asked("r_ext", Some("ext")),
        answered("r_ext"),
        process_started(),
    ];
    assert_eq!(seeded("seed-ask-done", &done, None).len(), 1);
}

#[test]
fn a_question_without_an_extension_is_a_request_and_no_ask() {
    let log = [asked("r_q", None), process_started()];
    let exit = seeded("seed-question", &log, Some(143));
    assert_eq!(exit.len(), 1);
    assert_eq!(suspended_on(&exit), Some("r_q".to_owned()));
}

#[test]
fn an_offer_the_preamble_followed_is_not_seeded() {
    let log = [offered("r_o"), built(None), process_started()];
    assert_eq!(
        suspended_on(&seeded("seed-offer-built", &log, Some(143))),
        None
    );
    let log = [offered("r_o"), process_started()];
    assert_eq!(
        suspended_on(&seeded("seed-offer-open", &log, Some(143))),
        Some("r_o".to_owned())
    );
}

#[test]
fn a_parent_segments_open_request_is_not_seeded() {
    let (_home, sessions) = sessions("seed-parent");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &requested_permission("r_parent")),
            envelope(A, 2, None, None, &asked("r_ext", Some("ext"))),
        ],
    );
    write_log(
        &sessions,
        B,
        &[envelope(B, 0, None, None, &started("/w", Some((A, 2))))],
    );
    let exit = exit_over(&resumed(&sessions.join(B)).unwrap(), Some(143));
    assert_eq!(exit.len(), 1);
    assert_eq!(suspended_on(&exit), None);
}

#[test]
fn the_seed_reads_requests_without_parsing_their_payloads() {
    let (_home, sessions) = sessions("seed-raw");
    let mut garbled = envelope(A, 1, None, None, &requested_permission("r_old"));
    garbled.payload.remove("declared");
    write_log(
        &sessions,
        A,
        &[envelope(A, 0, None, None, &started("/w", None)), garbled],
    );
    let exit = exit_over(&resumed(&sessions.join(A)).unwrap(), Some(143));
    assert_eq!(suspended_on(&exit), Some("r_old".to_owned()));
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
fn forked_without_a_build_takes_the_model_from_switches_not_calls() {
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
    assert_eq!(at_usage.model, None);
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
        envelope(A, 1, None, None, &built(None)),
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
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
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
fn the_window_reader_makes_a_parent_pdf_and_its_pages_absolute() {
    use contract::shapes::{ImagePart, PdfPart};
    let pdf = ContentPart::Pdf(
        PdfPart::new(
            "artifacts/p_3f2a9c0d1e4b5a67.pdf".into(),
            2,
            Some(vec![
                ImagePart {
                    path: "artifacts/i_0a1b2c3d4e5f6071.png".into(),
                    mime_type: "image/png".into(),
                    width: 1,
                    height: 1,
                },
                ImagePart {
                    path: "/already/absolute.png".into(),
                    mime_type: "image/png".into(),
                    width: 1,
                    height: 1,
                },
            ]),
        )
        .unwrap(),
    );
    let result = Event::ToolCallCompleted(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![pdf],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: None,
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
    });
    let (_home, sessions) = sessions("pdf-rewrite");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_1"), Some("a_1"), &result),
        ],
    );
    write_log(
        &sessions,
        B,
        &[envelope(B, 0, None, None, &started("/w", Some((A, 1))))],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    let log = Log::open(
        &sessions,
        SessionId(B.to_owned()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let lines = read_window(&log, folded.window, folded.end).unwrap();
    let completed = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap();
    let content = completed.payload["content"].as_array().unwrap();
    assert_eq!(
        content[0]["path"],
        json!(
            sessions
                .join(A)
                .join("artifacts/p_3f2a9c0d1e4b5a67.pdf")
                .display()
                .to_string()
        )
    );
    let pages = content[0]["pages"].as_array().unwrap();
    assert_eq!(
        pages[0]["path"],
        json!(
            sessions
                .join(A)
                .join("artifacts/i_0a1b2c3d4e5f6071.png")
                .display()
                .to_string()
        )
    );
    // An already-absolute page path is kept.
    assert_eq!(pages[1]["path"], json!("/already/absolute.png"));
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
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
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

fn named(name: Option<&str>, by: contract::events::NamedBy) -> Event {
    Event::SessionNamed(contract::events::SessionNamed {
        name: name.map(str::to_owned),
        by,
    })
}

#[test]
fn a_person_name_in_the_parent_does_not_pin_the_child() {
    use contract::events::NamedBy;
    let (_home, sessions) = sessions("naming-parent");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &named(Some("Mine"), NamedBy::Person)),
            envelope(A, 2, Some("t_1"), None, &turn()),
            envelope(A, 3, Some("t_1"), None, &handoff()),
        ],
    );
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 3)))),
            envelope(B, 1, Some("t_2"), None, &turn()),
        ],
    );
    let folded = fold(&log::history(&sessions.join(B)).unwrap()).unwrap();
    let control = contract::events::Control {
        handoff: None,
        questions: None,
        skill: None,
        name: Some("x".into()),
    };
    assert!(matches!(
        folded.naming.judge(CallStatus::Completed, Some(&control)),
        crate::naming::Named::Write(_)
    ));
}

#[test]
fn a_person_name_in_the_own_log_pins_the_session() {
    use contract::events::NamedBy;
    let (_home, sessions) = sessions("naming-own");
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
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 2)))),
            envelope(B, 1, None, None, &named(Some("Mine"), NamedBy::Person)),
            envelope(B, 2, Some("t_2"), None, &turn()),
        ],
    );
    let folded = fold(&log::history(&sessions.join(B)).unwrap()).unwrap();
    let control = contract::events::Control {
        handoff: None,
        questions: None,
        skill: None,
        name: Some("x".into()),
    };
    assert!(matches!(
        folded.naming.judge(CallStatus::Completed, Some(&control)),
        crate::naming::Named::Pinned(_)
    ));
}

#[test]
fn an_unreadable_name_line_fails_the_resume() {
    const C: &str = "s_cccccccccccccccc";
    let (_home, sessions) = sessions("naming-unreadable");
    let mut bad_payload = serde_json::Map::new();
    bad_payload.insert("by".to_owned(), json!("person"));
    let bad = Envelope {
        kind: "session_named".to_owned(),
        session_id: SessionId(C.to_owned()),
        ts: 1_759_150_000_001,
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: Some(Seq(1)),
        payload: bad_payload,
    };
    write_log(
        &sessions,
        C,
        &[
            envelope(C, 0, None, None, &started("/w", None)),
            bad,
            envelope(C, 2, Some("t_1"), None, &turn()),
            envelope(C, 3, Some("t_1"), None, &handoff()),
        ],
    );
    let Err(crate::Error::Unreadable(_)) = fold(&log::history(&sessions.join(C)).unwrap()) else {
        panic!("the unreadable name line read");
    };
}

#[test]
fn known_hosts_history_and_fork_fold_each_parent_only_to_its_point() {
    use contract::events::{InstructionFile, InstructionReason, InstructionSent};
    let (_home, sessions) = sessions("known-hosts");
    let named = |url: &str| {
        Event::TurnStarted(TurnStarted {
            input: vec![InputItem::Message {
                content: vec![ContentPart::Text { text: url.into() }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: None,
                },
                changed_by: None,
            }],
        })
    };
    let mut allowed =
        crate::completion::resolved(None, Decision::Allow, DecidedBy::Reviewer, None, None);
    allowed.hosts = Some(vec!["https://allowed.test/".into()]);
    let mut surfaced = crate::completion::completed("https://page-only.test/".into(), None);
    surfaced.hosts_surfaced = vec!["https://surfaced.test/".into()];
    let parent = [
        started("/w", None),
        named("https://person.test/"),
        Event::InstructionFile(InstructionFile {
            path: "AGENTS.md".into(),
            reason: InstructionReason::OwnEdit,
            extension: None,
            content: Some("https://instructions.test/".into()),
            sent: InstructionSent::Full,
        }),
        Event::ToolCallCompleted(surfaced),
        Event::PermissionResolved(allowed),
        named("https://after.test/"),
    ];
    let lines: Vec<_> = parent
        .iter()
        .enumerate()
        .map(|(seq, event)| envelope(A, u64::try_from(seq).unwrap(), None, None, event))
        .collect();
    write_log(&sessions, A, &lines);
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started("/w", Some((A, 4)))),
            envelope(B, 1, None, None, &named("https://child.test/")),
        ],
    );
    let child = resumed(&sessions.join(B)).unwrap();
    let fork = forked(&sessions.join(A), Seq(4)).unwrap();
    for hosts in [&child.known_hosts, &fork.known_hosts] {
        assert!(hosts.all_known(&[
            "https://person.test/".into(),
            "https://surfaced.test/".into(),
            "https://allowed.test/".into()
        ]));
        assert!(!hosts.all_known(&["https://instructions.test/".into()]));
        assert!(!hosts.all_known(&["https://after.test/".into()]));
        assert!(!hosts.all_known(&["https://page-only.test/".into()]));
    }
    assert!(child.known_hosts.all_known(&["https://child.test/".into()]));
    assert!(!fork.known_hosts.all_known(&["https://child.test/".into()]));
    assert!(
        !forked(&sessions.join(A), Seq(3))
            .unwrap()
            .known_hosts
            .all_known(&["https://allowed.test/".into()])
    );
    assert!(
        forked(&sessions.join(A), Seq(5))
            .unwrap()
            .known_hosts
            .all_known(&["https://after.test/".into()])
    );
}

#[test]
fn known_hosts_resume_and_fork_keep_changes_without_surfaced_hosts() {
    use contract::events::{FileChange, InstructionFile, InstructionReason, InstructionSent};

    let (_home, sessions) = sessions("known-hosts-changes");
    let instruction = Event::InstructionFile(InstructionFile {
        path: "AGENTS.md".into(),
        reason: InstructionReason::Changed,
        extension: None,
        content: Some("https://instructions.test/".into()),
        sent: InstructionSent::Full,
    });
    let mut changed = crate::completion::completed(String::new(), None);
    changed.changes = Some(vec![FileChange {
        path: "AGENTS.md".into(),
        added: 1,
        removed: 0,
    }]);
    let parent = [
        started("/w", None),
        instruction.clone(),
        Event::ToolCallCompleted(changed),
        instruction,
    ];
    let lines: Vec<_> = parent
        .iter()
        .enumerate()
        .map(|(seq, event)| envelope(A, u64::try_from(seq).unwrap(), None, None, event))
        .collect();
    assert!(!lines[2].payload.contains_key("hosts_surfaced"));
    assert!(lines[2].payload.contains_key("changes"));
    write_log(&sessions, A, &lines);

    let before = forked(&sessions.join(A), Seq(1)).unwrap();
    assert!(
        before
            .known_hosts
            .all_known(&["https://instructions.test/".into()])
    );
    let after = forked(&sessions.join(A), Seq(2)).unwrap();
    assert!(
        !after
            .known_hosts
            .all_known(&["https://instructions.test/".into()])
    );
    let resumed = resumed(&sessions.join(A)).unwrap();
    let fork = forked(&sessions.join(A), Seq(3)).unwrap();
    assert!(
        !resumed
            .known_hosts
            .all_known(&["https://instructions.test/".into()])
    );
    assert_eq!(resumed.known_hosts, fork.known_hosts);
}

#[test]
fn known_hosts_history_skips_results_without_a_surfaced_list_before_deserializing() {
    let (_home, sessions) = sessions("known-hosts-unparsed");
    let mut result = envelope(
        A,
        1,
        None,
        None,
        &Event::ToolCallCompleted(crate::completion::completed(
            "https://page.test/".into(),
            None,
        )),
    );
    result
        .payload
        .insert("content".into(), json!({"not":"content parts"}));
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            result.clone(),
        ],
    );
    let folded = resumed(&sessions.join(A)).unwrap();
    assert!(!folded.known_hosts.all_known(&["https://page.test/".into()]));
    result.payload.insert("hosts_surfaced".into(), json!([]));
    write_log(
        &sessions,
        A,
        &[envelope(A, 0, None, None, &started("/w", None)), result],
    );
    assert!(resumed(&sessions.join(A)).is_err());
}

fn requested() -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: "read".into(),
        arguments: json!({}),
        provider_id: None,
        repair: None,
        ran_by: None,
        provider_item: None,
    })
}

fn inner_requested() -> Event {
    Event::ToolCallRequested(ToolCallRequested {
        name: "inner".into(),
        arguments: json!({}),
        provider_id: None,
        repair: None,
        ran_by: Some(RanBy {
            extension: "ext".into(),
            anchor: Anchor::OuterCall(ActionId("a_0".into())),
        }),
        provider_item: None,
    })
}

fn skilled_completed(status: CallStatus, name: &str, path: &str) -> Event {
    Event::ToolCallCompleted(ToolCallCompleted {
        status,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text { text: "ok".into() }],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: Some(Control {
            handoff: None,
            questions: None,
            skill: Some(SkillLoad {
                name: name.into(),
                path: path.into(),
            }),
            name: None,
        }),
        changed_by: None,
        provider_item: None,
        hosts_surfaced: Vec::new(),
        loaded: None,
    })
}

fn begun() -> Event {
    Event::HandoffStarted(HandoffStarted {
        trigger: HandoffTrigger::Person,
    })
}

fn resent(skills: &[(&str, &str, &str)]) -> Event {
    Event::SkillsResent(SkillsResent {
        skills: skills
            .iter()
            .map(|(name, path, content)| SkillSent {
                name: (*name).into(),
                path: (*path).into(),
                content: (*content).into(),
            })
            .collect(),
    })
}

fn opening() -> Event {
    Event::OpeningMessage(OpeningMessage {
        environment: Environment {
            date: "2023-11-14".into(),
            os: "test-os".into(),
            arch: "test-arch".into(),
            shell: "/bin/sh".into(),
            workspace: "/w".into(),
            git: None,
            session_log: "/w/s_test/events.jsonl".into(),
        },
        instruction_files: Vec::new(),
        extension_sections: Vec::new(),
        skills: Vec::new(),
    })
}

fn alpha() -> SkillLoad {
    SkillLoad {
        name: "alpha".into(),
        path: "/w/alpha/SKILL.md".into(),
    }
}

fn seed_log() -> Vec<(u64, Option<&'static str>, Option<&'static str>, Event)> {
    vec![
        (0, None, None, started("/w", None)),
        (1, Some("t_1"), None, message_turn("ask")),
        (2, Some("t_1"), Some("a_1"), requested()),
        (
            3,
            Some("t_1"),
            Some("a_1"),
            skilled_completed(CallStatus::Completed, "alpha", "/w/alpha/SKILL.md"),
        ),
        (4, Some("t_2"), None, message_turn("ask")),
        (5, Some("t_2"), None, begun()),
        (6, Some("t_2"), None, handoff()),
    ]
}

fn write_seed_log(sessions: &Path) {
    write_log(
        sessions,
        A,
        &seed_log()
            .iter()
            .map(|(seq, turn, action, event)| envelope(A, *seq, *turn, *action, event))
            .collect::<Vec<_>>(),
    );
}

#[test]
fn the_seed_holds_the_loads_before_the_handoff_window() {
    let (_home, sessions) = sessions("seed-loads");
    write_seed_log(&sessions);
    let folded = resumed(&sessions.join(A)).unwrap();
    // The load predates the handoff turn's window, yet the seed holds it,
    // so the rebuilt restart arms the resend.
    assert_eq!(folded.seed.skills, [alpha()]);
}

#[test]
fn failed_inner_and_pre_handoff_loads_never_seed() {
    let (_home, sessions) = sessions("seed-loads-filtered");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_1"), None, &turn()),
            envelope(A, 2, Some("t_1"), Some("a_1"), &requested()),
            // A successful load before an earlier completed handoff.
            envelope(
                A,
                3,
                Some("t_1"),
                Some("a_1"),
                &skilled_completed(CallStatus::Completed, "old", "/w/old/SKILL.md"),
            ),
            envelope(A, 4, Some("t_1"), Some("a_2"), &requested()),
            // A failed call's skill never counts.
            envelope(
                A,
                5,
                Some("t_1"),
                Some("a_2"),
                &skilled_completed(CallStatus::Failed, "nope", "/w/nope/SKILL.md"),
            ),
            envelope(A, 6, Some("t_1"), Some("a_3"), &inner_requested()),
            // An inner call's skill never counts.
            envelope(
                A,
                7,
                Some("t_1"),
                Some("a_3"),
                &skilled_completed(CallStatus::Completed, "inner", "/w/inner/SKILL.md"),
            ),
            envelope(A, 8, Some("t_2"), None, &turn()),
            envelope(A, 9, Some("t_2"), None, &handoff()),
            envelope(A, 10, Some("t_3"), None, &turn()),
            envelope(A, 11, Some("t_3"), Some("a_4"), &requested()),
            envelope(
                A,
                12,
                Some("t_3"),
                Some("a_4"),
                &skilled_completed(CallStatus::Completed, "beta", "/w/beta/SKILL.md"),
            ),
            envelope(A, 13, Some("t_4"), None, &turn()),
            envelope(A, 14, Some("t_4"), None, &handoff()),
        ],
    );
    let folded = resumed(&sessions.join(A)).unwrap();
    assert_eq!(
        folded.seed.skills,
        [SkillLoad {
            name: "beta".into(),
            path: "/w/beta/SKILL.md".into(),
        }]
    );
}

/// A log whose one skill load of `status` predates the handoff, as
/// [`seed_log`] holds a completed one: only a completed Fiber-run load
/// arms the resend.
fn status_log(status: CallStatus) -> Vec<(u64, Option<&'static str>, Option<&'static str>, Event)> {
    vec![
        (0, None, None, started("/w", None)),
        (1, Some("t_1"), None, turn()),
        (2, Some("t_1"), Some("a_1"), requested()),
        (
            3,
            Some("t_1"),
            Some("a_1"),
            skilled_completed(status, "alpha", "/w/alpha/SKILL.md"),
        ),
        (4, Some("t_2"), None, turn()),
        (5, Some("t_2"), None, handoff()),
    ]
}

/// Folds [`status_log`] and checks the seed holds no skill and the live
/// render agrees with the rebuild: the load of that status never counts.
fn check_status_seeds_nothing(name: &str, status: CallStatus) {
    let (_home, sessions) = sessions(name);
    let lines: Vec<Envelope> = status_log(status)
        .iter()
        .map(|(seq, turn, action, event)| envelope(A, *seq, *turn, *action, event))
        .collect();
    write_log(&sessions, A, &lines);
    let folded = resumed(&sessions.join(A)).unwrap();
    assert!(folded.seed.skills.is_empty());
    let mut live = Vec::new();
    let mut had = BTreeMap::new();
    let mut carry = crate::handoff::Carry::default();
    for line in &lines {
        let Some(event) = Event::from_envelope(line).unwrap() else {
            continue;
        };
        crate::conversation::render(
            &mut live,
            &event,
            line.action_id.as_ref(),
            "fake/model-1",
            &mut had,
            &mut carry,
            None,
        );
    }
    assert!(carry.skills.is_empty());
    let rebuilt = crate::conversation::rebuild(&lines, "fake/model-1").unwrap();
    assert_eq!(live, rebuilt);
}

#[test]
fn a_cancelled_skill_load_seeds_nothing() {
    check_status_seeds_nothing("seed-cancelled", CallStatus::Cancelled);
}

#[test]
fn a_denied_skill_load_seeds_nothing() {
    check_status_seeds_nothing("seed-denied", CallStatus::Denied);
}

#[test]
fn a_failed_skill_load_seeds_nothing() {
    check_status_seeds_nothing("seed-failed", CallStatus::Failed);
}

#[test]
fn a_crash_after_handoff_completed_rebuilds_the_armed_resend() {
    let (_home, sessions) = sessions("crash-window-one");
    write_seed_log(&sessions);
    let folded = resumed(&sessions.join(A)).unwrap();
    let window = vec![
        envelope(A, 4, Some("t_2"), None, &message_turn("ask")),
        envelope(A, 5, Some("t_2"), None, &begun()),
        envelope(A, 6, Some("t_2"), None, &handoff()),
    ];
    let (_, _, _, carry) =
        rebuild_and_sent(&window, "fake/model-1", &HashSet::new(), folded.seed).unwrap();
    assert_eq!(carry.resend, [alpha()]);
    assert!(carry.skills.is_empty());
}

#[test]
fn a_crash_after_skills_resent_rebuilds_no_resend() {
    let (_home, sessions) = sessions("crash-window-two");
    write_seed_log(&sessions);
    let folded = resumed(&sessions.join(A)).unwrap();
    let window = vec![
        envelope(A, 4, Some("t_2"), None, &message_turn("ask")),
        envelope(A, 5, Some("t_2"), None, &begun()),
        envelope(A, 6, Some("t_2"), None, &handoff()),
        envelope(
            A,
            7,
            Some("t_2"),
            None,
            &resent(&[("alpha", "/w/alpha/SKILL.md", "Alpha body.")]),
        ),
    ];
    let (conversation, _, _, carry) =
        rebuild_and_sent(&window, "fake/model-1", &HashSet::new(), folded.seed).unwrap();
    assert!(carry.resend.is_empty());
    assert_eq!(carry.skills, [alpha()]);
    assert_eq!(
        conversation,
        [
            Input::User {
                text: "ask".into(),
                images: Vec::new(),
            },
            Input::User {
                text: String::new(),
                images: Vec::new(),
            },
            Input::User {
                text: "/w/alpha/SKILL.md\n\nAlpha body.".into(),
                images: Vec::new(),
            },
        ]
    );
}

#[test]
fn a_complete_handoff_rebuilds_the_opening_and_the_skills() {
    let (_home, sessions) = sessions("crash-window-complete");
    write_seed_log(&sessions);
    let folded = resumed(&sessions.join(A)).unwrap();
    let window = vec![
        envelope(A, 4, Some("t_2"), None, &message_turn("ask")),
        envelope(A, 5, Some("t_2"), None, &begun()),
        envelope(A, 6, Some("t_2"), None, &handoff()),
        envelope(
            A,
            7,
            Some("t_2"),
            None,
            &resent(&[("alpha", "/w/alpha/SKILL.md", "Alpha body.")]),
        ),
        envelope(A, 8, Some("t_2"), None, &opening()),
    ];
    let (conversation, _, _, carry) =
        rebuild_and_sent(&window, "fake/model-1", &HashSet::new(), folded.seed).unwrap();
    assert!(carry.resend.is_empty());
    assert_eq!(conversation.len(), 4);
    assert!(matches!(&conversation[0], Input::User { text, .. } if text.contains("2023-11-14")));
    assert_eq!(
        conversation[1],
        Input::User {
            text: "ask".into(),
            images: Vec::new(),
        }
    );
    assert_eq!(
        conversation[3],
        Input::User {
            text: "/w/alpha/SKILL.md\n\nAlpha body.".into(),
            images: Vec::new(),
        }
    );
}

#[test]
fn a_skill_sent_again_is_seeded_at_the_next_handoff() {
    let (_home, sessions) = sessions("seed-resent");
    write_seed_log(&sessions);
    let mut lines: Vec<Envelope> = seed_log()
        .iter()
        .map(|(seq, turn, action, event)| envelope(A, *seq, *turn, *action, event))
        .collect();
    lines.push(envelope(
        A,
        7,
        Some("t_2"),
        None,
        &resent(&[("alpha", "/w/alpha/SKILL.md", "Alpha body.")]),
    ));
    lines.push(envelope(A, 8, Some("t_2"), None, &opening()));
    lines.push(envelope(A, 9, Some("t_3"), None, &turn()));
    lines.push(envelope(A, 10, Some("t_3"), None, &begun()));
    lines.push(envelope(A, 11, Some("t_3"), None, &handoff()));
    write_log(&sessions, A, &lines);
    let folded = resumed(&sessions.join(A)).unwrap();
    assert_eq!(folded.seed.skills, [alpha()]);
}

#[test]
fn an_empty_resend_seeds_nothing() {
    let (_home, sessions) = sessions("seed-empty-resent");
    write_seed_log(&sessions);
    let mut lines: Vec<Envelope> = seed_log()
        .iter()
        .map(|(seq, turn, action, event)| envelope(A, *seq, *turn, *action, event))
        .collect();
    lines.push(envelope(A, 7, Some("t_2"), None, &resent(&[])));
    lines.push(envelope(A, 8, Some("t_2"), None, &opening()));
    lines.push(envelope(A, 9, Some("t_3"), None, &turn()));
    lines.push(envelope(A, 10, Some("t_3"), None, &begun()));
    lines.push(envelope(A, 11, Some("t_3"), None, &handoff()));
    write_log(&sessions, A, &lines);
    let folded = resumed(&sessions.join(A)).unwrap();
    assert!(folded.seed.skills.is_empty());
}

#[test]
fn a_provider_run_record_carrying_a_skill_seeds_nothing() {
    let (_home, sessions) = sessions("seed-provider-run");
    let provided = Event::ToolCallCompleted(ToolCallCompleted {
        status: CallStatus::Completed,
        reason: None,
        error: None,
        process: None,
        content: vec![ContentPart::Text { text: "ok".into() }],
        details: None,
        artifact: None,
        kept: None,
        changes: None,
        control: Some(Control {
            handoff: None,
            questions: None,
            skill: Some(alpha()),
            name: None,
        }),
        changed_by: None,
        provider_item: Some(json!({"type": "hosted_tool_call"})),
        hosts_surfaced: Vec::new(),
        loaded: None,
    });
    let lines = vec![
        envelope(A, 0, None, None, &started("/w", None)),
        envelope(A, 1, Some("t_1"), None, &turn()),
        envelope(A, 2, Some("t_1"), Some("a_1"), &requested()),
        envelope(A, 3, Some("t_1"), Some("a_1"), &provided),
        envelope(A, 4, Some("t_2"), None, &turn()),
        envelope(A, 5, Some("t_2"), None, &handoff()),
    ];
    write_log(&sessions, A, &lines);
    let folded = resumed(&sessions.join(A)).unwrap();
    assert!(folded.seed.skills.is_empty());
    // The live render and the rebuild render the same bytes.
    let mut live = Vec::new();
    let mut had = BTreeMap::new();
    let mut carry = crate::handoff::Carry::default();
    for line in &lines {
        let Some(event) = Event::from_envelope(line).unwrap() else {
            continue;
        };
        crate::conversation::render(
            &mut live,
            &event,
            line.action_id.as_ref(),
            "fake/model-1",
            &mut had,
            &mut carry,
            None,
        );
    }
    assert!(carry.skills.is_empty());
    let rebuilt = crate::conversation::rebuild(&lines, "fake/model-1").unwrap();
    assert_eq!(live, rebuilt);
}

#[test]
fn a_skill_load_with_hosts_reaches_the_seed_and_the_known_hosts() {
    let (_home, sessions) = sessions("seed-hosts");
    let mut loaded = skilled_completed(CallStatus::Completed, "alpha", "/w/alpha/SKILL.md");
    let Event::ToolCallCompleted(ref mut done) = loaded else {
        unreachable!();
    };
    done.hosts_surfaced = vec!["https://example.com/search".into()];
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_1"), None, &turn()),
            envelope(A, 2, Some("t_1"), Some("a_1"), &requested()),
            envelope(A, 3, Some("t_1"), Some("a_1"), &loaded),
            envelope(A, 4, Some("t_2"), None, &turn()),
            envelope(A, 5, Some("t_2"), None, &handoff()),
        ],
    );
    let folded = resumed(&sessions.join(A)).unwrap();
    // A completion carrying both a skill load and surfaced hosts updates both folded states.
    assert_eq!(folded.seed.skills, [alpha()]);
    assert!(
        folded
            .known_hosts
            .all_known(&["https://example.com/search".to_owned()])
    );
}

#[test]
fn a_turn_start_history_seed_keeps_extension_job_ownership() {
    let (_home, sessions) = sessions("extension-seed");
    let job = Event::JobStarted(contract::events::JobStarted {
        job_id: contract::JobId("j_owned".into()),
        tool: None,
        extension: Some("test/scan".into()),
        description: "scan".into(),
        output_path: "/child/events.jsonl".into(),
    });
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &job),
            envelope(A, 2, Some("t_1"), None, &turn()),
            envelope(A, 3, Some("t_1"), None, &handoff()),
        ],
    );
    let folded = resumed(&sessions.join(A)).unwrap();
    assert!(folded.seed.owned("j_owned"));
    assert_eq!(folded.seed.jobs, [("j_owned".into(), "scan".into())]);
}

#[test]
fn a_cut_handoff_renders_nothing_and_keeps_the_skills_loaded() {
    let seed = crate::handoff::Carry {
        skills: vec![alpha()],
        ..crate::handoff::Carry::default()
    };
    let lines = vec![
        envelope(A, 0, Some("t_1"), None, &message_turn("ask")),
        envelope(A, 1, Some("t_1"), Some("a_1"), &requested()),
        envelope(
            A,
            2,
            Some("t_1"),
            Some("a_1"),
            &skilled_completed(CallStatus::Completed, "alpha", "/w/alpha/SKILL.md"),
        ),
        envelope(A, 3, Some("t_1"), None, &handoff()),
        envelope(
            A,
            4,
            Some("t_1"),
            None,
            &resent(&[("alpha", "/w/alpha/SKILL.md", "Alpha body.")]),
        ),
        envelope(A, 5, Some("t_1"), None, &opening()),
    ];
    let (conversation, _) = summary_conversation(&lines, 3, "fake/model-1", seed.clone()).unwrap();
    // The cut handoff adds no input: the call and its result only, no
    // skill text and no opening.
    assert_eq!(conversation.len(), 3);
    assert!(
        !conversation
            .iter()
            .any(|input| matches!(input, Input::User { text, .. } if text.contains("Alpha body."))),
        "{conversation:?}"
    );
    // The cut changes no carry: no resend is armed, and the old context's
    // loads stay loaded, as rendering the prefix alone shows.
    let (_, _, _, carry) =
        rebuild_and_sent(&lines[..3], "fake/model-1", &HashSet::new(), seed).unwrap();
    assert!(carry.resend.is_empty());
    assert_eq!(carry.skills, [alpha()]);
}

fn kept(keys: &[(u64, u64)]) -> Event {
    Event::ReviewerKept(contract::events::ReviewerKept {
        kept: keys
            .iter()
            .map(|&(seq, item)| contract::events::KeptMessage {
                seq: Seq(seq),
                item,
            })
            .collect(),
        failed: None,
    })
}

fn user(text: &str) -> Input {
    Input::User {
        text: text.into(),
        images: Vec::new(),
    }
}

/// Turn 1's message A, then the handoff turn's message B, its completed
/// handoff, the new opening and the reviewer's selection of both.
fn kept_log() -> Vec<Envelope> {
    vec![
        envelope(A, 0, None, None, &started("/w", None)),
        envelope(A, 1, Some("t_1"), None, &message_turn("A")),
        envelope(A, 2, Some("t_2"), None, &message_turn("B")),
        envelope(A, 3, Some("t_2"), None, &begun()),
        envelope(A, 4, Some("t_2"), None, &handoff()),
        envelope(A, 5, Some("t_2"), None, &opening()),
        envelope(A, 6, Some("t_2"), None, &kept(&[(1, 0), (2, 0)])),
    ]
}

#[test]
fn a_resume_seed_carries_the_kept_messages() {
    let (_home, sessions) = sessions("seed-kept");
    let lines = kept_log();
    write_log(&sessions, A, &lines);
    let folded = resumed(&sessions.join(A)).unwrap();
    // The window starts at the handoff turn, so message A is only in the
    // seed.
    assert_eq!(folded.window, (0, 2));
    let window = &lines[2..];
    let (conversation, _, _, carry) =
        rebuild_and_sent(window, "fake/model-1", &HashSet::new(), folded.seed).unwrap();
    assert!(matches!(&conversation[0], Input::User { text, .. } if text.contains("2023-11-14")));
    assert_eq!(conversation[1..], [user("A"), user("B"), user("")]);
    // The same rebuild over the whole log holds the same index.
    let (whole, _, _, whole_carry) = rebuild_and_sent(
        &lines,
        "fake/model-1",
        &HashSet::new(),
        crate::handoff::Carry::default(),
    )
    .unwrap();
    assert_eq!(whole, conversation);
    assert_eq!(whole_carry.person, carry.person);
    assert_eq!(carry.person.len(), 2);
}

#[test]
fn a_summary_turn_message_never_reaches_the_seed() {
    let (_home, sessions) = sessions("seed-summary");
    let summary = Event::TurnStarted(TurnStarted {
        input: vec![
            InputItem::Rewind {
                command_id: contract::CommandId("c_r".into()),
            },
            InputItem::Message {
                content: vec![ContentPart::Text {
                    text: "summarise".into(),
                }],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: None,
                },
                changed_by: None,
            },
        ],
    });
    let completed = Event::TurnCompleted(contract::events::TurnCompleted {
        outcome: contract::events::TurnOutcome::Completed,
        error: None,
        questions: None,
    });
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, Some("t_1"), None, &summary),
            envelope(A, 2, Some("t_1"), None, &completed),
            envelope(A, 3, Some("t_2"), None, &message_turn("ask")),
            envelope(A, 4, Some("t_2"), None, &handoff()),
        ],
    );
    let folded = resumed(&sessions.join(A)).unwrap();
    assert_eq!(folded.window, (0, 3));
    assert!(folded.seed.person.is_empty(), "{:?}", folded.seed.person);
}

#[test]
fn a_reviewer_kept_after_the_cut_inserts_nothing() {
    let handoff_lines = |kept_line: bool| {
        let mut lines = vec![
            envelope(A, 0, Some("t_1"), None, &message_turn("A")),
            envelope(A, 1, Some("t_2"), None, &message_turn("B")),
            envelope(A, 2, Some("t_2"), None, &handoff()),
            envelope(A, 3, Some("t_2"), None, &opening()),
        ];
        if kept_line {
            lines.push(envelope(A, 4, Some("t_2"), None, &kept(&[(0, 0), (1, 0)])));
        }
        lines
    };
    let seed = crate::handoff::Carry::default();
    // Cut before the handoff completes: it renders as failed, so the
    // selection has nothing to join.
    let (before, _) =
        summary_conversation(&handoff_lines(true), 2, "fake/model-1", seed.clone()).unwrap();
    let (without, _) =
        summary_conversation(&handoff_lines(false), 2, "fake/model-1", seed.clone()).unwrap();
    assert_eq!(before, without);
    assert_eq!(before, [user("A"), user("B")]);
    // Cut between the completed handoff and its selection: the selection
    // is past the cut and inserts nothing.
    let (after, _) = summary_conversation(&handoff_lines(true), 4, "fake/model-1", seed).unwrap();
    assert_eq!(after.len(), 3);
    assert!(matches!(&after[0], Input::User { text, .. } if text.contains("2023-11-14")));
    assert_eq!(after[1..], [user("B"), user("")]);
}

/// A granted permission: the grant the fold carries.
fn granted(tool: &str) -> Event {
    Event::PermissionResolved(PermissionResolved {
        call: None,
        request_id: None,
        decision: Decision::Allow,
        decided_by: DecidedBy::Reviewer,
        reason: None,
        feedback: None,
        grant: Some(Grant {
            tool: tool.to_owned(),
            prefix: "cargo".to_owned(),
        }),
        rule: None,
        reviewer: None,
        hosts: None,
    })
}

/// A `session_started` in `workspace`, from a delegate `parent` and a
/// `forked_from` point: the delegate and move shapes the fold tells apart
/// by `parent` and the workspace alone.
fn started_as(workspace: &str, parent: Option<&str>, from: Option<(&str, u64)>) -> Event {
    Event::SessionStarted(SessionStarted {
        workspace: workspace.to_owned(),
        variables: Variables {
            path: "/usr/bin".to_owned(),
            names: Vec::new(),
            source: VariablesSource::Inherited,
        },
        parent: parent.map(|session| Parent {
            session_id: SessionId(session.to_owned()),
            delegate_id: contract::JobId("j_1".to_owned()),
        }),
        forked_from: from.map(|(session, seq)| Point {
            session_id: SessionId(session.to_owned()),
            seq: Seq(seq),
            project: None,
        }),
        rewind: None,
        worktree: None,
        supervised: false,
    })
}

/// The old log's last line continuing `next` at `seq`.
fn rewound(next: &str, seq: u64) -> Event {
    Event::Rewound(Rewound {
        new_session_id: SessionId(next.to_owned()),
        seq: Seq(seq),
        from_session_id: None,
        summary: None,
    })
}

/// The old log's last line moving to `next` at `seq`.
fn moved(next: &str, seq: u64) -> Event {
    Event::Moved(Moved {
        new_session_id: SessionId(next.to_owned()),
        seq: Seq(seq),
        workspace: "/w2".to_owned(),
    })
}

/// An offer of one skippable item: open while unresolved.
fn offered_item(id: &str) -> Event {
    Event::RepositoryCodeOffered(RepositoryCodeOffered {
        request_id: contract::RequestId(id.to_owned()),
        items: vec![OfferedItem {
            kind: OfferedKind::Extension,
            name: "n".to_owned(),
            hash: "h".to_owned(),
            required: false,
            summary: "s".to_owned(),
            files: Vec::new(),
            version: None,
            diff: None,
        }],
    })
}

/// The person's skip of the `offered_item` offer.
fn skipped(id: &str) -> Event {
    Event::RepositoryCodeResolved(RepositoryCodeResolved {
        request_id: contract::RequestId(id.to_owned()),
        decisions: vec![OfferDecision::Skip],
    })
}

/// A rewind edge and a move edge fold in one pass: the rewind keeps the
/// old workspace's state, and the move into another workspace drops the
/// grants and offers folded before it while keeping the session's model
/// and handoff state.
#[test]
fn a_rewind_then_a_move_clears_grants_and_offers_on_entry() {
    let (_home, sessions) = sessions("fold-move");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &built(Some("work"))),
            envelope(A, 2, None, None, &granted("shell")),
            envelope(A, 3, None, None, &offered_item("r_1")),
            envelope(A, 4, None, None, &rewound(B, 4)),
        ],
    );
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started_as("/w", None, Some((A, 4)))),
            envelope(B, 1, None, None, &granted("git")),
            envelope(B, 2, None, None, &offered_item("r_2")),
            envelope(B, 3, None, None, &moved(C, 3)),
        ],
    );
    write_log(
        &sessions,
        C,
        &[envelope(
            C,
            0,
            None,
            None,
            &started_as("/w2", None, Some((B, 3))),
        )],
    );
    let folded = resumed(&sessions.join(C)).unwrap();
    assert_eq!(folded.session, C);
    assert_eq!(folded.root, A);
    assert_eq!(folded.workspace, "/w2");
    // The rewind into the same workspace kept both; the move into
    // another cleared them on entry.
    assert!(folded.grants.is_empty(), "{:?}", folded.grants);
    assert_eq!(folded.offers.counts(), (0, 0));
    // The session's other handoff state survives the move: its model,
    // credential and window.
    assert_eq!(folded.model.as_deref(), Some("fake/model-1"));
    assert_eq!(folded.credential.as_deref(), Some("work"));
    assert_eq!(folded.window, (0, 0));
}

/// A move into the same workspace keeps the grants and offers folded
/// before it: only a workspace change clears them.
#[test]
fn a_move_into_the_same_workspace_keeps_grants_and_offers() {
    let (_home, sessions) = sessions("fold-same-workspace");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &granted("shell")),
            envelope(A, 2, None, None, &offered_item("r_1")),
            envelope(A, 3, None, None, &skipped("r_1")),
            envelope(A, 4, None, None, &offered_item("r_2")),
            envelope(A, 5, None, None, &moved(B, 5)),
        ],
    );
    write_log(
        &sessions,
        B,
        &[envelope(
            B,
            0,
            None,
            None,
            &started_as("/w", None, Some((A, 5))),
        )],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    assert_eq!(folded.grants.len(), 1);
    assert_eq!(folded.grants[0].tool, "shell");
    // The skip resolved before the move stays skipped, and the open
    // offer stays open.
    assert_eq!(folded.offers.counts(), (1, 1));
}

/// A cross-workspace delegate fork keeps both: its `parent` names the
/// delegating turn, so the reset for moves never runs for it.
#[test]
fn a_cross_workspace_fork_with_a_parent_keeps_grants_and_offers() {
    let (_home, sessions) = sessions("fold-delegate");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &granted("shell")),
            envelope(A, 2, None, None, &offered_item("r_1")),
        ],
    );
    write_log(
        &sessions,
        B,
        &[envelope(
            B,
            0,
            None,
            None,
            &started_as("/w2", Some(A), Some((A, 2))),
        )],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    assert_eq!(folded.grants.len(), 1);
    assert_eq!(folded.grants[0].tool, "shell");
    assert_eq!(folded.offers.counts(), (1, 0));
}

/// A grant resolved after the move folds again: the reset clears the
/// staging area on entry, not the child's own lines after it.
#[test]
fn a_grant_after_the_move_folds_after_the_reset() {
    let (_home, sessions) = sessions("fold-after-move");
    write_log(
        &sessions,
        A,
        &[
            envelope(A, 0, None, None, &started("/w", None)),
            envelope(A, 1, None, None, &granted("shell")),
            envelope(A, 2, None, None, &moved(B, 2)),
        ],
    );
    write_log(
        &sessions,
        B,
        &[
            envelope(B, 0, None, None, &started_as("/w2", None, Some((A, 2)))),
            envelope(B, 1, None, None, &granted("git")),
        ],
    );
    let folded = resumed(&sessions.join(B)).unwrap();
    assert_eq!(folded.grants.len(), 1);
    assert_eq!(folded.grants[0].tool, "git");
}
