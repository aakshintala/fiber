//! A session that continues another from a step boundary
//! (`docs/events.md`, "Rewind"): the inherited preamble, the note, the cache
//! key and the old session's worktree, at crate level.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

extern crate support as ext_support;

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use contract::commands::{Reply, ReplyAnswer, RewindArgs};
use contract::events::{
    CommandResult, Parent, SessionStarted, TurnOutcome, Variables, VariablesSource,
};
use contract::inbox::{Ack, Answer, Delivery, Rejection};
use contract::provider::Input;
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{ContentPart, Failure, Origin, Point, Sender, Worktree};
use contract::tool::Tool;
use contract::{CommandId, Envelope, ErrorCode, JobId, RequestId, Seq, SessionId};
use fakes::Deadline;
use fakes::{Scripted, ScriptedProvider, StubJobs};
use log::Log;
use r#loop::{Continuation, Loop, Model, Permissions, PromptInputs, Rewound, rewind_note};
use serde_json::{Map, Value, json};

use support::{
    DEADLINE, MODEL, Session, TestTool, allow, calls_reply, delivery, ignore, kinds, message,
    paris, read_until, reply_to, rewind,
};

const B_ID: &str = "s_rewound00000001";

fn write_tool() -> Arc<TestTool> {
    Arc::new(TestTool::declaring(
        "write_file",
        "Wrote it.",
        vec![contract::shapes::Effect::Writes],
        Some(vec!["/w/note.txt".into()]),
    ))
}

fn exec_tool() -> Arc<TestTool> {
    Arc::new(TestTool::declaring(
        "run_cmd",
        "Ran it.",
        vec![contract::shapes::Effect::Executes],
        None,
    ))
}

fn allow_rule(tool: &str) -> Rule {
    // A standing allow, so the call runs without asking.
    Rule {
        decision: RuleDecision::Allow,
        tool: tool.into(),
        prefix: String::new(),
        added: None,
        session_id: None,
    }
}

/// Session A after two turns: turn 1 answers "one-done", turn 2 calls both
/// tools, then answers "two-done".
fn run_a() -> Session {
    run_a_with_tail(vec![])
}

fn run_a_with_tail(tail: Vec<Scripted>) -> Session {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::clone(&write_tool()) as Arc<dyn Tool>,
        Arc::clone(&exec_tool()) as Arc<dyn Tool>,
    ];
    let mut session = Session::with_tools(
        vec![
            Scripted::text("one-done"),
            calls_reply("working", &[("write_file", paris()), ("run_cmd", paris())]),
            Scripted::text("two-done"),
        ]
        .into_iter()
        .chain(tail)
        .collect(),
        None,
        tools,
    );
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.rules.set(StandingRules {
        global: vec![allow_rule("write_file"), allow_rule("run_cmd")],
        project: Vec::new(),
    });
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session
}

/// Turn 2's `turn_started` seq, minus one: the point B continues from.
fn point(lines: &[Envelope]) -> u64 {
    let mut starts = lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .map(|line| line.seq.unwrap().0);
    starts.next().unwrap();
    starts.next().unwrap() - 1
}

/// Starts B from A's session: a new log beside A's, rewound from `from`
/// with `note`, making its first request from `script`.
fn start_b(
    session: &Session,
    id: &str,
    from: Point,
    note: String,
    worktree: Option<Worktree>,
    script: Vec<Scripted>,
) -> (Loop, mpsc::Sender<Delivery>, Arc<ScriptedProvider>, PathBuf) {
    start_b_with_summary(session, id, from, note, worktree, script, None)
}

fn start_b_with_summary(
    session: &Session,
    id: &str,
    from: Point,
    note: String,
    worktree: Option<Worktree>,
    script: Vec<Scripted>,
    summary: Option<String>,
) -> (Loop, mpsc::Sender<Delivery>, Arc<ScriptedProvider>, PathBuf) {
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock = session.clock.clone();
    let log = Arc::new(Log::create(&home, SessionId(id.into()), clock.clone()).unwrap());
    let dir = log.dir().to_path_buf();
    let provider = Arc::new(ScriptedProvider::new(script));
    let (tx, rx) = mpsc::channel();
    let clock: Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = PromptInputs::new(
        home,
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let tools: Vec<(String, Arc<dyn Tool>)> = vec![
        (
            "builtin".to_owned(),
            Arc::clone(&write_tool()) as Arc<dyn Tool>,
        ),
        (
            "builtin".to_owned(),
            Arc::clone(&exec_tool()) as Arc<dyn Tool>,
        ),
    ];
    let rules: Arc<dyn contract::rules::Rules> = session.rules.clone();
    let b = Loop::rewound(
        r#loop::Session {
            log,
            provider: provider.clone(),
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools,
            permissions: Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules,
            },
        },
        Rewound {
            factory: None,
            from,
            worktree,
            how: Continuation::Rewind {
                note,
                summary,
                name: r#loop::latest_name(&session.dir),
            },
        },
    )
    .unwrap();
    (b, tx, provider, dir)
}

/// Runs one turn of `b` for the prompt `text`, failing at one deadline
/// instead of hanging.
#[track_caller]
fn run_turn(b: Loop, tx: &mpsc::Sender<Delivery>, text: &str) -> Loop {
    tx.send(delivery(text)).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut b = b;
        let outcome = b.turn().unwrap();
        done.send((b, outcome)).unwrap();
    });
    let (b, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    b
}

fn session_started(lines: &[Envelope]) -> &Envelope {
    lines
        .iter()
        .find(|line| line.kind == "session_started")
        .unwrap()
}

#[test]
fn a_rewound_session_holds_only_its_own_first_line_before_its_prompt() {
    let session = run_a();
    let lines = log::read(&session.dir).unwrap();
    let at = point(&lines);
    let note = rewind_note(&session.dir, Seq(at)).unwrap();
    let (_b, _tx, _provider, dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note.clone(),
        None,
        vec![Scripted::text("three-done")],
    );
    let lines = log::read(&dir).unwrap();
    assert_eq!(kinds(&lines), ["session_started"]);
    let first = session_started(&lines);
    assert_eq!(
        first.payload["forked_from"],
        json!({"session_id": "s_test", "seq": at})
    );
    assert!(first.payload["rewind"].get("jobs").is_none());
    assert_eq!(first.payload["rewind"]["note"], json!(note));
    assert!(first.payload.get("parent").is_none());
    assert!(first.payload["rewind"].get("summary").is_none());
    assert!(
        lines
            .iter()
            .all(|line| line.session_id == SessionId(B_ID.into()))
    );
}

#[test]
fn a_job_the_old_session_left_running_is_no_orphan_in_the_new_one() {
    // The rewind stopped the old session's jobs, so a `job_started` the old
    // log never ended writes no orphaned `job_completed` there.
    use contract::events::JobStarted;
    let session = run_a();
    session
        .log
        .append(
            &contract::events::Event::JobStarted(JobStarted {
                job_id: contract::JobId("j_orphan".into()),
                tool: None,
                extension: None,
                description: "a running job".into(),
                output_path: "jobs/j_orphan".into(),
            }),
            None,
            None,
        )
        .unwrap();
    let lines = log::read(&session.dir).unwrap();
    let at = lines.last().unwrap().seq.unwrap().0;
    let (b, tx, provider, dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        String::new(),
        None,
        vec![Scripted::text("three-done")],
    );
    let lines = log::read(&dir).unwrap();
    assert_eq!(kinds(&lines), ["session_started"]);
    let _b = run_turn(b, &tx, "three");
    let lines = log::read(&dir).unwrap();
    assert!(
        lines.iter().all(|line| line.kind != "job_completed"),
        "no orphaned completion is written"
    );
    let first = &provider.requests()[0];
    assert!(
        first.conversation.iter().all(|input| match input {
            Input::User { text, .. } => !text.contains("j_orphan"),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => true,
        }),
        "no job notice reaches the model"
    );
}

#[test]
fn a_handoff_started_in_the_new_session_after_a_rewind_names_no_job() {
    // A job started before the point with no end in the window would read
    // as running in the new session's handoff text: the rewind stopped it
    // in the old log, so the new session's handoff carry holds no job.
    let session = run_a();
    session
        .log
        .append(
            &contract::events::Event::JobStarted(contract::events::JobStarted {
                job_id: contract::JobId("j_win".into()),
                tool: None,
                extension: None,
                description: "a job in the window".into(),
                output_path: "jobs/j_win".into(),
            }),
            None,
            None,
        )
        .unwrap();
    let lines = log::read(&session.dir).unwrap();
    let at = lines.last().unwrap().seq.unwrap().0;
    let (b, tx, provider, dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        String::new(),
        None,
        vec![Scripted::text("The note."), Scripted::text("Next.")],
    );
    tx.send(Delivery::Handoff(
        CommandId("c_h".into()),
        contract::commands::Handoff { instructions: None },
        ignore(),
    ))
    .unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut b = b;
        let outcome = b.turn().unwrap();
        done.send((b, outcome)).unwrap();
    });
    let (b, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the handoff turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    let lines = log::read(&dir).unwrap();
    assert!(
        lines.iter().any(|line| line.kind == "handoff_completed"),
        "the handoff ran"
    );
    // The next turn sends the restarted conversation: the carried input,
    // the note, and no line for a job.
    let b = run_turn(b, &tx, "next");
    drop(b);
    assert!(
        provider.requests().len() >= 2,
        "the next turn sent a request"
    );
    for request in provider.requests().iter().skip(1) {
        for input in request.conversation.iter() {
            if let Input::User { text, .. } = input {
                assert!(!text.contains("j_win"), "a stopped job is named: {text}");
            }
        }
    }
}

#[test]
fn a_rewound_sessions_first_request_matches_the_olds_at_the_point() {
    let session = run_a();
    let a_lines = log::read(&session.dir).unwrap();
    let at = point(&a_lines);
    let note = rewind_note(&session.dir, Seq(at)).unwrap();
    let a_requests = session.provider.requests();
    let a_second = &a_requests[1];
    // Turn 2's input is the last input of its first request: everything
    // before it is the history the rewind keeps.
    let k = a_second.conversation.len() - 1;
    let (b, tx, provider, dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note.clone(),
        None,
        vec![Scripted::text("three-done")],
    );
    let _b = run_turn(b, &tx, "three");
    let b_requests = provider.requests();
    assert_eq!(b_requests.len(), 1);
    let first = &b_requests[0];
    assert_eq!(first.system_prompt, a_second.system_prompt);
    assert_eq!(first.thinking, a_second.thinking);
    assert_eq!(first.tool_choice, a_second.tool_choice);
    assert_eq!(first.cache_lifetime, a_second.cache_lifetime);
    assert_eq!(first.cache_key, "s_test");
    assert_eq!(first.previous_end, a_second.previous_end);
    assert_eq!(first.tools, a_second.tools);
    // The sent tools are the logged build, verbatim.
    let built = a_lines
        .iter()
        .find(|line| line.kind == "preamble_built")
        .unwrap();
    let logged: Vec<Value> = built.payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["definition"].clone())
        .collect();
    let sent: Vec<Value> = first
        .sent_tools
        .clone()
        .unwrap()
        .into_iter()
        .map(Value::Object)
        .collect();
    assert_eq!(sent, logged);
    // The history to the point, then the note, then the new prompt.
    assert_eq!(&first.conversation[..k], &a_second.conversation[..k]);
    assert_eq!(
        first.conversation[k],
        Input::User {
            text: note,
            images: Vec::new(),
        }
    );
    assert!(matches!(
        &first.conversation[k + 1],
        Input::User { text, .. } if text == "three"
    ));
    // The new session builds nothing of its own.
    let lines = log::read(&dir).unwrap();
    assert!(
        lines
            .iter()
            .all(|line| line.kind != "preamble_built" && line.kind != "opening_message")
    );
}

#[test]
fn a_summary_is_one_turn_on_the_old_session_before_rewound() {
    let session = run_a_with_tail(vec![Scripted::text("Tried X.")]);
    let before = log::read(&session.dir).unwrap();
    let requests = session.provider.requests();
    let last = requests.last().unwrap();
    let (command, answer) = rewind(args(None, None, true));
    session.inbox.send(command).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut session = session;
        let result = session.looped.as_mut().unwrap().turn();
        done.send((session, result)).unwrap();
    });
    let (session, result) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the summary ended");
    assert_eq!(result.unwrap(), None);
    assert!(Deadline::after(DEADLINE).recv(&answer).unwrap().is_ok());
    let lines = log::read(&session.dir).unwrap();
    let tail = &lines[before.len()..];
    assert_eq!(
        kinds(tail),
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "rewound"
        ]
    );
    assert_eq!(
        tail[0].payload["input"],
        json!([{"type":"rewind","command_id":"c_rewind"}])
    );
    assert_eq!(tail[6].payload["outcome"], "completed");
    assert_eq!(tail[7].payload["summary"], "Tried X.");
    let after = session.provider.requests();
    assert_eq!(after.len(), requests.len() + 1);
    let request = after.last().unwrap();
    let mut expected = last.conversation.to_vec();
    expected.push(Input::Assistant {
        model: MODEL.into(),
        text: "two-done".into(),
        provider_item: None,
    });
    assert_eq!(&request.conversation[..expected.len()], &expected[..]);
    assert_eq!(request.conversation.len(), expected.len() + 1);
    assert!(
        matches!(request.conversation.last().unwrap(), Input::User { text, .. } if text.contains("two"))
    );
    assert_eq!(request.system_prompt, last.system_prompt);
    assert_eq!(request.sent_tools, last.sent_tools);
    assert_eq!(request.cache_key, last.cache_key);
    assert_eq!(request.max_output_tokens, None);
}

#[test]
fn a_summary_follows_history_before_the_note_live_and_resumed() {
    let session = run_a();
    let lines = log::read(&session.dir).unwrap();
    let at = point(&lines);
    let note = rewind_note(&session.dir, Seq(at)).unwrap();
    let history = session.provider.requests()[1].conversation.clone();
    let k = history.len() - 1;
    let (b, tx, provider, dir) = start_b_with_summary(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note.clone(),
        None,
        vec![Scripted::text("three-done")],
        Some("the summary".into()),
    );
    let logged = log::read(&dir).unwrap();
    assert_eq!(kinds(&logged), ["session_started"]);
    assert_eq!(logged[0].payload["rewind"]["summary"], "the summary");
    let b = run_turn(b, &tx, "three");
    let request = &provider.requests()[0];
    assert_eq!(&request.conversation[..k], &history[..k]);
    for (index, text) in [(k, "the summary"), (k + 1, note.as_str()), (k + 2, "three")] {
        assert_eq!(
            request.conversation[index],
            Input::User {
                text: text.into(),
                images: vec![]
            }
        );
    }
    drop(b);
    let home = dir.parent().unwrap().to_path_buf();
    let log = Arc::new(Log::open(&home, SessionId(B_ID.into()), session.clock.clone()).unwrap());
    let resumed_provider = Arc::new(ScriptedProvider::new(vec![Scripted::text("four-done")]));
    let (tx, rx) = mpsc::channel();
    let b = Loop::resume(
        r#loop::Session {
            log,
            provider: resumed_provider.clone(),
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt: PromptInputs::new(
                home,
                "/bin/sh".into(),
                dir.join("events.jsonl").display().to_string(),
                session.clock.clone(),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: vec![],
            permissions: Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: vec![],
                rules: session.rules.clone(),
            },
        },
        r#loop::resumed(&dir).unwrap(),
    )
    .unwrap();
    let _b = run_turn(b, &tx, "four");
    assert_eq!(
        &resumed_provider.requests()[0].conversation[..request.conversation.len()],
        &request.conversation[..]
    );
}

#[test]
fn the_note_names_what_the_session_did_after_the_point() {
    let session = run_a();
    let lines = log::read(&session.dir).unwrap();
    let note = rewind_note(&session.dir, Seq(point(&lines))).unwrap();
    assert!(note.contains("/w/note.txt"));
    assert!(note.contains("run_cmd"));
}

#[test]
fn a_rewound_session_records_the_worktree_it_is_given() {
    let session = run_a();
    let lines = log::read(&session.dir).unwrap();
    let at = point(&lines);
    let from = || Point {
        session_id: SessionId("s_test".into()),
        seq: Seq(at),
    };
    let worktree = Worktree {
        path: "/w/tree".into(),
        branch: "fiber/s_test".into(),
        repository: Some("/r/.git".into()),
    };
    let (_b, _tx, _provider, dir) = start_b(
        &session,
        "s_given00000001",
        from(),
        String::new(),
        Some(worktree.clone()),
        vec![Scripted::text("three-done")],
    );
    let read_b = log::read(&dir).unwrap();
    let first = session_started(&read_b);
    assert_eq!(
        first.payload["worktree"],
        json!({"path": "/w/tree", "branch": "fiber/s_test", "repository": "/r/.git"})
    );
    let (_b, _tx, _provider, dir) = start_b(
        &session,
        "s_given00000002",
        from(),
        String::new(),
        None,
        vec![Scripted::text("three-done")],
    );
    assert!(
        session_started(&log::read(&dir).unwrap())
            .payload
            .get("worktree")
            .is_none()
    );
}

#[test]
fn a_resumed_rewound_session_keeps_its_history_note_and_key() {
    let session = run_a();
    let a_lines = log::read(&session.dir).unwrap();
    let at = point(&a_lines);
    let note = rewind_note(&session.dir, Seq(at)).unwrap();
    let (b, tx, provider, dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note.clone(),
        None,
        vec![Scripted::text("three-done")],
    );
    let b = run_turn(b, &tx, "three");
    drop(b);
    let first = &provider.requests()[0];
    let folded = r#loop::resumed(&dir).unwrap();
    assert_eq!(folded.root, "s_test");
    assert_eq!(folded.session, B_ID);
    // A fresh loop over the resumed chain sends the history, the note and
    // the turn, under the root's key.
    let home = dir.parent().unwrap().to_path_buf();
    let clock = session.clock.clone();
    let log = Arc::new(Log::open(&home, SessionId(B_ID.into()), clock.clone()).unwrap());
    let next_provider = Arc::new(ScriptedProvider::new(vec![Scripted::text("four-done")]));
    let (tx, rx) = mpsc::channel();
    let clock: Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = PromptInputs::new(
        home,
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let rules: Arc<dyn contract::rules::Rules> = session.rules.clone();
    let b = Loop::resume(
        r#loop::Session {
            log,
            provider: next_provider.clone(),
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: vec![
                (
                    "builtin".to_owned(),
                    Arc::clone(&write_tool()) as Arc<dyn Tool>,
                ),
                (
                    "builtin".to_owned(),
                    Arc::clone(&exec_tool()) as Arc<dyn Tool>,
                ),
            ],
            permissions: Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules,
            },
        },
        folded,
    )
    .unwrap();
    let _b = run_turn(b, &tx, "four");
    let resumed_first = &next_provider.requests()[0];
    assert_eq!(resumed_first.cache_key, "s_test");
    assert_eq!(
        &resumed_first.conversation[..first.conversation.len()],
        &first.conversation[..]
    );
    drop(session);
}

#[test]
fn a_rewind_of_a_rewind_folds_both_points_in_order() {
    let session = run_a();
    let a_lines = log::read(&session.dir).unwrap();
    let at = point(&a_lines);
    let note_ab = rewind_note(&session.dir, Seq(at)).unwrap();
    let (b, tx, _provider, b_dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note_ab.clone(),
        None,
        vec![Scripted::text("three-done")],
    );
    let _b = run_turn(b, &tx, "three");
    let b_lines = log::read(&b_dir).unwrap();
    let b_turn = b_lines
        .iter()
        .find(|line| line.kind == "turn_started")
        .unwrap()
        .seq
        .unwrap()
        .0;
    let note_bc = rewind_note(&b_dir, Seq(b_turn)).unwrap();
    let (c, tx, provider, _dir) = start_b(
        &session,
        "s_rewound00000002",
        Point {
            session_id: SessionId(B_ID.into()),
            seq: Seq(b_turn),
        },
        note_bc.clone(),
        None,
        vec![Scripted::text("four-done")],
    );
    let _c = run_turn(c, &tx, "four");
    let first = &provider.requests()[0];
    let texts: Vec<&str> = first
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.as_str()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();
    let mut ordered = texts.iter();
    assert!(ordered.any(|text| text.contains("one")));
    assert!(ordered.any(|text| *text == note_ab));
    assert!(ordered.any(|text| text.contains("three")));
    assert!(ordered.any(|text| *text == note_bc));
    drop(session);
}

#[test]
fn a_rewind_through_the_chain_holds_no_line_of_the_middle_session() {
    let session = run_a();
    let a_lines = log::read(&session.dir).unwrap();
    let at = point(&a_lines);
    let note_ab = rewind_note(&session.dir, Seq(at)).unwrap();
    let (b, tx, _provider, _b_dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note_ab,
        None,
        vec![Scripted::text("three-done")],
    );
    let _b = run_turn(b, &tx, "three");
    // C continues A at the same point, through B's chain: nothing of B.
    let note = rewind_note(&session.dir, Seq(at)).unwrap();
    let (c, tx, provider, dir) = start_b(
        &session,
        "s_rewound00000003",
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note.clone(),
        None,
        vec![Scripted::text("four-done")],
    );
    let _c = run_turn(c, &tx, "four");
    let first = &provider.requests()[0];
    let read_c = log::read(&dir).unwrap();
    let started = session_started(&read_c);
    assert_eq!(
        started.payload["forked_from"],
        json!({"session_id": "s_test", "seq": at})
    );
    let texts: Vec<&str> = first
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.as_str()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();
    assert!(texts.iter().any(|text| text.contains("one")));
    assert!(texts.iter().any(|text| *text == note));
    assert!(!texts.iter().any(|text| text.contains("three")));
    drop(session);
}

#[test]
fn an_image_in_the_inherited_history_reads_from_the_session_that_wrote_it() {
    let mut session = Session::with_tools(vec![Scripted::text("seen")], None, Vec::new());
    let bytes = b"fake-png-bytes";
    fs::create_dir_all(session.dir.join("artifacts")).unwrap();
    fs::write(session.dir.join("artifacts/i.png"), bytes).unwrap();
    session
        .inbox
        .send(Delivery::Prompt(
            contract::inbox::Message {
                content: vec![
                    ContentPart::Text { text: "one".into() },
                    ContentPart::Image {
                        path: "artifacts/i.png".into(),
                        mime_type: "image/png".into(),
                        width: 1,
                        height: 1,
                    },
                ],
                sender: Sender {
                    origin: Origin::Driver,
                    command_id: Some(CommandId("c_one".into())),
                },
            },
            ignore(),
        ))
        .unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = log::read(&session.dir).unwrap();
    let start = lines
        .iter()
        .find(|line| line.kind == "turn_started")
        .unwrap()
        .seq
        .unwrap()
        .0;
    let (b, tx, provider, dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(start),
        },
        String::new(),
        None,
        vec![Scripted::text("done")],
    );
    let _b = run_turn(b, &tx, "go");
    let first = &provider.requests()[0];
    let images: Vec<&contract::provider::ImageRef> = first
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { images, .. } | Input::ToolResult { images, .. } => {
                Some(images.as_slice())
            }
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .flatten()
        .collect();
    assert_eq!(images.len(), 1);
    let expected = session.dir.join("artifacts/i.png").display().to_string();
    assert_eq!(images[0].path, expected);
    assert_eq!(fs::read(&images[0].path).unwrap(), bytes);
    assert!(!dir.join("artifacts/i.png").exists());
    // The child's request carries the parent's file, keyed by the absolute
    // path the conversation holds, as the same base64.
    assert_eq!(&*first.media[&expected], STANDARD.encode(bytes));
    drop(session);
}

#[test]
fn a_pdf_in_the_inherited_history_reads_from_the_session_that_wrote_it() {
    let mut tool = TestTool::reads("cat", "PDF: 2 pages.\n");
    tool.output.content.push(ContentPart::Pdf(
        contract::shapes::PdfPart::new(
            "artifacts/p_3f2a9c0d1e4b5a67.pdf".into(),
            2,
            Some(vec![
                contract::shapes::ImagePart {
                    path: "artifacts/i_0a1b2c3d4e5f6071.png".into(),
                    mime_type: "image/png".into(),
                    width: 1545,
                    height: 2000,
                },
                contract::shapes::ImagePart {
                    path: "artifacts/i_8090a0b0c0d0e0f0.png".into(),
                    mime_type: "image/png".into(),
                    width: 1545,
                    height: 2000,
                },
            ]),
        )
        .unwrap(),
    ));
    let mut session = Session::with_tools(
        vec![
            calls_reply("working", &[("cat", paris())]),
            Scripted::text("done"),
        ],
        None,
        vec![Arc::new(tool)],
    );
    let pdf_bytes = b"%PDF-1.4 fake";
    fs::create_dir_all(session.dir.join("artifacts")).unwrap();
    fs::write(
        session.dir.join("artifacts/p_3f2a9c0d1e4b5a67.pdf"),
        pdf_bytes,
    )
    .unwrap();
    fs::write(
        session.dir.join("artifacts/i_0a1b2c3d4e5f6071.png"),
        b"page-one",
    )
    .unwrap();
    fs::write(
        session.dir.join("artifacts/i_8090a0b0c0d0e0f0.png"),
        b"page-two",
    )
    .unwrap();
    session.rules.set(StandingRules {
        global: vec![allow_rule("cat")],
        project: Vec::new(),
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let lines = log::read(&session.dir).unwrap();
    let at = lines
        .iter()
        .find(|line| line.kind == "tool_call_completed")
        .unwrap()
        .seq
        .unwrap()
        .0;
    let (b, tx, provider, dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        String::new(),
        None,
        vec![Scripted::text("done")],
    );
    let _b = run_turn(b, &tx, "go");
    let first = &provider.requests()[0];
    let pdfs: Vec<&contract::provider::PdfRef> = first
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::ToolResult { pdfs, .. } => Some(pdfs.as_slice()),
            Input::User { .. }
            | Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .flatten()
        .collect();
    assert_eq!(pdfs.len(), 1);
    let expected = session
        .dir
        .join("artifacts/p_3f2a9c0d1e4b5a67.pdf")
        .display()
        .to_string();
    assert_eq!(pdfs[0].path, expected);
    assert_eq!(pdfs[0].page_count, 2);
    let pages = pdfs[0].pages.as_ref().unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(
        pages[0].path,
        session
            .dir
            .join("artifacts/i_0a1b2c3d4e5f6071.png")
            .display()
            .to_string()
    );
    assert_eq!(fs::read(&pdfs[0].path).unwrap(), pdf_bytes);
    assert!(!dir.join("artifacts/p_3f2a9c0d1e4b5a67.pdf").exists());
    // The PDF and both rendered pages resolve to the parent's bytes.
    let media = |name: &str| first.media[&session.dir.join(name).display().to_string()].to_string();
    assert_eq!(
        media("artifacts/p_3f2a9c0d1e4b5a67.pdf"),
        STANDARD.encode(pdf_bytes)
    );
    assert_eq!(
        media("artifacts/i_0a1b2c3d4e5f6071.png"),
        STANDARD.encode(b"page-one")
    );
    assert_eq!(
        media("artifacts/i_8090a0b0c0d0e0f0.png"),
        STANDARD.encode(b"page-two")
    );
    drop(session);
}

#[test]
fn a_rewind_before_the_first_request_keeps_the_logged_model_and_thinking() {
    use contract::events::{
        CacheLifetime, PreambleBuilt, PreambleReason, SentTool, SessionStarted, Variables,
        VariablesSource,
    };
    let root = fakes::TempDir::new("fiber-rewind-model");
    let home = root.path().to_path_buf();
    let clock = fakes::clock::FakeClock::new();
    let a =
        Arc::new(Log::create(&home, SessionId("s_model00000001".into()), clock.clone()).unwrap());
    let definition = json!({
        "type": "function",
        "name": "get_weather",
        "description": "Weather.",
        "parameters": {"type": "object"},
        "strict": true,
    });
    a.append(
        &contract::events::Event::SessionStarted(SessionStarted {
            workspace: "/w".into(),
            variables: Variables {
                path: "/usr/bin".into(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: None,
            forked_from: None,
            rewind: None,
            worktree: None,
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    a.append(
        &contract::events::Event::PreambleBuilt(PreambleBuilt {
            reason: PreambleReason::Start,
            model: "fake/model-9".into(),
            context_window: fakes::CONTEXT_WINDOW,
            trigger_at: None,
            budget: None,
            thinking: Some("high".into()),
            tool_choice: "auto".into(),
            cache_lifetime: CacheLifetime::OneHour,
            credential: Some("work".into()),
            system_prompt: "SYS9".into(),
            tools: vec![SentTool {
                name: "get_weather".into(),
                registered_by: "builtin".into(),
                deferred: false,
                definition: definition.as_object().unwrap().clone(),
            }],
            replaced: Vec::new(),
        }),
        None,
        None,
    )
    .unwrap();
    let a_dir = a.dir().to_path_buf();
    let folded = r#loop::forked(&a_dir, Seq(1)).unwrap();
    assert_eq!(folded.model.as_deref(), Some("fake/model-9"));
    assert_eq!(folded.thinking.as_deref(), Some("high"));
    drop(a);
    // B starts from that build: its first request sends it verbatim.
    let clock_dyn: Arc<dyn contract::clock::Clock> = clock;
    let b_log = Arc::new(Log::create(&home, SessionId(B_ID.into()), clock_dyn.clone()).unwrap());
    let b_dir = b_log.dir().to_path_buf();
    let provider = Arc::new(ScriptedProvider::new(vec![Scripted::text("done")]));
    let (tx, rx) = mpsc::channel();
    let mut prompt = PromptInputs::new(
        home.clone(),
        "/bin/sh".into(),
        b_dir.join("events.jsonl").display().to_string(),
        clock_dyn,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let rules: Arc<dyn contract::rules::Rules> = Arc::new(support::FakeRules::empty());
    let b = Loop::rewound(
        r#loop::Session {
            log: b_log,
            provider: provider.clone(),
            model: Model {
                reference: "fake/model-9".into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: Vec::new(),
            permissions: Permissions {
                workspace: "/w".into(),
                credentials: home.clone(),
                credential_files: Vec::new(),
                rules,
            },
        },
        Rewound {
            factory: None,
            from: Point {
                session_id: SessionId("s_model00000001".into()),
                seq: Seq(1),
            },
            worktree: None,
            how: Continuation::Rewind {
                note: String::new(),
                summary: None,
                name: r#loop::latest_name(&a_dir),
            },
        },
    )
    .unwrap();
    let _b = run_turn(b, &tx, "go");
    let first = &provider.requests()[0];
    assert_eq!(first.system_prompt, "SYS9");
    assert_eq!(first.thinking, Some(contract::ThinkingLevel::High));
    let sent: Vec<Value> = first
        .sent_tools
        .clone()
        .unwrap()
        .into_iter()
        .map(Value::Object)
        .collect();
    assert_eq!(sent, vec![definition]);
}

const A_ID: &str = "s_aaaaaaaaaaaaaaaa";
const B_ID2: &str = "s_bbbbbbbbbbbbbbbb";

fn args(from: Option<&str>, seq: Option<u64>, summarise: bool) -> RewindArgs {
    RewindArgs {
        from_session_id: from.map(|id| SessionId(id.into())),
        seq: seq.map(Seq),
        summarise,
    }
}

#[track_caller]
fn answer_of(rx: mpsc::Receiver<Answer>) -> Answer {
    Deadline::after(DEADLINE)
        .recv(&rx)
        .expect("the rewind is answered in time")
}

fn accepted(answer: Answer) -> SessionId {
    match answer {
        Ok(Some(CommandResult::Rewind { new_session_id })) => new_session_id,
        other => panic!("the rewind is accepted, got {other:?}"),
    }
}

fn rejected(answer: Answer) -> Rejection {
    match answer {
        Err(rejection) => rejection,
        Ok(_) => panic!("the rewind is refused, got {answer:?}"),
    }
}

fn log_bytes(dir: &std::path::Path) -> Vec<u8> {
    fs::read(dir.join("events.jsonl")).unwrap()
}

fn assert_minted(id: &SessionId) {
    let hex = id.0.strip_prefix("s_").unwrap_or("");
    assert!(
        hex.len() == 16 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the new session id is minted: {}",
        id.0
    );
}

/// Runs one turn wait on `looped` on its own thread, after sending
/// `delivery`: the loop and the wait's outcome, failing at one named
/// deadline instead of hanging.
#[track_caller]
fn drive(
    mut looped: Loop,
    tx: &mpsc::Sender<Delivery>,
    delivery: Delivery,
) -> (Loop, Option<TurnOutcome>) {
    tx.send(delivery).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the wait ended in time")
}

/// Sends `batch` to an idle `looped` in order and ends the wait: every
/// refusal answers while the wait goes on, and `sender` (the only live
/// inbox sender) is dropped to end it with no turn. Returns the loop; the
/// test reads each answer on its own channel, failing at one named
/// deadline instead of hanging.
#[track_caller]
fn drive_closed(mut looped: Loop, sender: mpsc::Sender<Delivery>, batch: Vec<Delivery>) -> Loop {
    for delivery in batch {
        sender.send(delivery).unwrap();
    }
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    drop(sender);
    let (looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the wait ended in time");
    assert_eq!(outcome, None, "no turn starts");
    looped
}

/// Takes the session's inbox sender, leaving a dead one behind: dropping
/// the taken sender ends a driven wait, while the session stays alive for
/// its log and home.
fn take_inbox(session: &mut Session) -> mpsc::Sender<Delivery> {
    std::mem::replace(&mut session.inbox, mpsc::channel().0)
}

/// Session A after two turns, with `id` as its session id: a session id a
/// `from_session_id` can name.
fn run_a_with_id(id: &str) -> Session {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::clone(&write_tool()) as Arc<dyn Tool>,
        Arc::clone(&exec_tool()) as Arc<dyn Tool>,
    ];
    let mut session = Session::with_tools_and_id(
        vec![
            Scripted::text("one-done"),
            calls_reply("working", &[("write_file", paris()), ("run_cmd", paris())]),
            Scripted::text("two-done"),
        ],
        None,
        tools,
        SessionId(id.into()),
    );
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.rules.set(StandingRules {
        global: vec![allow_rule("write_file"), allow_rule("run_cmd")],
        project: Vec::new(),
    });
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session
}

/// A hand-built session beside A's, continuing `from`: its log holds
/// `session_started` and `fiber_started`, and its loop is resumed, as a new
/// process resumes it. The caller keeps `session` alive for A's log and
/// home.
fn resume_b(
    session: &Session,
    id: &str,
    from: Option<Point>,
    parent: Option<Parent>,
) -> (Loop, mpsc::Sender<Delivery>, PathBuf) {
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock = session.clock.clone();
    let log = Arc::new(Log::create(&home, SessionId(id.into()), clock.clone()).unwrap());
    let dir = log.dir().to_path_buf();
    log.append(
        &contract::events::Event::SessionStarted(SessionStarted {
            workspace: session.workspace.display().to_string(),
            variables: Variables {
                path: "/usr/bin:/bin".to_owned(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent,
            forked_from: from,
            rewind: None,
            worktree: None,
            supervised: false,
        }),
        None,
        None,
    )
    .unwrap();
    r#loop::fiber_started(&log, "0.0.1", true).unwrap();
    let folded = r#loop::resumed(&dir).unwrap();
    let (tx, rx) = mpsc::channel();
    let clock: Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = PromptInputs::new(
        home,
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let rules: Arc<dyn contract::rules::Rules> = session.rules.clone();
    let looped = Loop::resume(
        r#loop::Session {
            log,
            provider: Arc::new(ScriptedProvider::new(Vec::new())),
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: Vec::new(),
            permissions: Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules,
            },
        },
        folded,
    )
    .unwrap();
    (looped, tx, dir)
}

fn resume_existing(
    session: &Session,
    id: &str,
    script: Vec<Scripted>,
) -> (Loop, mpsc::Sender<Delivery>, Arc<ScriptedProvider>) {
    let home = session.dir.parent().unwrap().to_path_buf();
    let log = if id == "s_test" {
        session.log.clone()
    } else {
        Arc::new(Log::open(&home, SessionId(id.into()), session.clock.clone()).unwrap())
    };
    let dir = log.dir().to_path_buf();
    r#loop::fiber_started(&log, "0.0.1", true).unwrap();
    let provider = Arc::new(ScriptedProvider::new(script));
    let (tx, rx) = mpsc::channel();
    let mut prompt = PromptInputs::new(
        home,
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        session.clock.clone(),
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let looped = Loop::resume(
        r#loop::Session {
            log,
            provider: provider.clone(),
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: vec![
                ("builtin".into(), write_tool()),
                ("builtin".into(), exec_tool()),
            ],
            permissions: Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: vec![],
                rules: session.rules.clone(),
            },
        },
        r#loop::resumed(&dir).unwrap(),
    )
    .unwrap();
    (looped, tx, provider)
}

#[test]
fn an_ancestor_summary_keeps_the_entire_cut_path() {
    let session = run_a_with_id(A_ID);
    let lines = log::read(&session.dir).unwrap();
    let at = point(&lines);
    let (b, tx, _, dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId(A_ID.into()),
            seq: lines.last().unwrap().seq.unwrap(),
        },
        "B's note".into(),
        None,
        vec![Scripted::text("B replied")],
    );
    drop(run_turn(b, &tx, "B prompt"));
    let (b, tx, provider) = resume_existing(&session, B_ID, vec![Scripted::text("All the work.")]);
    let (rw, answer) = rewind(args(Some(A_ID), Some(at), true));
    drop(drive(b, &tx, rw));
    accepted(answer_of(answer));
    let request = &provider.requests()[0];
    let texts: Vec<&str> = request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } | Input::Assistant { text, .. } => Some(text.as_str()),
            Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();
    for text in [
        "one",
        "one-done",
        "two",
        "two-done",
        "B's note",
        "B prompt",
        "B replied",
    ] {
        assert!(
            texts.contains(&text),
            "the cut path contains {text}: {texts:?}"
        );
    }
    assert!(texts.last().unwrap().contains("two"));
    assert_eq!(
        log::read(&dir).unwrap().last().unwrap().payload["summary"],
        "All the work."
    );
}

#[test]
fn an_ancestor_summary_starts_its_excerpt_after_the_inherited_last_line() {
    let session = run_a_with_id(A_ID);
    let lines = log::read(&session.dir).unwrap();
    // The inherited segment ends on the person's input, immediately before
    // step_started. Moving the cut back even one line quotes that input.
    let at = turn_start(&lines, 2);
    let (b, tx, _, _) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId(A_ID.into()),
            seq: Seq(at),
        },
        "B's note".into(),
        None,
        vec![Scripted::text("B replied")],
    );
    drop(run_turn(b, &tx, "B prompt"));
    let (b, tx, provider) = resume_existing(&session, B_ID, vec![Scripted::text("The path.")]);
    let (rw, answer) = rewind(args(Some(A_ID), Some(at), true));
    drop(drive(b, &tx, rw));
    accepted(answer_of(answer));
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert!(request.conversation.contains(&Input::User {
        text: "two".into(),
        images: vec![],
    }));
    assert!(
        matches!(request.conversation.last().unwrap(), Input::User { text, .. }
            if text.contains("B prompt") && !text.contains("two")),
        "the summary quotes B's first message, not the inherited input: {:?}",
        request.conversation.last()
    );
}

#[test]
fn a_summary_reads_through_a_handoff_without_its_note_request() {
    let session = run_a_handed();
    let lines = log::read(&session.dir).unwrap();
    let at = turn_start(&lines, 1) - 1;
    let (b, tx, before, _) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        "the note".into(),
        None,
        vec![Scripted::text("B")],
    );
    drop(run_turn(b, &tx, "B prompt"));
    let prefix = before.requests()[0].conversation.clone();
    let (looped, tx, provider) = resume_existing(
        &session,
        "s_test",
        vec![Scripted::text("Across the handoff.")],
    );
    let (rw, answer) = rewind(args(None, Some(at), true));
    drop(drive(looped, &tx, rw));
    accepted(answer_of(answer));
    let request = &provider.requests()[0];
    assert_eq!(
        &request.conversation[..prefix.len() - 2],
        &prefix[..prefix.len() - 2]
    );
    let texts: Vec<&str> = request
        .conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } | Input::Assistant { text, .. } => Some(text.as_str()),
            Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect();
    for text in ["hi", "working", "Done.", "again", "Again."] {
        assert!(texts.contains(&text), "{text}: {texts:?}");
    }
    assert!(!texts.contains(&"The note."));
    assert_eq!(
        request
            .conversation
            .iter()
            .filter(|input| matches!(input, Input::User { .. }))
            .count(),
        4
    );
}

struct SummaryHooks {
    base: Arc<support::CountingHooks>,
    refuse: Mutex<bool>,
    context: bool,
    models: Mutex<Vec<(String, usize)>>,
    provider: Arc<ScriptedProvider>,
}

macro_rules! summary_hook_delegate {
    ($name:ident, $input:ident, $output:ident) => {
        fn $name(&self, input: &contract::hook::$input<'_>) -> contract::hook::$output {
            contract::hook::Hooks::$name(self.base.as_ref(), input)
        }
    };
}

impl contract::hook::Hooks for SummaryHooks {
    summary_hook_delegate!(after_tool, AfterToolCall, AfterToolAnswer);
    summary_hook_delegate!(before_tool, BeforeToolCall, BeforeToolAnswer);
    summary_hook_delegate!(check_tool, BeforeToolCall, CheckAnswer);
    summary_hook_delegate!(before_message, BeforeMessage, BeforeMessageAnswer);
    summary_hook_delegate!(session_start, SessionStart, ContextAnswer);
    summary_hook_delegate!(turn_end, TurnEnd, ContextAnswer);
    summary_hook_delegate!(before_handoff, BeforeHandoff, BeforeHandoffAnswer);

    fn turn_start(&self, turn: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        if self.context
            && turn
                .input
                .iter()
                .any(|item| matches!(item, contract::events::InputItem::Rewind { .. }))
        {
            support::start_added("summary-hook", "Only the summary sees this context.")
        } else {
            support::start_empty()
        }
    }

    fn before_model_call(
        &self,
        call: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        self.models
            .lock()
            .unwrap()
            .push((call.model.into(), self.provider.requests().len()));
        let outcome = if std::mem::take(&mut *self.refuse.lock().unwrap()) {
            contract::hook::CheckOutcome::Refused(contract::hook::Refusal {
                extension: "summary-hook".into(),
                reason: "not allowed".into(),
            })
        } else {
            contract::hook::CheckOutcome::Passed
        };
        contract::hook::CheckAnswer {
            outcome,
            notices: vec![],
        }
    }

    fn deliver_to(&self, _: mpsc::Sender<Delivery>) {}
}

#[test]
fn summary_hooks_see_the_request_once_and_context_stays_inside_the_summary() {
    let mut session = run_a_with_tail(vec![Scripted::text("  "), Scripted::text("next")]);
    let hooks = Arc::new(SummaryHooks {
        base: support::CountingHooks::new(),
        refuse: Mutex::new(false),
        context: true,
        models: Mutex::new(vec![]),
        provider: session.provider.clone(),
    });
    session = session.hooks(hooks.clone());
    let (rw, answer) = rewind(args(None, None, true));
    session.inbox.send(rw).unwrap();
    session.inbox.send(delivery("next")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(rejected(answer_of(answer)).code, ErrorCode::SummaryFailed);
    assert_eq!(
        *hooks.models.lock().unwrap(),
        [(MODEL.into(), 3), (MODEL.into(), 4)]
    );
    let requests = session.provider.requests();
    let context = Input::User {
        text: "Only the summary sees this context.".into(),
        images: vec![],
    };
    assert!(requests[3].conversation.contains(&context));
    assert!(!requests[4].conversation.contains(&context));
}

#[test]
fn a_budget_or_hook_stop_sends_no_summary_request() {
    for budget in [true, false] {
        let mut session = run_a_with_tail(vec![Scripted::text("next")]);
        let looped = session.looped.take().unwrap();
        let looped = if budget {
            looped.budget(Some(0.0))
        } else {
            looped.hooks(Arc::new(SummaryHooks {
                base: support::CountingHooks::new(),
                refuse: Mutex::new(true),
                context: false,
                models: Mutex::new(vec![]),
                provider: session.provider.clone(),
            }))
        };
        let before = log::read(&session.dir).unwrap().len();
        let (rw, answer) = rewind(args(None, None, true));
        let tx = take_inbox(&mut session);
        drop(drive_closed(looped, tx, vec![rw]));
        let rejection = rejected(answer_of(answer));
        assert_eq!(rejection.code, ErrorCode::SummaryFailed);
        assert!(
            rejection
                .message
                .starts_with("The summary could not be made: ")
        );
        assert!(rejection.message.contains(if budget {
            "spending budget"
        } else {
            "summary-hook"
        }));
        assert_eq!(session.provider.requests().len(), 3);
        let lines = log::read(&session.dir).unwrap();
        assert_eq!(
            kinds(&lines[before..]),
            ["turn_started", "step_started", "turn_completed"]
        );
        assert_eq!(lines.last().unwrap().payload["outcome"], "failed");
        assert_eq!(
            lines.last().unwrap().payload["error"]["code"],
            "budget_exceeded"
        );
        assert!(lines.iter().all(|line| line.kind != "rewound"));
        let (looped, tx, provider) =
            resume_existing(&session, "s_test", vec![Scripted::text("next")]);
        drop(run_turn(looped, &tx, "next"));
        assert_eq!(provider.requests().len(), 1);
    }
}

fn summary_failure(message: &str) -> contract::shapes::Failure {
    contract::shapes::Failure {
        code: ErrorCode::InvalidRequest,
        message: message.into(),
        retry_after_ms: None,
        provider: None,
    }
}

#[test]
fn failed_summaries_keep_usage_but_never_change_the_next_conversation() {
    let mut truncated = Scripted::text("cut");
    truncated.end.as_mut().unwrap().finish = contract::provider::Finish::OutputLimit;
    let cases = [
        (
            Scripted::failed_after(
                summary_failure("bad summary request"),
                fakes::call_usage("failed"),
            ),
            "bad summary request",
            "failed",
            vec![
                "assistant_message_started",
                "usage_recorded",
                "assistant_message_completed",
            ],
        ),
        (
            Scripted::text("  "),
            "the summary reply held no text.",
            "completed",
            vec![
                "assistant_message_started",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
            ],
        ),
        (
            calls_reply("Do not keep this", &[("write_file", paris())]),
            "the summary reply made tool calls.",
            "completed",
            vec![
                "assistant_message_started",
                "text_completed",
                "tool_call_requested",
                "usage_recorded",
                "assistant_message_completed",
                "tool_call_completed",
            ],
        ),
        (
            truncated,
            "the summary reached the output limit.",
            "completed",
            vec![
                "assistant_message_started",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
            ],
        ),
        (
            Scripted::cancelled_after(fakes::call_usage("cancelled")),
            "the summary was interrupted.",
            "interrupted",
            vec!["assistant_message_started", "usage_recorded"],
        ),
    ];
    for (script, reason, outcome, body) in cases {
        let mut session = run_a_with_tail(vec![script, Scripted::text("after")]);
        let before = log::read(&session.dir).unwrap().len();
        let last = session.provider.requests().last().unwrap().clone();
        let (rw, answer) = rewind(args(None, None, true));
        session.inbox.send(rw).unwrap();
        session.inbox.send(delivery("next")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let rejection = rejected(answer_of(answer));
        assert_eq!(rejection.code, ErrorCode::SummaryFailed);
        assert_eq!(
            rejection.message,
            format!("The summary could not be made: {reason}")
        );
        let lines = log::read(&session.dir).unwrap();
        let summary_end = before + 2 + body.len();
        let mut expected = vec!["turn_started", "step_started"];
        expected.extend(body);
        expected.push("turn_completed");
        assert_eq!(kinds(&lines[before..=summary_end]), expected);
        assert_eq!(lines[summary_end].payload["outcome"], outcome);
        assert!(lines.iter().all(|line| line.kind != "rewound"));
        let mut conversation = last.conversation.to_vec();
        conversation.push(Input::Assistant {
            model: MODEL.into(),
            text: "two-done".into(),
            provider_item: None,
        });
        conversation.push(Input::User {
            text: "next".into(),
            images: vec![],
        });
        assert_eq!(
            &session.provider.requests().last().unwrap().conversation[..],
            &conversation[..]
        );
        assert_eq!(
            session.provider.requests().last().unwrap().previous_end,
            Some(last.conversation.len())
        );
        let tool = lines[before..=summary_end]
            .iter()
            .find(|line| line.kind == "tool_call_completed");
        if let Some(tool) = tool {
            assert_eq!(tool.payload["status"], "cancelled");
        }
        assert!(
            lines[before..=summary_end]
                .iter()
                .all(|line| line.kind != "tool_call_started")
        );
    }
}

#[test]
fn a_failed_summary_keeps_the_default_retry_point() {
    let mut session = run_a_with_tail(vec![Scripted::failed(summary_failure("summary failed"))]);
    let at = point(&log::read(&session.dir).unwrap());
    let (rw, answer) = rewind(args(None, None, true));
    let (retry, retried) = rewind(args(None, None, false));
    session.inbox.send(rw).unwrap();
    session.inbox.send(retry).unwrap();
    assert_eq!(session.turn(), None);
    assert_eq!(rejected(answer_of(answer)).code, ErrorCode::SummaryFailed);
    accepted(answer_of(retried));
    assert_eq!(
        log::read(&session.dir).unwrap().last().unwrap().payload["seq"],
        at
    );
}

#[test]
fn failed_summary_tool_calls_are_absent_on_resume_too() {
    let mut session = run_a_with_tail(vec![
        calls_reply("discard me", &[("write_file", paris())]),
        Scripted::text("after"),
    ]);
    let (rw, answer) = rewind(args(None, None, true));
    session.inbox.send(rw).unwrap();
    session.inbox.send(delivery("next")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(rejected(answer_of(answer)).code, ErrorCode::SummaryFailed);
    let live = session.provider.requests().last().unwrap().clone();
    let (looped, tx, provider) =
        resume_existing(&session, "s_test", vec![Scripted::text("resumed")]);
    drop(run_turn(looped, &tx, "again"));
    let request = &provider.requests()[0];
    assert_eq!(
        &request.conversation[..live.conversation.len()],
        &live.conversation[..]
    );
    assert_eq!(request.previous_end, Some(live.conversation.len()));
}

#[test]
fn a_failed_summary_does_not_move_the_resumed_cache_boundary() {
    let mut session = run_a_with_tail(vec![calls_reply("discard", &[("write_file", paris())])]);
    let last = session.provider.requests().last().unwrap().clone();
    let (rw, answer) = rewind(args(None, None, true));
    let looped = session.looped.take().unwrap();
    let tx = take_inbox(&mut session);
    drop(drive_closed(looped, tx, vec![rw]));
    assert_eq!(rejected(answer_of(answer)).code, ErrorCode::SummaryFailed);
    let (looped, tx, provider) = resume_existing(&session, "s_test", vec![Scripted::text("after")]);
    drop(run_turn(looped, &tx, "next"));
    let request = &provider.requests()[0];
    assert_eq!(request.previous_end, Some(last.conversation.len()));
    let mut expected = last.conversation.to_vec();
    expected.push(Input::Assistant {
        model: MODEL.into(),
        text: "two-done".into(),
        provider_item: None,
    });
    expected.push(Input::User {
        text: "next".into(),
        images: vec![],
    });
    assert_eq!(&request.conversation[..], &expected[..]);
}

#[test]
fn a_failed_summary_reuses_its_new_preamble_at_the_next_prompt() {
    let session = run_a();
    let before = log::read(&session.dir).unwrap().len();
    let (mut looped, tx, provider) = resume_existing(
        &session,
        "s_test",
        vec![
            Scripted::failed(summary_failure("failed")),
            Scripted::text("after"),
        ],
    );
    let (rw, answer) = rewind(args(None, None, true));
    tx.send(rw).unwrap();
    tx.send(delivery("next")).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    let (_looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the prompt after the failed summary ended");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(rejected(answer_of(answer)).code, ErrorCode::SummaryFailed);
    assert_eq!(provider.requests().len(), 2);
    let lines = log::read(&session.dir).unwrap();
    assert_eq!(
        lines[before..]
            .iter()
            .filter(|line| line.kind == "preamble_built")
            .count(),
        1
    );
}

#[test]
fn the_reviewer_sees_no_failed_summary_live_or_resumed() {
    let mut expected = None;
    for mode in ["without", "live", "resumed"] {
        let mut tail = vec![];
        if mode != "without" {
            tail.push(calls_reply("summary prose", &[("write_file", paris())]));
        }
        if mode != "resumed" {
            tail.extend([
                calls_reply("next", &[("run_cmd", paris())]),
                Scripted::text("done"),
            ]);
        }
        let mut session = run_a_with_tail(tail);
        session.rules.set(StandingRules::default());
        let mut answer = None;
        if mode != "without" {
            let (rw, rx) = rewind(args(None, None, true));
            answer = Some(rx);
            if mode == "resumed" {
                let looped = session.looped.take().unwrap();
                let tx = take_inbox(&mut session);
                drop(drive_closed(looped, tx, vec![rw]));
                let (looped, tx, provider) = resume_existing(
                    &session,
                    "s_test",
                    vec![
                        calls_reply("next", &[("run_cmd", paris())]),
                        Scripted::text("done"),
                    ],
                );
                session.looped = Some(looped);
                session.inbox = tx;
                session.provider = provider;
            } else {
                session.inbox.send(rw).unwrap();
            }
        }
        let reviewer = session.reviewer(vec![Scripted::text("allow")]);
        session.inbox.send(delivery("next prompt")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        if let Some(answer) = answer {
            assert_eq!(rejected(answer_of(answer)).code, ErrorCode::SummaryFailed);
        }
        let requests = reviewer.requests();
        assert_eq!(requests.len(), 1);
        let replay = support::review::replay(
            &support::review::ReviewArgs {
                dir: session.dir.clone(),
                notes: None,
                keep_all: false,
            },
            std::io::Cursor::new(Vec::<u8>::new()),
            Vec::<u8>::new(),
        )
        .unwrap();
        assert_eq!(replay.requests.len(), 2);
        assert_eq!(replay.requests[0].1.conversation, requests[0].conversation);
        let mut conversation = requests[0].conversation.to_vec();
        for input in &mut conversation {
            if let Input::User { text, .. } = input {
                *text = text.replace(session.workspace.to_str().unwrap(), "<workspace>");
            }
        }
        match &expected {
            None => expected = Some(conversation),
            Some(expected) => assert_eq!(&conversation, expected, "{mode}"),
        }
    }
}

#[test]
fn a_crashed_summary_window_closes_for_the_reviewer_and_conversation() {
    let mut session = run_a_with_tail(vec![
        calls_reply("next reply", &[("run_cmd", paris())]),
        Scripted::text("done"),
    ]);
    let last = session.provider.requests().last().unwrap().clone();
    let looped = session.looped.as_mut().unwrap();
    let turn = contract::TurnId("t_summary_crashed".into());
    let usage = log::read(&session.dir)
        .unwrap()
        .iter()
        .rev()
        .find(|line| line.kind == "usage_recorded")
        .and_then(|line| contract::events::Event::from_envelope(line).unwrap())
        .unwrap();
    let events = [
        contract::events::Event::TurnStarted(contract::events::TurnStarted {
            input: vec![contract::events::InputItem::Rewind {
                command_id: CommandId("crash".into()),
            }],
        }),
        contract::events::Event::TextCompleted(contract::events::TextCompleted {
            text: "incomplete summary".into(),
            provider_item: None,
        }),
        usage,
        contract::events::Event::FiberStarted(contract::events::FiberStarted {
            version: "0.0.1".into(),
            resumed: true,
        }),
    ];
    for event in events {
        looped.write(&event, Some(&turn), None).unwrap();
    }
    session.rules.set(StandingRules::default());
    let reviewer = session.reviewer(vec![Scripted::text("allow")]);
    session.inbox.send(delivery("after crash")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let mut live = reviewer.requests()[0].conversation.to_vec();
    // The last two messages describe only the current call and review stage.
    live.truncate(live.len() - 2);
    assert!(
        live.iter()
            .any(|input| matches!(input, Input::User { text, .. } if text.contains("after crash")))
    );
    assert!(live.iter().all(
        |input| !matches!(input, Input::User { text, .. } if text.contains("incomplete summary"))
    ));
    let request = &session.provider.requests()[3];
    assert_eq!(
        &request.conversation[..last.conversation.len()],
        &last.conversation[..]
    );
    assert!(request.conversation.iter().all(
        |input| !matches!(input, Input::Assistant { text, .. } if text == "incomplete summary")
    ));
    let (looped, tx, _) = resume_existing(
        &session,
        "s_test",
        vec![
            calls_reply("later", &[("write_file", paris())]),
            Scripted::text("done"),
        ],
    );
    session.looped = Some(looped);
    session.inbox = tx;
    let reviewer = session.reviewer(vec![Scripted::text("allow")]);
    session.inbox.send(delivery("later prompt")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let rebuilt = reviewer.requests()[0].conversation.clone();
    assert_eq!(&rebuilt[..live.len()], &live[..]);
    assert!(
        rebuilt
            .iter()
            .any(|input| matches!(input, Input::User { text, .. } if text == "The agent: done"))
    );
    assert!(
        rebuilt.iter().any(
            |input| matches!(input, Input::User { text, .. } if text.contains("later prompt"))
        )
    );
}

#[test]
fn a_summary_retry_is_one_turn_with_two_billed_attempts() {
    let mut failed = summary_failure("try again");
    failed.code = ErrorCode::RateLimited;
    let mut session = run_a_with_tail(vec![
        Scripted::failed_after(failed, fakes::call_usage("summary-first")),
        Scripted::text("Tried X."),
    ]);
    let before = log::read(&session.dir).unwrap().len();
    let watcher = session.log.watch();
    let clock = session.clock.clone();
    let until = contract::clock::Clock::now(clock.as_ref()) + std::time::Duration::from_secs(2);
    let mut looped = session.looped.take().unwrap();
    let (rw, answer) = rewind(args(None, None, true));
    session.inbox.send(rw).unwrap();
    let (done, finished) = mpsc::channel();
    let worker = thread::spawn(move || {
        let result = looped.turn();
        done.send(result).unwrap();
    });
    clock.advance_when_parked(
        Some(until),
        Some(worker.thread().id()),
        std::time::Duration::from_secs(2),
        &Deadline::after(DEADLINE),
    );
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the retried summary ended")
            .unwrap(),
        None
    );
    accepted(answer_of(answer));
    let lines = log::read(&session.dir).unwrap();
    let tail = &lines[before..];
    assert_eq!(
        kinds(tail),
        [
            "turn_started",
            "step_started",
            "assistant_message_started",
            "usage_recorded",
            "assistant_message_completed",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "rewound"
        ]
    );
    assert_eq!(
        tail[0].payload["input"],
        json!([{"type":"rewind", "command_id":"c_rewind"}])
    );
    let (_, observed) = read_until(watcher, "the retried rewind", |line| line.kind == "rewound");
    let retries: Vec<_> = observed
        .iter()
        .filter(|line| line.kind == "retry_scheduled")
        .collect();
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0].payload["delay_ms"], 2000);
    assert_eq!(tail[10].payload["summary"], "Tried X.");
    assert_eq!(session.provider.requests().len(), 5);
    assert_eq!(
        session.provider.requests()[3],
        session.provider.requests()[4]
    );
}

struct SummaryRepository;

impl contract::repository::RepositoryCode for SummaryRepository {
    fn file(&self, _: &str) -> Result<String, contract::shapes::Failure> {
        Err(contract::shapes::Failure {
            code: contract::ErrorCode::IoFailed,
            message: "This fake does not read offered files.".into(),
            retry_after_ms: None,
            provider: None,
        })
    }
    fn unapproved(
        &self,
    ) -> Result<Vec<contract::repository::Unapproved>, contract::shapes::Failure> {
        Ok(vec![])
    }
    fn decide(
        &self,
        _: &contract::events::OfferedItem,
        _: contract::events::OfferDecision,
    ) -> Result<contract::repository::Decided, contract::shapes::Failure> {
        Ok(contract::repository::Decided::Recorded)
    }
}

#[test]
fn prompts_deferred_by_the_repository_offer_are_answered_after_the_summary() {
    use contract::events::{
        Clients, Event, OfferDecision, OfferedItem, OfferedKind, RepositoryCodeOffered,
    };
    for success in [true, false] {
        let session = run_a();
        let offer = contract::RequestId("r_summary_offer".into());
        session
            .log
            .append(&Event::Clients(Clients { count: 1 }), None, None)
            .unwrap();
        session
            .log
            .append(
                &Event::RepositoryCodeOffered(RepositoryCodeOffered {
                    request_id: offer.clone(),
                    items: vec![OfferedItem {
                        files: vec![],
                        kind: OfferedKind::McpServer,
                        name: "summary-code".into(),
                        hash: "h1".into(),
                        required: false,
                        summary: "test repository code".into(),
                        version: None,
                        diff: None,
                    }],
                }),
                None,
                None,
            )
            .unwrap();
        let script = if success {
            Scripted::text("Tried X.")
        } else {
            Scripted::failed(summary_failure("summary failed"))
        };
        let (looped, tx, provider) =
            resume_existing(&session, "s_test", vec![script, Scripted::text("next")]);
        let mut looped = looped.repository_code(Arc::new(SummaryRepository));
        let (rw, answer) = rewind(args(None, None, true));
        let (prompt_send, prompt_answer) = mpsc::channel();
        let prompt_ack = Ack(Box::new(move |answer| {
            prompt_send.send(answer).unwrap();
        }));
        tx.send(rw).unwrap();
        tx.send(Delivery::Prompt(message("deferred prompt"), prompt_ack))
            .unwrap();
        tx.send(Delivery::Reply(
            Reply {
                request_id: offer,
                answer: ReplyAnswer::Decisions {
                    decisions: vec![OfferDecision::Approve],
                },
            },
            ignore(),
        ))
        .unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || {
            let result = looped.turn();
            done.send(result).unwrap();
        });
        let outcome = Deadline::after(DEADLINE)
            .recv(&finished)
            .expect("the offer and summary ended")
            .unwrap();
        let lines = log::read(&session.dir).unwrap();
        if success {
            assert_eq!(outcome, None);
            accepted(answer_of(answer));
            let rejection = rejected(answer_of(prompt_answer));
            assert_eq!(rejection.code, ErrorCode::Closing);
            assert_eq!(
                rejection.message,
                "The session was rewound and takes no more commands."
            );
            assert_eq!(lines.last().unwrap().kind, "rewound");
            assert_eq!(provider.requests().len(), 1);
        } else {
            assert_eq!(outcome, Some(TurnOutcome::Completed));
            assert_eq!(rejected(answer_of(answer)).code, ErrorCode::SummaryFailed);
            assert!(answer_of(prompt_answer).is_ok());
            assert!(lines.iter().all(|line| line.kind != "rewound"));
            assert_eq!(provider.requests().len(), 2);
            assert!(
                matches!(provider.requests()[1].conversation.last(), Some(Input::User { text, .. }) if text == "deferred prompt")
            );
        }
    }
}

#[test]
fn a_summary_builds_the_preamble_in_a_resumed_process_before_its_turn() {
    let session = run_a();
    let before = log::read(&session.dir).unwrap().len();
    let (looped, tx, provider) =
        resume_existing(&session, "s_test", vec![Scripted::text("Resumed summary.")]);
    let (rw, answer) = rewind(args(None, None, true));
    drop(drive(looped, &tx, rw));
    accepted(answer_of(answer));
    let lines = log::read(&session.dir).unwrap();
    assert_eq!(
        kinds(&lines[before..]),
        [
            "fiber_started",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "rewound"
        ]
    );
    assert_eq!(provider.requests().len(), 1);
}

#[test]
fn a_summary_after_an_idle_switch_uses_the_new_model_and_preamble() {
    let mut session = run_a();
    let provider = Arc::new(ScriptedProvider::new(vec![Scripted::text(
        "Switched summary.",
    )]));
    let switched = provider.clone();
    let prepare: r#loop::Prepare = Arc::new(move |_, _, chosen| {
        Ok(r#loop::Prepared {
            provider: switched.clone(),
            model: Model {
                reference: "fake/switched".into(),
                cost: None,
                subscription: false,
            },
            thinking: chosen,
            chosen,
            credential: Some("work".into()),
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: Some("switched addendum".into()),
            handoff: r#loop::HandoffSettings::default(),
            reviewer: Err(contract::shapes::Failure {
                code: ErrorCode::NoModel,
                message: "no reviewer".into(),
                retry_after_ms: None,
                provider: None,
            }),
            web_search: r#loop::Hosted::Keep,
            notice: None,
            applied: None,
            credential_files: vec![],
            observe: None,
            reviewer_observe: None,
        })
    });
    let looped = session
        .looped
        .take()
        .unwrap()
        .switcher(prepare, r#loop::Switchable { chosen: None });
    session
        .inbox
        .send(support::model("fake/switched", None))
        .unwrap();
    let (rw, answer) = rewind(args(None, None, true));
    drop(drive(looped, &session.inbox, rw));
    accepted(answer_of(answer));
    assert_eq!(provider.requests().len(), 1);
    assert!(
        provider.requests()[0]
            .system_prompt
            .contains("switched addendum")
    );
    let lines = log::read(&session.dir).unwrap();
    let built = lines
        .iter()
        .rfind(|line| line.kind == "preamble_built")
        .unwrap();
    assert_eq!(built.payload["model"], "fake/switched");
}

#[test]
fn a_default_rewind_closes_the_session_and_names_the_new_one() {
    let mut session = run_a();
    let calls = session.provider.requests().len();
    let lines = log::read(&session.dir).unwrap();
    let at = point(&lines);
    let (rw, answered) = rewind(args(None, None, false));
    let looped = session.looped.take().unwrap();
    let (looped, outcome) = drive(looped, &session.inbox, rw);
    session.looped = Some(looped);
    let new = accepted(answer_of(answered));
    assert_minted(&new);
    assert_eq!(outcome, None, "no turn starts after the rewind");
    // The log's last line is `rewound`, naming the new session and the
    // point, with no `from_session_id` key for this session's own point.
    let lines = log::read(&session.dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(2)
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(tail, ["rewound", "turn_completed"]);
    assert!(
        lines.iter().all(|line| line.kind != "fiber_exited"),
        "no `fiber_exited` follows `rewound`"
    );
    let last = lines.last().unwrap();
    assert_eq!(last.payload["new_session_id"], json!(new.0));
    assert_eq!(last.payload["seq"], json!(at));
    assert_eq!(session.provider.requests().len(), calls);
    assert!(last.payload.get("summary").is_none());
    assert!(last.payload.get("jobs").is_none());
    assert!(
        last.payload.get("from_session_id").is_none(),
        "this session's own point names no session"
    );
    // The process ends with nothing more written.
    let count = log::read(&session.dir).unwrap().len();
    let ran = session.looped.take().unwrap().run();
    ran.result.unwrap();
    let after = log::read(&session.dir).unwrap();
    assert_eq!(after.len(), count, "`run` writes nothing after `rewound`");
    assert_eq!(after.last().unwrap().kind, "rewound");
    // The exit knows the session rewound from the run's own summary: it
    // writes nothing and reports the run's error.
    let exited = r#loop::fiber_exited(&session.log, &ran.summary, Ok(()), false, None).unwrap();
    assert_eq!((exited.code, exited.error), (0, None));
    let failure = Failure {
        code: ErrorCode::InvalidRequest,
        message: "The run failed.".into(),
        retry_after_ms: None,
        provider: None,
    };
    let exited = r#loop::fiber_exited(
        &session.log,
        &ran.summary,
        Err(failure.clone()),
        false,
        None,
    )
    .unwrap();
    assert_eq!((exited.code, exited.error), (1, Some(failure)));
    assert_eq!(log::read(&session.dir).unwrap().len(), count);
}

#[test]
fn a_malformed_line_after_line_0_refuses_the_rewind_log_corrupt_and_writes_no_rewound() {
    use std::io::Write as _;
    let mut session = run_a();
    // The own log's line 0 reads; a later line is not an envelope.
    let path = session.dir.join("events.jsonl");
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"not an envelope\n")
        .unwrap();
    let (rw, answered) = rewind(args(None, None, false));
    session.inbox.send(rw).unwrap();
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.turn()).unwrap());
    let result = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the rewind ended the wait in time");
    assert_eq!(result.unwrap_err().code(), ErrorCode::LogCorrupt);
    // The ack is dropped unanswered, and nothing was written before the
    // check refused it.
    assert_eq!(
        Deadline::after(DEADLINE).recv(&answered),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
    let text = fs::read_to_string(&path).unwrap();
    assert!(!text.contains("\"kind\":\"rewound\""));
}

#[test]
fn a_message_after_rewound_is_refused() {
    let mut session = run_a();
    let admission = Arc::new(ext_support::admission::Admission::default());
    let looped = session
        .looped
        .take()
        .unwrap()
        .admission(Arc::clone(&admission));
    session.looped = Some(looped);
    // The rewind answers through an ack that probes admission first: the
    // flag is already set before any client learns of `rewound`.
    let (tx, answered) = mpsc::channel();
    let (probe_tx, probe_rx) = mpsc::channel();
    let probing = Arc::clone(&admission);
    let ack = Ack(Box::new(move |answer| {
        let mut called = false;
        let done = probing.admit(|| {
            called = true;
        });
        probe_tx.send((done.is_some(), called)).unwrap();
        tx.send(answer).unwrap();
    }));
    let rw = Delivery::Rewind(CommandId("c_rewind".into()), args(None, None, false), ack);
    let looped = session.looped.take().unwrap();
    let (looped, outcome) = drive(looped, &session.inbox, rw);
    session.looped = Some(looped);
    let new = accepted(answer_of(answered));
    assert_minted(&new);
    assert_eq!(outcome, None, "no turn starts after the rewind");
    assert_eq!(
        Deadline::after(DEADLINE).recv(&probe_rx),
        Ok((false, false)),
        "admission closed under the ack, with the send never running"
    );
    // The process ends with nothing more written, and admission stays shut.
    session.looped.take().unwrap().run().result.unwrap();
    let mut called = false;
    let done = admission.admit(|| {
        called = true;
    });
    assert!(done.is_none());
    assert!(!called);
}

#[test]
fn a_refused_rewind_leaves_admission_open() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let admission = Arc::new(ext_support::admission::Admission::default());
    let looped = session
        .looped
        .take()
        .unwrap()
        .admission(Arc::clone(&admission));
    session.looped = Some(looped);
    let before = log_bytes(&session.dir);
    let inbox = take_inbox(&mut session);
    let (rw, answered) = rewind(args(None, None, false));
    let looped = session.looped.take().unwrap();
    session.looped = Some(drive_closed(looped, inbox, vec![rw]));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(log_bytes(&session.dir), before);
    // The refusal closed nothing: a later message is still admitted.
    assert!(admission.admit(|| true).is_some());
}

#[test]
fn an_explicit_seq_at_a_boundary_is_accepted() {
    let mut session = run_a();
    let lines = log::read(&session.dir).unwrap();
    let first_turn = lines
        .iter()
        .find(|line| line.kind == "turn_started")
        .unwrap()
        .seq
        .unwrap()
        .0;
    let (rw, answered) = rewind(args(None, Some(first_turn - 1), false));
    let looped = session.looped.take().unwrap();
    let (looped, outcome) = drive(looped, &session.inbox, rw);
    session.looped = Some(looped);
    accepted(answer_of(answered));
    assert_eq!(outcome, None);
    let lines = log::read(&session.dir).unwrap();
    assert_eq!(lines.last().unwrap().kind, "rewound");
    assert_eq!(lines.last().unwrap().payload["seq"], json!(first_turn - 1));
}

#[test]
fn a_seq_off_a_boundary_is_refused_and_writes_nothing() {
    let mut session = run_a();
    let lines = log::read(&session.dir).unwrap();
    let last = lines.last().unwrap().seq.unwrap().0;
    let before = log_bytes(&session.dir);
    let inbox = take_inbox(&mut session);
    let (rw, answered) = rewind(args(None, Some(last), false));
    let looped = session.looped.take().unwrap();
    session.looped = Some(drive_closed(looped, inbox, vec![rw]));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::NotStepBoundary);
    assert_eq!(
        rejection.message,
        format!(
            "Line {last} is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results."
        )
    );
    assert_eq!(log_bytes(&session.dir), before);
}

#[test]
fn a_seq_without_a_successor_is_refused_and_writes_nothing() {
    // `u64::MAX` has no next line: the refusal answers on the test's
    // named deadline instead of panicking the session.
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let before = log_bytes(&session.dir);
    let inbox = take_inbox(&mut session);
    let (rw, answered) = rewind(args(None, Some(u64::MAX), false));
    let looped = session.looped.take().unwrap();
    session.looped = Some(drive_closed(looped, inbox, vec![rw]));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::NotStepBoundary);
    assert_eq!(
        rejection.message,
        format!(
            "Line {} is not a step boundary: the start of a turn, just after the person's input, or just after a batch of tool results.",
            u64::MAX
        )
    );
    assert_eq!(log_bytes(&session.dir), before);
}

#[test]
fn a_rewind_with_no_turn_is_invalid_arguments() {
    let mut session = Session::new(vec![Scripted::text("unused")], None);
    let before = log_bytes(&session.dir);
    let inbox = take_inbox(&mut session);
    let (rw, answered) = rewind(args(None, None, false));
    let looped = session.looped.take().unwrap();
    session.looped = Some(drive_closed(looped, inbox, vec![rw]));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::InvalidArguments);
    assert_eq!(rejection.message, "This session has no turn to rewind to.");
    assert_eq!(log_bytes(&session.dir), before);
}

/// A tool that sends one `rewind` to the inbox when it runs: the drain
/// after the call takes it while the turn still runs. With `close_first`
/// it sends `close` ahead of it, as `fiber ask` does, so the rewind is
/// taken on a closing turn.
struct SendRewind {
    inbox: Mutex<Option<mpsc::Sender<Delivery>>>,
    seen: Mutex<Option<mpsc::Receiver<Answer>>>,
    close_first: bool,
}

impl SendRewind {
    fn install(&self, inbox: mpsc::Sender<Delivery>) {
        *self.inbox.lock().unwrap() = Some(inbox);
    }

    #[track_caller]
    fn answer(&self) -> Answer {
        let waiting = self
            .seen
            .lock()
            .unwrap()
            .take()
            .expect("the tool sent its rewind");
        Deadline::after(DEADLINE)
            .recv(&waiting)
            .expect("the rewind is answered in time")
    }
}

impl contract::tool::Tool for SendRewind {
    fn definition(&self) -> contract::provider::ToolDefinition {
        contract::provider::ToolDefinition {
            name: "send".into(),
            description: "Sends the rewind.".into(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            deferred: false,
            hosted: None,
        }
    }

    fn effects(
        &self,
        _: &Map<String, Value>,
    ) -> Result<contract::tool::Effects, contract::tool::EffectsError> {
        Ok(contract::tool::Effects {
            declared: contract::shapes::DeclaredEffects {
                effects: vec![contract::shapes::Effect::Reads],
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
        _: &Map<String, Value>,
        _: &dyn contract::tool::Cancel,
        _: &dyn contract::emit::Emit,
    ) -> contract::tool::Output {
        let inbox = self
            .inbox
            .lock()
            .unwrap()
            .clone()
            .expect("the inbox sender is installed");
        if self.close_first {
            inbox.send(Delivery::Close(ignore())).unwrap();
        }
        let (rw, answered) = rewind(args(None, None, false));
        inbox.send(rw).unwrap();
        *self.seen.lock().unwrap() = Some(answered);
        contract::tool::Output {
            content: vec![ContentPart::Text {
                text: "sent".into(),
            }],
            ..contract::tool::Output::default()
        }
    }

    fn bound(&self) -> contract::tool::Bound {
        contract::tool::Bound::DEFAULT
    }
}

#[test]
fn a_rewind_during_a_turn_is_busy() {
    let tool = Arc::new(SendRewind {
        inbox: Mutex::new(None),
        seen: Mutex::new(None),
        close_first: false,
    });
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("send", json!({}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::clone(&tool) as Arc<dyn Tool>],
    );
    tool.install(session.inbox.clone());
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let rejection = rejected(tool.answer());
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(rejection.message, "A turn is running; rewind once it ends.");
    assert!(
        log::read(&session.dir)
            .unwrap()
            .iter()
            .all(|line| line.kind != "rewound"),
        "the refused rewind closes nothing"
    );
}

#[test]
fn a_rewind_during_a_closing_turn_is_closing() {
    let tool = Arc::new(SendRewind {
        inbox: Mutex::new(None),
        seen: Mutex::new(None),
        close_first: true,
    });
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("send", json!({}))]),
            Scripted::text("Done."),
        ],
        None,
        vec![Arc::clone(&tool) as Arc<dyn Tool>],
    );
    tool.install(session.inbox.clone());
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let rejection = rejected(tool.answer());
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert_eq!(
        rejection.message,
        "The session is closing and takes no new turn."
    );
}

fn ask_shell() -> Arc<TestTool> {
    let mut tool = TestTool::declaring(
        "shell",
        "Ran it.",
        vec![contract::shapes::Effect::Executes],
        None,
    );
    tool.subject = Some("npm publish".into());
    Arc::new(tool)
}

fn ask_rule() -> Rule {
    Rule {
        decision: RuleDecision::Ask,
        tool: "shell".into(),
        prefix: "npm publish".into(),
        added: None,
        session_id: None,
    }
}

#[test]
fn a_rewind_while_an_approval_waits_is_busy() {
    let tool = ask_shell();
    let mut session = Session::with_tools(
        vec![
            calls_reply("", &[("shell", paris())]),
            Scripted::text("Done."),
        ],
        None,
        vec![tool as Arc<dyn Tool>],
    );
    session.rules.set(StandingRules {
        global: vec![ask_rule()],
        project: Vec::new(),
    });
    // Watches for the approval wait, sends the rewind while it waits, then
    // answers allow: the turn always ends, whatever the rewind answered.
    let (checked, seen) = mpsc::channel();
    let watcher = session.log.watch();
    let inbox = session.inbox.clone();
    let waiting = thread::spawn(move || {
        let (_, lines) = read_until(watcher, "a permission_requested line", |line| {
            line.kind == "permission_requested"
        });
        let id = lines.last().unwrap().payload["request_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (rw, answered) = rewind(args(None, None, false));
        inbox.send(rw).unwrap();
        let answer = Deadline::after(DEADLINE)
            .recv(&answered)
            .expect("the rewind is answered in time");
        inbox
            .send(reply_to(RequestId(id), allow(), ignore()))
            .unwrap();
        checked.send(answer).unwrap();
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    waiting.join().expect("the watcher ends");
    let rejection = rejected(
        Deadline::after(DEADLINE)
            .recv(&seen)
            .expect("the answer arrives"),
    );
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(rejection.message, "A turn is running; rewind once it ends.");
}

#[test]
fn a_rewind_after_close_is_closing_and_writes_nothing() {
    let mut session = Session::new(vec![Scripted::text("done")], None);
    let before = log_bytes(&session.dir);
    let inbox = take_inbox(&mut session);
    inbox.send(Delivery::Close(ignore())).unwrap();
    let (rw, answered) = rewind(args(None, None, false));
    let looped = session.looped.take().unwrap();
    session.looped = Some(drive_closed(looped, inbox, vec![rw]));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::Closing);
    assert_eq!(
        rejection.message,
        "The session is closing and takes no new turn."
    );
    assert_eq!(log_bytes(&session.dir), before);
}

#[test]
fn a_prompt_and_a_rewind_in_one_batch_runs_the_turn_and_refuses_the_rewind() {
    let mut session = Session::new(vec![Scripted::text("done")], None);
    let (rw, answered) = rewind(args(None, None, false));
    session.inbox.send(delivery("hi")).unwrap();
    session.inbox.send(rw).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(rejection.message, "A turn is running; rewind once it ends.");
    assert!(
        log::read(&session.dir)
            .unwrap()
            .iter()
            .all(|line| line.kind != "rewound")
    );
}

#[test]
fn a_rewind_before_a_prompt_and_a_close_refuses_both_closing() {
    let mut session = Session::new(vec![Scripted::text("done")], None);
    session.inbox.send(delivery("first")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let (prompt_tx, prompted) = mpsc::channel();
    let (close_tx, closed) = mpsc::channel();
    let inbox = take_inbox(&mut session);
    let (rw, answered) = rewind(args(None, None, false));
    let prompt = Delivery::Prompt(
        message("hi"),
        contract::inbox::Ack(Box::new(move |answer| {
            let _sent = prompt_tx.send(answer);
        })),
    );
    let close = Delivery::Close(contract::inbox::Ack(Box::new(move |answer| {
        let _sent = close_tx.send(answer);
    })));
    let looped = session.looped.take().unwrap();
    session.looped = Some(drive_closed(looped, inbox, vec![rw, prompt, close]));
    accepted(answer_of(answered));
    for (name, rx) in [("prompt", prompted), ("close", closed)] {
        match Deadline::after(DEADLINE)
            .recv(&rx)
            .expect("refused in time")
        {
            Err(rejection) => {
                assert_eq!(rejection.code, ErrorCode::Closing, "{name}");
                assert_eq!(
                    rejection.message, "The session was rewound and takes no more commands.",
                    "{name}"
                );
            }
            Ok(_) => panic!("the {name} is refused"),
        }
    }
    // Nothing is written after `rewound`.
    let lines = log::read(&session.dir).unwrap();
    let tail: Vec<&str> = lines
        .iter()
        .rev()
        .take(2)
        .map(|line| line.kind.as_str())
        .collect();
    assert_eq!(tail, ["rewound", "turn_completed"]);
}

#[test]
fn a_rewind_on_a_delegate_is_refused() {
    let session = run_a();
    let (looped, tx, dir) = resume_b(
        &session,
        "s_dddddddddddddddd",
        None,
        Some(Parent {
            session_id: SessionId("s_eeeeeeeeeeeeeeee".into()),
            delegate_id: JobId("j_d".into()),
        }),
    );
    let before = log_bytes(&dir);
    let (rw, answered) = rewind(args(None, None, false));
    let looped = drive_closed(looped, tx, vec![rw]);
    drop(looped);
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::DelegateSession);
    assert_eq!(rejection.message, "A delegate cannot be rewound.");
    assert_eq!(log_bytes(&dir), before);
}

#[test]
fn a_rewind_naming_its_own_session_rewinds() {
    let mut session = run_a();
    let lines = log::read(&session.dir).unwrap();
    let at = point(&lines);
    let (rw, answered) = rewind(args(Some("s_test"), None, false));
    let looped = session.looped.take().unwrap();
    let (looped, outcome) = drive(looped, &session.inbox, rw);
    session.looped = Some(looped);
    accepted(answer_of(answered));
    assert_eq!(outcome, None);
    let lines = log::read(&session.dir).unwrap();
    let last = lines.last().unwrap();
    assert_eq!(last.kind, "rewound");
    assert_eq!(last.payload["seq"], json!(at));
    assert!(last.payload.get("from_session_id").is_none());
}

#[test]
fn malformed_from_session_ids_are_invalid_arguments() {
    for from in [
        "../x",
        "s_ABC",
        "",
        "s_0123456789abcde",
        "s_0123456789abcdef0",
    ] {
        // No turn is needed: the shape check runs before any point.
        let mut session = Session::new(vec![Scripted::text("unused")], None);
        let home = session.dir.parent().unwrap().to_path_buf();
        let mut listed: Vec<String> = fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        listed.sort();
        let before = log_bytes(&session.dir);
        let inbox = take_inbox(&mut session);
        let (rw, answered) = rewind(args(Some(from), None, false));
        let looped = session.looped.take().unwrap();
        session.looped = Some(drive_closed(looped, inbox, vec![rw]));
        let rejection = rejected(answer_of(answered));
        assert_eq!(rejection.code, ErrorCode::InvalidArguments, "{from}");
        assert_eq!(
            rejection.message,
            format!("{from} is not a session id: one is `s_` followed by 16 lowercase hex digits."),
            "{from}"
        );
        assert_eq!(log_bytes(&session.dir), before, "{from}");
        let mut entries: Vec<String> = fs::read_dir(&home)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        assert_eq!(entries, listed, "{from}: nothing is created");
    }
}

fn hub_sentence(id: &str) -> String {
    format!(
        "Session {id} is neither this session nor one it continues. To rewind it, send `rewind` for it through the hub."
    )
}

#[test]
fn a_rewind_naming_a_session_off_the_chain_is_refused() {
    // Beside this session: one still running (its log held), one exited,
    // and one missing entirely. None is on its chain, running or not.
    // No turn is needed: the chain check runs before any point.
    for from in [
        "s_cccccccccccccccc",
        "s_dddddddddddddddd",
        "s_eeeeeeeeeeeeeeee",
    ] {
        let mut session = Session::new(vec![Scripted::text("unused")], None);
        let home = session.dir.parent().unwrap().to_path_buf();
        let running = Log::create(&home, SessionId(from.into()), session.clock.clone()).unwrap();
        if from == "s_eeeeeeeeeeeeeeee" {
            // Missing entirely: no directory.
            drop(running);
            fs::remove_dir_all(home.join(from)).unwrap();
        } else if from == "s_dddddddddddddddd" {
            // Exited: a closed log.
            running
                .append(
                    &contract::events::Event::SessionStarted(SessionStarted {
                        workspace: session.workspace.display().to_string(),
                        variables: Variables {
                            path: "/usr/bin:/bin".to_owned(),
                            names: Vec::new(),
                            source: VariablesSource::Inherited,
                        },
                        parent: None,
                        forked_from: None,
                        rewind: None,
                        worktree: None,
                        supervised: false,
                    }),
                    None,
                    None,
                )
                .unwrap();
        }
        let before = log_bytes(&session.dir);
        let inbox = take_inbox(&mut session);
        let (rw, answered) = rewind(args(Some(from), None, false));
        let looped = session.looped.take().unwrap();
        session.looped = Some(drive_closed(looped, inbox, vec![rw]));
        let rejection = rejected(answer_of(answered));
        assert_eq!(rejection.code, ErrorCode::InvalidArguments, "{from}");
        assert_eq!(rejection.message, hub_sentence(from), "{from}");
        assert_eq!(log_bytes(&session.dir), before, "{from}");
    }
}

/// Session A after two turns under a session id a `from_session_id` can
/// name, with its two turn starts and the point B continues from.
fn run_chained_a() -> (Session, u64, u64, u64) {
    let session = run_a_with_id(A_ID);
    let lines = log::read(&session.dir).unwrap();
    let mut starts = lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .map(|line| line.seq.unwrap().0);
    let first = starts.next().unwrap();
    let second = starts.next().unwrap();
    (session, first, second, second - 1)
}

#[test]
fn a_rewind_to_an_ancestor_point_names_that_session() {
    let (session, first, _, at) = run_chained_a();
    let (looped, tx, dir) = resume_b(
        &session,
        B_ID2,
        Some(Point {
            session_id: SessionId(A_ID.into()),
            seq: Seq(at),
        }),
        None,
    );
    let count = log::read(&dir).unwrap().len();
    let (rw, answered) = rewind(args(Some(A_ID), Some(first - 1), false));
    let (looped, outcome) = drive(looped, &tx, rw);
    drop(looped);
    accepted(answer_of(answered));
    assert_eq!(outcome, None);
    let lines = log::read(&dir).unwrap();
    assert_eq!(lines.len(), count + 1, "only `rewound` is written");
    let last = lines.last().unwrap();
    assert_eq!(last.kind, "rewound");
    assert_eq!(last.payload["seq"], json!(first - 1));
    assert_eq!(last.payload["from_session_id"], json!(A_ID));
    assert!(last.payload.get("jobs").is_none());
}

#[test]
fn a_seq_past_the_ancestor_bound_is_not_in_this_sessions_history() {
    let (session, _, second, at) = run_chained_a();
    let (looped, tx, dir) = resume_b(
        &session,
        B_ID2,
        Some(Point {
            session_id: SessionId(A_ID.into()),
            seq: Seq(at),
        }),
        None,
    );
    let before = log_bytes(&dir);
    let (rw, answered) = rewind(args(Some(A_ID), Some(second), false));
    let looped = drive_closed(looped, tx, vec![rw]);
    drop(looped);
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::NotStepBoundary);
    assert_eq!(
        rejection.message,
        format!("Line {second} of session {A_ID} is not in this session's history.")
    );
    assert_eq!(log_bytes(&dir), before);
}

#[test]
fn the_default_point_under_a_bound_is_the_ancestor_turn_before_it() {
    let (session, first, _, at) = run_chained_a();
    let (looped, tx, dir) = resume_b(
        &session,
        B_ID2,
        Some(Point {
            session_id: SessionId(A_ID.into()),
            seq: Seq(at),
        }),
        None,
    );
    let (rw, answered) = rewind(args(Some(A_ID), None, false));
    let (looped, outcome) = drive(looped, &tx, rw);
    drop(looped);
    accepted(answer_of(answered));
    assert_eq!(outcome, None);
    let lines = log::read(&dir).unwrap();
    let last = lines.last().unwrap();
    assert_eq!(last.kind, "rewound");
    assert_eq!(last.payload["seq"], json!(first - 1));
    assert_eq!(last.payload["from_session_id"], json!(A_ID));
}

#[test]
fn a_summary_checks_jobs_and_the_point_before_starting() {
    let (session, _, _, at) = run_chained_a();
    let (looped, tx, dir) = resume_b(
        &session,
        B_ID2,
        Some(Point {
            session_id: SessionId(A_ID.into()),
            seq: Seq(at),
        }),
        None,
    );
    let before = log_bytes(&dir);
    let (rw, answered) = rewind(args(Some(A_ID), Some(u64::MAX), true));
    let looped = drive_closed(looped, tx, vec![rw]);
    drop(looped);
    assert_eq!(
        rejected(answer_of(answered)).code,
        ErrorCode::NotStepBoundary
    );
    assert_eq!(log_bytes(&dir), before);

    let (looped, tx, dir) = resume_b(
        &session,
        "s_bbbbbbbbbbbbbbb3",
        Some(Point {
            session_id: SessionId(A_ID.into()),
            seq: Seq(at),
        }),
        None,
    );
    let looped = looped.jobs(Arc::new(StubJobs::new([JobId("j_1".into())])));
    let before = log::read(&dir).unwrap().len();
    let (rw, answered) = rewind(args(Some(A_ID), None, true));
    drop(drive_closed(looped, tx, vec![rw]));
    // The summary runs before the stop, so a summary that cannot be made
    // fails the rewind even while jobs run: no `rewound` follows.
    assert_eq!(rejected(answer_of(answered)).code, ErrorCode::SummaryFailed);
    let lines = log::read(&dir).unwrap();
    assert!(lines.len() > before, "the failed summary keeps its turn");
    assert!(lines.iter().all(|line| line.kind != "rewound"));
}

/// Loop-level handoff rewinds (`docs/events.md`, "Rewind"): session A runs
/// an in-turn handoff in its first turn, then a plain second turn. A rewind
/// past the handoff starts from the handoff's context; one before it
/// ignores the handoff.
const HANDOFF_TRIGGER: u64 = 1_000;

fn handed_settings() -> r#loop::HandoffSettings {
    r#loop::HandoffSettings {
        tokens: HANDOFF_TRIGGER,
        nudge: false,
        ..r#loop::HandoffSettings::default()
    }
}

/// A reply of `text`, with `tokens` in its prompt.
fn handed_said(text: &str, tokens: u64) -> Scripted {
    support::with_tokens(Scripted::text(text), tokens, 0)
}

/// Session A after two turns: turn 1 calls `write_file`, hands off on
/// the trigger and answers "Done." from the new context; turn 2 answers
/// "Again." with no handoff. Its tools match the ones `start_b` wires,
/// so a rewound session's request tools equal the old session's.
fn run_a_handed() -> Session {
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::clone(&write_tool()) as Arc<dyn Tool>,
        Arc::clone(&exec_tool()) as Arc<dyn Tool>,
    ];
    let mut session = Session::with_tools(
        vec![
            support::with_tokens(
                calls_reply("working", &[("write_file", paris())]),
                HANDOFF_TRIGGER - 1,
                0,
            ),
            Scripted::text("The note."),
            handed_said("Done.", 50),
            handed_said("Again.", 50),
        ],
        None,
        tools,
    )
    .handoff(handed_settings());
    session.rules.set(StandingRules {
        global: vec![allow_rule("write_file")],
        project: Vec::new(),
    });
    session.inbox.send(delivery("hi")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session
}

/// The `seq` of the `turn_started` starting A's `nth` turn, one-indexed.
fn turn_start(lines: &[Envelope], nth: usize) -> u64 {
    lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .nth(nth - 1)
        .unwrap()
        .seq
        .unwrap()
        .0
}

fn user_text(input: &Input) -> &str {
    match input {
        Input::User { text, .. } => text,
        other @ (Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. }
        | Input::ToolSearchResult { .. }) => panic!("not a user message: {other:?}"),
    }
}

#[test]
fn a_rewound_session_after_a_handoff_starts_from_the_handoffs_context() {
    let session = run_a_handed();
    let a_lines = log::read(&session.dir).unwrap();
    // The handoff ran in turn 1: its completion and the new opening sit
    // between the two turns.
    let kinds: Vec<&str> = a_lines.iter().map(|line| line.kind.as_str()).collect();
    let completed = kinds
        .iter()
        .position(|kind| *kind == "handoff_completed")
        .unwrap();
    assert_eq!(kinds[completed + 1], "opening_message");
    assert!(
        completed
            < kinds
                .iter()
                .position(|kind| *kind == "turn_completed")
                .unwrap()
    );
    let a_requests = session.provider.requests();
    assert_eq!(a_requests.len(), 4, "call, note, reply, second turn");
    // The second turn's first request follows the handoff: the new opening,
    // the carried input, the note, then its own prompt.
    let a_second = a_requests.last().unwrap();
    assert_eq!(a_second.conversation.len(), 5);
    assert!(
        user_text(&a_second.conversation[0]).starts_with("This message is from Fiber"),
        "the opening message renders first"
    );
    assert_eq!(user_text(&a_second.conversation[1]), "hi");
    assert_eq!(user_text(&a_second.conversation[2]), "The note.");
    assert!(
        matches!(&a_second.conversation[3], Input::Assistant { text, .. } if text == "Done."),
        "turn 1's post-handoff reply stays in the context"
    );
    assert_eq!(user_text(&a_second.conversation[4]), "again");
    // The point is the line before the second turn's `turn_started`, a
    // boundary after the handoff.
    let at = turn_start(&a_lines, 2) - 1;
    let note = rewind_note(&session.dir, Seq(at)).unwrap();
    let k = a_second.conversation.len() - 1;
    let (b, tx, provider, _dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note.clone(),
        None,
        vec![Scripted::text("three-done")],
    );
    let _b = run_turn(b, &tx, "three");
    let b_requests = provider.requests();
    assert_eq!(b_requests.len(), 1);
    let first = &b_requests[0];
    assert_eq!(first.system_prompt, a_second.system_prompt);
    assert_eq!(first.thinking, a_second.thinking);
    assert_eq!(first.tool_choice, a_second.tool_choice);
    assert_eq!(first.cache_lifetime, a_second.cache_lifetime);
    assert_eq!(first.cache_key, "s_test");
    assert_eq!(first.previous_end, a_second.previous_end);
    assert_eq!(first.tools, a_second.tools);
    // The handoff's context, then the rewind note, then the new prompt.
    assert_eq!(&first.conversation[..k], &a_second.conversation[..k]);
    assert_eq!(
        first.conversation[k],
        Input::User {
            text: note,
            images: Vec::new(),
        }
    );
    assert_eq!(user_text(&first.conversation[k + 1]), "three");
}

#[test]
fn a_rewound_session_before_a_handoff_ignores_it() {
    let session = run_a_handed();
    let a_lines = log::read(&session.dir).unwrap();
    // The point is the boundary just before the step that triggered the
    // handoff: the tool results answered, the next line a `step_started`.
    let handoff = a_lines
        .iter()
        .find(|line| line.kind == "handoff_started")
        .unwrap()
        .seq
        .unwrap()
        .0;
    let step = a_lines
        .iter()
        .rfind(|line| line.kind == "step_started" && line.seq.unwrap().0 < handoff)
        .unwrap()
        .seq
        .unwrap()
        .0;
    let at = step - 1;
    assert_eq!(
        a_lines
            .iter()
            .find(|line| line.seq.unwrap().0 == step)
            .unwrap()
            .kind,
        "step_started"
    );
    // A's note request: the history to the point, then the handoff's
    // instruction. It is the only request ending in one.
    let a_requests = session.provider.requests();
    let noted = a_requests
        .iter()
        .find(|request| {
            request
                .conversation
                .last()
                .is_some_and(|last| user_text(last).starts_with("Fiber is about to restart"))
        })
        .expect("the handoff's note request ran");
    let k = noted.conversation.len() - 1;
    let note = rewind_note(&session.dir, Seq(at)).unwrap();
    let (b, tx, provider, _dir) = start_b(
        &session,
        B_ID,
        Point {
            session_id: SessionId("s_test".into()),
            seq: Seq(at),
        },
        note.clone(),
        None,
        vec![Scripted::text("three-done")],
    );
    let _b = run_turn(b, &tx, "three");
    let b_requests = provider.requests();
    assert_eq!(b_requests.len(), 1);
    let first = &b_requests[0];
    assert_eq!(first.system_prompt, noted.system_prompt);
    assert_eq!(first.thinking, noted.thinking);
    assert_eq!(first.tool_choice, noted.tool_choice);
    assert_eq!(first.cache_lifetime, noted.cache_lifetime);
    assert_eq!(first.previous_end, noted.previous_end);
    assert_eq!(first.tools, noted.tools);
    // The history to the point, then the rewind note, then the new prompt.
    assert_eq!(&first.conversation[..k], &noted.conversation[..k]);
    assert_eq!(
        first.conversation[k],
        Input::User {
            text: note,
            images: Vec::new(),
        }
    );
    assert_eq!(user_text(&first.conversation[k + 1]), "three");
    assert!(
        first.conversation.iter().all(|input| match input {
            Input::User { text, .. } => !text.contains("The note."),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => true,
        }),
        "no handoff note reaches the rewound session"
    );
}

#[test]
fn known_hosts_rewind_retains_hosts_before_the_point_and_reviews_later_hosts() {
    let mut parent = Session::new(vec![Scripted::text("one"), Scripted::text("two")], None);
    parent
        .inbox
        .send(delivery("Read https://before.test/"))
        .unwrap();
    assert_eq!(parent.turn(), Some(TurnOutcome::Completed));
    let at = log::read(&parent.dir).unwrap().last().unwrap().seq.unwrap();
    parent
        .inbox
        .send(delivery("Read https://after.test/"))
        .unwrap();
    assert_eq!(parent.turn(), Some(TurnOutcome::Completed));
    let home = parent.dir.parent().unwrap().to_path_buf();
    let child_log =
        Arc::new(Log::create(&home, SessionId(B_ID.into()), parent.clock.clone()).unwrap());
    let tools: Vec<_> = ["before", "after"]
        .into_iter()
        .map(|name| {
            let mut tool = TestTool::declaring(
                name,
                "Fetched.",
                vec![contract::shapes::Effect::Network],
                None,
            );
            tool.hosts = Some(vec![format!("https://{name}.test/")]);
            Arc::new(tool)
        })
        .collect();
    let provider = Arc::new(ScriptedProvider::new(vec![
        calls_reply("", &[("before", paris()), ("after", paris())]),
        Scripted::text("Done."),
    ]));
    let reviewer = Arc::new(ScriptedProvider::new(vec![Scripted::text("allow")]));
    let (tx, rx) = mpsc::channel();
    let child = Loop::rewound(
        r#loop::Session {
            log: child_log.clone(),
            provider,
            model: Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt: PromptInputs::new(
                home,
                "/bin/sh".into(),
                child_log.dir().join("events.jsonl").display().to_string(),
                parent.clock.clone(),
                fakes::CONTEXT_WINDOW,
            ),
            inbox: rx,
            tools: tools
                .iter()
                .map(|tool| ("builtin".into(), tool.clone() as Arc<dyn Tool>))
                .collect(),
            permissions: Permissions {
                workspace: parent.workspace.display().to_string(),
                credentials: parent.credentials.clone(),
                credential_files: vec![],
                rules: parent.rules.clone(),
            },
        },
        Rewound {
            factory: None,
            from: Point {
                session_id: SessionId("s_test".into()),
                seq: at,
            },
            worktree: None,
            how: Continuation::Rewind {
                note: rewind_note(&parent.dir, at).unwrap(),
                summary: None,
                name: None,
            },
        },
    )
    .unwrap()
    .reviewer(
        Ok(r#loop::Reviewer {
            provider: reviewer.clone(),
            model: Model {
                reference: "fake/reviewer-1".into(),
                cost: None,
                subscription: false,
            },
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            thinking_levels: vec![],
        }),
        r#loop::BlockLimits::default(),
    );
    let _child = run_turn(child, &tx, "continue");
    let lines = log::read(child_log.dir()).unwrap();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "usage_recorded",
            "permission_resolved",
            "tool_call_started",
            "tool_call_started",
            "tool_call_completed",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed"
        ]
    );
    assert_eq!(reviewer.requests().len(), 1);
    let resolved = lines
        .iter()
        .find(|line| line.kind == "permission_resolved")
        .unwrap();
    assert_eq!(resolved.payload["hosts"], json!(["https://after.test/"]));
    for tool in tools {
        assert_eq!(tool.ran().len(), 1);
    }
}
