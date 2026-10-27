//! Tests for the `rewind` command's point: what counts as a step boundary
//! in one log (`docs/events.md`, "Rewind"), and the default point.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use contract::{ActionId, Envelope, Seq, SessionId};

use super::{Target, point_for, valid_session_id};

fn line(kind: &str, seq: u64, action: Option<&str>) -> Envelope {
    Envelope {
        kind: kind.into(),
        session_id: SessionId("s_0123456789abcdef".into()),
        ts: 1,
        schema_version: contract::SCHEMA_VERSION,
        turn_id: None,
        action_id: action.map(|action| ActionId(action.into())),
        seq: Some(Seq(seq)),
        payload: if kind == "turn_started" {
            serde_json::json!({"input": []})
                .as_object()
                .unwrap()
                .clone()
        } else {
            serde_json::Map::new()
        },
    }
}

fn own_target() -> Target {
    Target {
        session: SessionId("s_0123456789abcdef".into()),
        dir: std::path::PathBuf::from("/sessions/s_0123456789abcdef"),
        bound: None,
    }
}

fn ancestor_target(to: u64) -> Target {
    Target {
        session: SessionId("s_aaaaaaaaaaaaaaaa".into()),
        dir: std::path::PathBuf::from("/sessions/s_aaaaaaaaaaaaaaaa"),
        bound: Some(to),
    }
}

fn boundary() -> String {
    "Line 4 is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results.".to_owned()
}

/// One log with a turn starting at 5: `turn_started` is the line after the
/// point 4.
fn two_turns() -> Vec<Envelope> {
    vec![
        line("session_started", 0, None),
        line("turn_started", 1, None),
        line("turn_completed", 2, None),
        line("turn_started", 5, None),
        line("turn_completed", 6, None),
    ]
}

#[test]
fn a_turn_start_after_the_point_is_a_step_boundary() {
    let lines = two_turns();
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(4)), false), Ok(4));
}

#[test]
fn a_step_start_after_the_point_is_a_step_boundary() {
    let lines = vec![
        line("session_started", 0, None),
        line("turn_started", 1, None),
        line("step_started", 3, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(2)), false), Ok(2));
}

#[test]
fn a_step_start_after_a_turn_start_is_a_step_boundary() {
    let lines = vec![
        line("session_started", 0, None),
        line("turn_started", 4, None),
        line("step_started", 5, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(3)), false), Ok(3));
}

#[test]
fn other_kinds_after_the_point_are_not_step_boundaries() {
    for kind in [
        "tool_call_completed",
        "turn_completed",
        "fiber_started",
        "session_started",
    ] {
        let lines = vec![line("session_started", 0, None), line(kind, 5, None)];
        let refused = point_for(&lines, &own_target(), Some(Seq(4)), false).unwrap_err();
        assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
        assert_eq!(refused.message, boundary(), "{kind}");
    }
}

#[test]
fn the_last_line_is_not_a_step_boundary() {
    let lines = two_turns();
    let last = lines.last().unwrap().seq.unwrap().0;
    let refused = point_for(&lines, &own_target(), Some(Seq(last)), false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        format!(
            "Line {last} is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results."
        )
    );
}

#[test]
fn a_point_past_the_end_is_not_a_step_boundary() {
    let lines = two_turns();
    let refused = point_for(&lines, &own_target(), Some(Seq(60)), false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "Line 60 is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results."
    );
}

#[test]
fn an_unanswered_call_before_the_point_refuses_it() {
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, Some("a_1")),
        line("turn_started", 5, None),
        line("tool_call_completed", 6, Some("a_1")),
    ];
    let refused = point_for(&lines, &own_target(), Some(Seq(4)), false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(refused.message, boundary());
}

#[test]
fn a_completion_for_a_call_after_the_point_accepts_it() {
    let lines = vec![
        line("session_started", 0, None),
        line("turn_started", 5, None),
        line("tool_call_requested", 6, Some("a_1")),
        line("tool_call_completed", 7, Some("a_1")),
    ];
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(4)), false), Ok(4));
}

#[test]
fn a_completion_for_a_call_at_the_point_refuses_it() {
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 4, Some("a_1")),
        line("turn_started", 5, None),
        line("tool_call_completed", 6, Some("a_1")),
    ];
    let refused = point_for(&lines, &own_target(), Some(Seq(4)), false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(refused.message, boundary());
}

#[test]
fn a_point_at_the_bound_is_accepted() {
    let lines = two_turns();
    assert_eq!(
        point_for(&lines, &ancestor_target(4), Some(Seq(4)), false),
        Ok(4)
    );
}

#[test]
fn a_point_past_the_bound_is_not_in_this_sessions_history() {
    let lines = two_turns();
    let refused = point_for(&lines, &ancestor_target(4), Some(Seq(5)), false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "Line 5 of session s_aaaaaaaaaaaaaaaa is not in this session's history."
    );
}

#[test]
fn the_default_point_is_the_latest_turn_start_minus_one() {
    let lines = two_turns();
    assert_eq!(point_for(&lines, &own_target(), None, false), Ok(4));
}

#[test]
fn the_default_point_rejects_an_unreadable_turn() {
    let mut broken = line("turn_started", 7, None);
    broken.payload.insert("input".into(), serde_json::json!(7));
    let refused = point_for(&[broken], &own_target(), None, false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::LogCorrupt);
    assert!(refused.message.contains("sequence"), "{}", refused.message);
}

#[test]
fn the_default_point_ignores_summary_turns() {
    let mut lines = two_turns();
    let mut summary = line("turn_started", 7, None);
    summary.payload.insert(
        "input".into(),
        serde_json::json!([{"type":"rewind", "command_id":"any"}]),
    );
    lines.push(summary.clone());
    assert_eq!(point_for(&lines, &own_target(), None, false), Ok(4));
    let refused = point_for(&[summary], &own_target(), None, false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(refused.message, "This session has no turn to rewind to.");
}

#[test]
fn the_default_point_with_one_turn_is_before_it() {
    let lines = vec![
        line("session_started", 0, None),
        line("fiber_started", 1, None),
        line("turn_started", 2, None),
        line("turn_completed", 3, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), None, false), Ok(1));
}

#[test]
fn the_default_point_with_no_turn_is_invalid_arguments() {
    let lines = vec![
        line("session_started", 0, None),
        line("fiber_started", 1, None),
    ];
    let refused = point_for(&lines, &own_target(), None, false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(refused.message, "This session has no turn to rewind to.");
}

#[test]
fn the_default_point_stops_at_the_bound() {
    // The latest turn starts past the bound: the default is the latest
    // turn start at or before it.
    let lines = two_turns();
    assert_eq!(point_for(&lines, &ancestor_target(2), None, false), Ok(0));
}

#[test]
fn the_default_point_with_no_turn_before_the_bound_is_invalid_arguments() {
    let lines = two_turns();
    let refused = point_for(&lines, &ancestor_target(0), None, false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::InvalidArguments);
    assert_eq!(refused.message, "This session has no turn to rewind to.");
}

#[test]
fn a_point_without_a_representable_successor_is_not_a_step_boundary() {
    // `u64::MAX` has no next line: the lookup for it must refuse, not
    // wrap or panic.
    let lines = two_turns();
    let refused = point_for(&lines, &own_target(), Some(Seq(u64::MAX)), false).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        format!(
            "Line {} is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results.",
            u64::MAX
        )
    );
}

#[test]
fn a_completion_at_the_point_answers_a_call_before_it() {
    // Only a completion after the point blocks it: one at the point is
    // the answer, not a wait.
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, Some("a_1")),
        line("tool_call_completed", 4, Some("a_1")),
        line("turn_started", 5, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), Some(Seq(4)), false), Ok(4));
}

/// The end-of-log point (`at_end`, sent by `/fork`) is the last line's
/// `seq`, whatever kind it is: it needs no next line.
#[test]
fn at_end_resolves_to_the_last_lines_seq() {
    let lines = two_turns();
    assert_eq!(point_for(&lines, &own_target(), None, true), Ok(6));
}

#[test]
fn at_end_includes_a_line_appended_after_the_send() {
    // The admission-time read holds the new line: the point moves with it.
    let mut lines = two_turns();
    lines.push(line("usage_recorded", 7, None));
    assert_eq!(point_for(&lines, &own_target(), None, true), Ok(7));
}

#[test]
fn at_end_accepts_every_answered_call_shape() {
    // Requested and completed, started and completed, and the full
    // requested-started-completed shape: each action id has its answer.
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, Some("a_1")),
        line("tool_call_completed", 2, Some("a_1")),
        line("tool_call_started", 3, Some("a_2")),
        line("tool_call_completed", 4, Some("a_2")),
        line("tool_call_requested", 5, Some("a_3")),
        line("tool_call_started", 6, Some("a_3")),
        line("tool_call_completed", 7, Some("a_3")),
        line("turn_completed", 8, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), None, true), Ok(8));
}

#[test]
fn at_end_accepts_a_hosted_provider_call_with_its_completion() {
    // A call the provider ran itself is still a `tool_call_requested`
    // line: its action id is what the completion answers.
    let mut requested = line("tool_call_requested", 1, Some("a_1"));
    requested.payload.insert(
        "provider_item".into(),
        serde_json::json!({"type": "web_search_call"}),
    );
    let lines = vec![
        line("session_started", 0, None),
        requested,
        line("tool_call_completed", 2, Some("a_1")),
        line("turn_completed", 3, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), None, true), Ok(3));
}

#[test]
fn at_end_ignores_a_call_line_with_no_action_id() {
    // With no action id there is no call to answer.
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, None),
        line("turn_completed", 2, None),
    ];
    assert_eq!(point_for(&lines, &own_target(), None, true), Ok(2));
}

#[test]
fn at_end_refuses_a_requested_call_with_no_completion() {
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, Some("a_1")),
        line("turn_completed", 2, None),
    ];
    let refused = point_for(&lines, &own_target(), None, true).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "The end of the log is not a step boundary: a tool call before it has no recorded result."
    );
}

#[test]
fn at_end_refuses_a_started_call_with_no_completion() {
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_started", 1, Some("a_1")),
        line("turn_completed", 2, None),
    ];
    let refused = point_for(&lines, &own_target(), None, true).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "The end of the log is not a step boundary: a tool call before it has no recorded result."
    );
}

#[test]
fn at_end_refuses_a_completion_for_another_call() {
    // `a_1` is requested but only `a_2` completes: `a_1` is unanswered.
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, Some("a_1")),
        line("tool_call_completed", 2, Some("a_2")),
        line("turn_completed", 3, None),
    ];
    let refused = point_for(&lines, &own_target(), None, true).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "The end of the log is not a step boundary: a tool call before it has no recorded result."
    );
}

#[test]
fn at_end_refuses_an_unanswered_call_after_a_restart() {
    // A crash left the call unanswered: a later `fiber_started` does not
    // excuse it.
    let lines = vec![
        line("session_started", 0, None),
        line("tool_call_requested", 1, Some("a_1")),
        line("fiber_started", 2, None),
        line("turn_completed", 3, None),
    ];
    let refused = point_for(&lines, &own_target(), None, true).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "The end of the log is not a step boundary: a tool call before it has no recorded result."
    );
}

#[test]
fn at_end_without_a_last_line_seq_is_not_a_step_boundary() {
    let mut lines = two_turns();
    lines.pop();
    let mut last = line("turn_completed", 6, None);
    last.seq = None;
    lines.push(last);
    let refused = point_for(&lines, &own_target(), None, true).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "The end of the log is not a step boundary: its last line has no recorded position."
    );
}

#[test]
fn at_end_on_an_empty_log_is_not_a_step_boundary() {
    let refused = point_for(&[], &own_target(), None, true).unwrap_err();
    assert_eq!(refused.code, contract::ErrorCode::NotStepBoundary);
    assert_eq!(
        refused.message,
        "The end of the log is not a step boundary: the log holds no line."
    );
}

#[test]
fn a_session_id_is_s_and_sixteen_lowercase_hex_digits() {
    for (text, valid) in [
        ("s_0123456789abcdef", true),
        ("s_0123456789abcdeg", false),
        ("s_0123456789ABCDEF", false),
        ("s_0123456789abcde", false),
        ("s_0123456789abcdef0", false),
        ("0123456789abcdef", false),
        ("s_gggggggggggggggg", false),
    ] {
        assert_eq!(valid_session_id(text), valid, "{text}");
    }
}

/// The rewind path's closed-starts test (N20 (g)): its own small loop rig
/// and `Jobs` fake, copying `shutdown_tests.rs`'s `World::new` and
/// `Listed` shape. The fake models the registry's rule: a held start
/// signals before the fake's gate is taken, then reads the fake's closed
/// flag under it.
mod closed_starts {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "test helpers; a failure is the test's"
    )]

    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use contract::commands::RewindArgs;
    use contract::events::{Event, TurnStarted};
    use contract::inbox::{Ack, Delivery, Rejection};
    use contract::jobs::{Foreground, Jobs, OpenError, Opened, Opening};
    use contract::{CommandId, ErrorCode, JobId, Seq, SessionId};
    use fakes::Deadline;
    use fakes::StubJobs;
    use fakes::clock::FakeClock;

    use crate::Loop;
    use crate::inbox::TurnInput;

    const DEADLINE: Duration = Duration::from_secs(10);

    struct Listed {
        running: StubJobs,
        order: Mutex<Vec<String>>,
        gate: Mutex<()>,
        closed: Mutex<bool>,
        hold: Mutex<Option<(Sender<()>, Receiver<()>)>>,
        bound: Mutex<Vec<JobId>>,
        launched: Mutex<usize>,
    }

    impl Listed {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                running: StubJobs::default(),
                order: Mutex::new(Vec::new()),
                gate: Mutex::new(()),
                closed: Mutex::new(false),
                hold: Mutex::new(None),
                bound: Mutex::new(Vec::new()),
                launched: Mutex::new(0),
            })
        }

        fn hold_start(&self, reached: Sender<()>, release: Receiver<()>) {
            *self.hold.lock().unwrap() = Some((reached, release));
        }

        fn start(&self, id: &str) -> Result<JobId, (ErrorCode, String)> {
            if let Some((reached, release)) = self.hold.lock().unwrap().take() {
                let _sent = reached.send(());
                let _released = release.recv();
            }
            let _gate = self.gate.lock().unwrap();
            if *self.closed.lock().unwrap() {
                return Err((
                    ErrorCode::Closing,
                    "The session is ending, so no delegate was started.".into(),
                ));
            }
            let job = JobId(id.into());
            self.bound.lock().unwrap().push(job.clone());
            self.running.update_running(|ids| ids.push(job.clone()));
            *self.launched.lock().unwrap() += 1;
            Ok(job)
        }
    }

    impl Jobs for Listed {
        fn open(&self, _: Opening) -> Result<Opened, OpenError> {
            Err(OpenError {
                path: "unused".into(),
                source: std::io::Error::other("a listed job is not opened"),
            })
        }

        fn stop(&self, _: &JobId) -> bool {
            false
        }

        fn stop_delegates(&self) -> usize {
            0
        }

        fn background(&self) -> usize {
            0
        }

        fn foreground(&self, _: Foreground) {}

        fn running(&self) -> Vec<JobId> {
            self.order.lock().unwrap().push("running".into());
            Jobs::running(&self.running)
        }

        fn deliver_to(&self, _: Sender<Delivery>) {}

        fn close_extension_starts(&self) {
            let _gate = self.gate.lock().unwrap();
            *self.closed.lock().unwrap() = true;
            self.order.lock().unwrap().push("close".into());
        }
    }

    struct Rig {
        _home: fakes::TempDir,
        _inbox: Sender<Delivery>,
        looped: Loop,
    }

    fn rig(jobs: Arc<Listed>) -> Rig {
        let home = fakes::TempDir::new("fiber-rewind-close");
        let workspace = home.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let fake = FakeClock::new();
        let clock: Arc<dyn contract::clock::Clock> = fake.clone();
        let log = Arc::new(
            log::Log::create(home.path(), SessionId("s_test".into()), Arc::clone(&clock)).unwrap(),
        );
        let (inbox, rx) = mpsc::channel();
        let looped = Loop::start(
            crate::Session {
                log,
                provider: Arc::new(fakes::ScriptedProvider::new(Vec::new())),
                model: crate::Model {
                    reference: "fake/model".into(),
                    cost: None,
                    subscription: false,
                },
                prompt: crate::prompt::PromptInputs::new(
                    home.path().to_path_buf(),
                    "/bin/sh".into(),
                    home.path()
                        .join("s_test/events.jsonl")
                        .display()
                        .to_string(),
                    clock,
                    fakes::CONTEXT_WINDOW,
                ),
                inbox: rx,
                tools: Vec::new(),
                permissions: crate::Permissions {
                    workspace: workspace.display().to_string(),
                    credentials: home.path().join("credentials"),
                    credential_files: Vec::new(),
                    rules: Arc::new(fakes::NoRules),
                },
            },
            None,
        )
        .unwrap()
        .jobs(jobs as Arc<dyn Jobs>);
        Rig {
            _home: home,
            _inbox: inbox,
            looped,
        }
    }

    #[test]
    fn a_start_held_past_the_rewind_sweep_is_refused_closed() {
        let jobs = Listed::new();
        let mut rig = rig(Arc::clone(&jobs));
        // One turn in the log, so point 0 is a step boundary: the next
        // line starts a turn.
        rig.looped
            .write(
                &Event::TurnStarted(TurnStarted { input: vec![] }),
                None,
                None,
            )
            .unwrap();
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        jobs.hold_start(reached_tx, release_rx);
        let (result_tx, result_rx) = mpsc::channel();
        let starter = Arc::clone(&jobs);
        std::thread::spawn(move || {
            let _sent = result_tx.send(starter.start("j_new"));
        });
        let wait = Deadline::after(DEADLINE);
        wait.recv(&reached_rx)
            .expect("the start holds at the pause point");
        let (ack_tx, ack_rx) = mpsc::channel();
        let ack = Ack(Box::new(move |result: Result<_, Rejection>| {
            let _sent = ack_tx.send(result.map(|_| ()));
        }));
        rig.looped
            .take_rewind(
                CommandId("c_1".into()),
                RewindArgs {
                    from_session_id: None,
                    seq: Some(Seq(0)),
                    summarise: false,
                    at_end: false,
                },
                ack,
                &TurnInput::of(Vec::new()),
            )
            .unwrap();
        wait.recv(&ack_rx)
            .expect("the rewind is acknowledged")
            .expect("the rewind is accepted");
        // The close runs beside `close_admission`, before
        // `stop_jobs_for_rewind`'s first `running()` read.
        let order = jobs.order.lock().unwrap().clone();
        assert!(
            order.len() >= 2 && order[0] == "close" && order[1] == "running",
            "{order:?}"
        );
        drop(release_tx);
        let Err((code, message)) = wait.recv(&result_rx).expect("the start answered") else {
            panic!("the held start is refused");
        };
        assert_eq!(code, ErrorCode::Closing);
        assert_eq!(
            message,
            "The session is ending, so no delegate was started."
        );
        assert!(jobs.bound.lock().unwrap().is_empty());
        assert_eq!(*jobs.launched.lock().unwrap(), 0);
        assert!(jobs.running().is_empty());
    }
}
