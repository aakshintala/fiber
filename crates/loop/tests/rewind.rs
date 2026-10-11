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

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::commands::RewindArgs;
use contract::events::{
    CommandResult, Parent, SessionStarted, TurnOutcome, Variables, VariablesSource,
};
use contract::inbox::{Answer, Delivery, Rejection};
use contract::provider::Input;
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{ContentPart, Origin, Point, Sender, Worktree};
use contract::tool::Tool;
use contract::{CommandId, Envelope, ErrorCode, JobId, RequestId, Seq, SessionId};
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use r#loop::{Loop, Model, Permissions, PromptInputs, Rewound, rewind_note};
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
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::clone(&write_tool()) as Arc<dyn Tool>,
        Arc::clone(&exec_tool()) as Arc<dyn Tool>,
    ];
    let mut session = Session::with_tools(
        vec![
            Scripted::text("one-done"),
            calls_reply("working", &[("write_file", paris()), ("run_cmd", paris())]),
            Scripted::text("two-done"),
        ],
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
            from,
            note,
            worktree,
        },
    )
    .unwrap();
    (b, tx, provider, dir)
}

/// Runs one turn of `b` for the prompt `text`, failing at one deadline
/// instead of hanging.
fn run_turn(b: Loop, tx: &mpsc::Sender<Delivery>, text: &str) -> Loop {
    tx.send(delivery(text)).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let mut b = b;
        let outcome = b.turn().unwrap();
        done.send((b, outcome)).unwrap();
    });
    let (b, outcome) = finished
        .recv_timeout(DEADLINE)
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
    assert_eq!(first.payload["rewind"]["jobs"], json!([]));
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
    // The new session adopts no job, so a `job_started` the old log never
    // ended writes no orphaned `job_completed` there.
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
            | Input::ToolResult { .. } => true,
        }),
        "no job notice reaches the model"
    );
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
        json!({"path": "/w/tree", "branch": "fiber/s_test"})
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
            | Input::ToolResult { .. } => None,
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
            | Input::ToolResult { .. } => None,
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
            Input::Assistant { .. } | Input::Reasoning { .. } | Input::ToolCall { .. } => None,
        })
        .flatten()
        .collect();
    assert_eq!(images.len(), 1);
    let expected = session.dir.join("artifacts/i.png").display().to_string();
    assert_eq!(images[0].path, expected);
    assert_eq!(fs::read(&images[0].path).unwrap(), bytes);
    assert!(!dir.join("artifacts/i.png").exists());
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
            | Input::ToolCall { .. } => None,
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
            from: Point {
                session_id: SessionId("s_model00000001".into()),
                seq: Seq(1),
            },
            note: String::new(),
            worktree: None,
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

fn args(from: Option<&str>, seq: Option<u64>, summarise: bool, adopt: Vec<&str>) -> RewindArgs {
    RewindArgs {
        from_session_id: from.map(|id| SessionId(id.into())),
        seq: seq.map(Seq),
        summarise,
        adopt: adopt.into_iter().map(|job| JobId(job.into())).collect(),
    }
}

fn answer_of(rx: mpsc::Receiver<Answer>) -> Answer {
    rx.recv_timeout(DEADLINE)
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
    finished
        .recv_timeout(DEADLINE)
        .expect("the wait ended in time")
}

/// Sends `batch` to an idle `looped` in order and ends the wait: every
/// refusal answers while the wait goes on, and `sender` (the only live
/// inbox sender) is dropped to end it with no turn. Returns the loop; the
/// test reads each answer on its own channel, failing at one named
/// deadline instead of hanging.
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
    let (looped, outcome) = finished
        .recv_timeout(DEADLINE)
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

/// Jobs that list `running` as running and end nothing: what a `rewind`
/// refusal reads.
struct StillRunning(Vec<JobId>);

impl contract::jobs::Jobs for StillRunning {
    fn open(
        &self,
        _: contract::jobs::Opening,
    ) -> Result<contract::jobs::Opened, contract::jobs::OpenError> {
        Err(contract::jobs::OpenError::Io {
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

    fn foreground(&self, _: contract::jobs::Foreground) {}

    fn running(&self) -> Vec<JobId> {
        self.0.clone()
    }

    fn deliver_to(&self, _: mpsc::Sender<Delivery>) {}
}

#[test]
fn a_default_rewind_closes_the_session_and_names_the_new_one() {
    let mut session = run_a();
    let lines = log::read(&session.dir).unwrap();
    let at = point(&lines);
    let (rw, answered) = rewind(args(None, None, false, vec![]));
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
    assert_eq!(last.payload["jobs"], json!([]));
    assert!(
        last.payload.get("from_session_id").is_none(),
        "this session's own point names no session"
    );
    // The process ends with nothing more written.
    let count = log::read(&session.dir).unwrap().len();
    session.looped.take().unwrap().run().unwrap();
    let after = log::read(&session.dir).unwrap();
    assert_eq!(after.len(), count, "`run` writes nothing after `rewound`");
    assert_eq!(after.last().unwrap().kind, "rewound");
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
    let (rw, answered) = rewind(args(None, Some(first_turn - 1), false, vec![]));
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
    let (rw, answered) = rewind(args(None, Some(last), false, vec![]));
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
    let (rw, answered) = rewind(args(None, Some(u64::MAX), false, vec![]));
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
    let (rw, answered) = rewind(args(None, None, false, vec![]));
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

    fn answer(&self) -> Answer {
        self.seen
            .lock()
            .unwrap()
            .take()
            .expect("the tool sent its rewind")
            .recv_timeout(DEADLINE)
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
            },
            subject: Some(String::new()),
            prefix: None,
            always_reviewed: false,
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
        let (rw, answered) = rewind(args(None, None, false, vec![]));
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
        let (rw, answered) = rewind(args(None, None, false, vec![]));
        inbox.send(rw).unwrap();
        let answer = answered
            .recv_timeout(DEADLINE)
            .expect("the rewind is answered in time");
        inbox
            .send(reply_to(RequestId(id), allow(), ignore()))
            .unwrap();
        checked.send(answer).unwrap();
    });
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    waiting.join().expect("the watcher ends");
    let rejection = rejected(seen.recv_timeout(DEADLINE).expect("the answer arrives"));
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(rejection.message, "A turn is running; rewind once it ends.");
}

#[test]
fn a_rewind_after_close_is_closing_and_writes_nothing() {
    let mut session = Session::new(vec![Scripted::text("done")], None);
    let before = log_bytes(&session.dir);
    let inbox = take_inbox(&mut session);
    inbox.send(Delivery::Close(ignore())).unwrap();
    let (rw, answered) = rewind(args(None, None, false, vec![]));
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
    let (rw, answered) = rewind(args(None, None, false, vec![]));
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
    let (rw, answered) = rewind(args(None, None, false, vec![]));
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
        match rx.recv_timeout(DEADLINE).expect("refused in time") {
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
    let (rw, answered) = rewind(args(None, None, false, vec![]));
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
    let (rw, answered) = rewind(args(Some("s_test"), None, false, vec![]));
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
        let (rw, answered) = rewind(args(Some(from), None, false, vec![]));
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
                    }),
                    None,
                    None,
                )
                .unwrap();
        }
        let before = log_bytes(&session.dir);
        let inbox = take_inbox(&mut session);
        let (rw, answered) = rewind(args(Some(from), None, false, vec![]));
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
    let (rw, answered) = rewind(args(Some(A_ID), Some(first - 1), false, vec![]));
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
    assert_eq!(last.payload["jobs"], json!([]));
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
    let (rw, answered) = rewind(args(Some(A_ID), Some(second), false, vec![]));
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
    let (rw, answered) = rewind(args(Some(A_ID), None, false, vec![]));
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
fn a_rewind_asking_for_a_summary_fails_before_any_point() {
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
    let (rw, answered) = rewind(args(Some(A_ID), None, true, vec![]));
    let looped = drive_closed(looped, tx, vec![rw]);
    drop(looped);
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::SummaryFailed);
    assert_eq!(
        rejection.message,
        "Summaries are not built in this Fiber yet."
    );
    assert_eq!(log_bytes(&dir), before);
}

#[test]
fn a_rewind_while_a_job_runs_is_busy() {
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
    let looped = looped
        .jobs(Arc::new(StillRunning(vec![JobId("j_1".into())])) as Arc<dyn contract::jobs::Jobs>);
    let before = log_bytes(&dir);
    let (rw, answered) = rewind(args(None, None, false, vec![]));
    let looped = drive_closed(looped, tx, vec![rw]);
    drop(looped);
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::Busy);
    assert_eq!(
        rejection.message,
        "Jobs are running; stop them before rewinding."
    );
    assert_eq!(log_bytes(&dir), before);
}

#[test]
fn a_rewind_adopting_a_job_that_is_not_running_is_stale() {
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
    let (rw, answered) = rewind(args(None, None, false, vec!["j_x"]));
    let looped = drive_closed(looped, tx, vec![rw]);
    drop(looped);
    let rejection = rejected(answer_of(answered));
    assert_eq!(rejection.code, ErrorCode::StaleRequest);
    assert_eq!(rejection.message, "The adopted jobs are not running: j_x.");
    assert_eq!(log_bytes(&dir), before);
}
