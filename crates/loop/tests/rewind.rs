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
use std::sync::{Arc, mpsc};
use std::thread;

use contract::events::TurnOutcome;
use contract::inbox::Delivery;
use contract::provider::Input;
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::shapes::{ContentPart, Origin, Point, Sender, Worktree};
use contract::tool::Tool;
use contract::{CommandId, Envelope, Seq, SessionId};
use fakes::{Scripted, ScriptedProvider};
use log::Log;
use r#loop::{Loop, Model, Permissions, PromptInputs, Rewound, rewind_note};
use serde_json::{Value, json};

use support::{DEADLINE, MODEL, Session, TestTool, calls_reply, delivery, ignore, kinds};

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

fn paris() -> Value {
    json!({"city": "Paris"})
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
        log,
        Rewound {
            from,
            note,
            worktree,
        },
        provider.clone(),
        Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
        prompt,
        rx,
        tools,
        Permissions {
            workspace: session.workspace.display().to_string(),
            credentials: session.credentials.clone(),
            credential_files: Vec::new(),
            rules,
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
        log,
        folded,
        next_provider.clone(),
        Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
        prompt,
        rx,
        vec![
            (
                "builtin".to_owned(),
                Arc::clone(&write_tool()) as Arc<dyn Tool>,
            ),
            (
                "builtin".to_owned(),
                Arc::clone(&exec_tool()) as Arc<dyn Tool>,
            ),
        ],
        Permissions {
            workspace: session.workspace.display().to_string(),
            credentials: session.credentials.clone(),
            credential_files: Vec::new(),
            rules,
        },
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
        b_log,
        Rewound {
            from: Point {
                session_id: SessionId("s_model00000001".into()),
                seq: Seq(1),
            },
            note: String::new(),
            worktree: None,
        },
        provider.clone(),
        Model {
            reference: "fake/model-9".into(),
            cost: None,
            subscription: false,
        },
        prompt,
        rx,
        Vec::new(),
        Permissions {
            workspace: "/w".into(),
            credentials: home.clone(),
            credential_files: Vec::new(),
            rules,
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
