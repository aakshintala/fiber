//! Forks inherit the logged preamble and history, but admit their own first
//! task through the ordinary turn path (`docs/delegates.md`, "Forks").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::events::{CacheLifetime, LoadedExtension};
use contract::events::{Event, JobStarted, Parent, SessionNamed, TurnOutcome};
use contract::hook::{BeforeMessageAnswer, BeforeMessageOutcome, Hooks, Replaced};
use contract::inbox::{Ack, Delivery};
use contract::provider::Input;
use contract::shapes::{ContentPart, Point, Worktree};
use contract::tool::Tool;
use contract::{ErrorCode, JobId, Seq, SessionId};
use fakes::{Deadline, Scripted, ScriptedProvider};
use r#loop::{
    Continuation, Loop, Model, Permissions, PromptInputs, PromptPatch, ReloadPrepared,
    ReloadSettings, Rewound, fork_task,
};
use serde_json::{Value, json};
use support::{
    DEADLINE, MODEL, Session, StartHooks, TestTool, calls_reply, delivery, kinds, paris,
    start_added,
};

const FORK: &str = "s_fork";
const FORK_TURN: &[&str] = &[
    "session_started",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

fn task() -> String {
    fork_task(&SessionId("s_test".into()), 1, "check it", "")
}

fn tool(name: &'static str) -> Arc<dyn Tool> {
    Arc::new(TestTool::declaring(name, "done", Vec::new(), None))
}

fn parent() -> Session {
    let mut session = Session::with_tools(
        vec![
            Scripted::text("one-done"),
            calls_reply("working", &[("test_tool", paris())]),
            Scripted::text("two-done"),
        ],
        None,
        vec![tool("test_tool")],
    );
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session
}

// Each point is immediately before a logged model request.
fn point(dir: &std::path::Path, request: usize) -> Point {
    let lines = log::read(dir).unwrap();
    let line = lines
        .iter()
        .filter(|line| line.kind == "assistant_message_started")
        .nth(request)
        .unwrap();
    Point {
        session_id: line.session_id.clone(),
        seq: Seq(line.seq.unwrap().0 - 1),
    }
}

fn start(
    session: &Session,
    id: &str,
    from: Point,
    how: Continuation,
    worktree: Option<Worktree>,
    tools: Vec<Arc<dyn Tool>>,
    script: Vec<Scripted>,
) -> (Loop, mpsc::Sender<Delivery>, Arc<ScriptedProvider>, PathBuf) {
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock: Arc<dyn contract::clock::Clock> = session.clock.clone();
    let log = Arc::new(log::Log::create(&home, SessionId(id.into()), clock.clone()).unwrap());
    let dir = log.dir().to_path_buf();
    let provider = Arc::new(ScriptedProvider::new(script));
    let (tx, rx) = mpsc::channel();
    let mut prompt = PromptInputs::new(
        home,
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let looped = Loop::rewound(
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
            tools: tools
                .into_iter()
                .map(|tool| ("builtin".into(), tool))
                .collect(),
            permissions: Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules: session.rules.clone(),
            },
        },
        Rewound {
            factory: None,
            from,
            worktree,
            how,
        },
    )
    .unwrap();
    (looped, tx, provider, dir)
}

fn fork(
    session: &Session,
    from: Point,
    tools: Vec<Arc<dyn Tool>>,
    script: Vec<Scripted>,
) -> (Loop, mpsc::Sender<Delivery>, Arc<ScriptedProvider>, PathBuf) {
    let parent = Parent {
        session_id: from.session_id.clone(),
        delegate_id: JobId("j_fork".into()),
    };
    start(
        session,
        FORK,
        from,
        Continuation::Fork { parent },
        None,
        tools,
        script,
    )
}

fn turn(mut looped: Loop, tx: &mpsc::Sender<Delivery>, text: &str) -> Loop {
    let mut prompt = support::message(text);
    prompt.sender.command_id = Some(contract::CommandId("c_fork_task".into()));
    tx.send(Delivery::Prompt(prompt, support::ignore()))
        .unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn();
        done.send((looped, outcome)).unwrap();
    });
    let (looped, outcome) =
        Deadline::after(DEADLINE).recv_or_fail(&finished, "the fork turn ended");
    assert_eq!(outcome.unwrap(), Some(TurnOutcome::Completed));
    looped
}

fn user(input: &Input) -> &str {
    match input {
        Input::User { text, .. } => text,
        other @ (Input::Assistant { .. }
        | Input::Reasoning { .. }
        | Input::ToolCall { .. }
        | Input::ToolResult { .. }
        | Input::ToolSearchResult { .. }) => panic!("expected user input, got {other:?}"),
    }
}

fn definitions(dir: &std::path::Path, reason: &str) -> Vec<serde_json::Map<String, Value>> {
    let lines = log::read(dir).unwrap();
    lines
        .iter()
        .rev()
        .find(|line| line.kind == "preamble_built" && line.payload["reason"] == reason)
        .unwrap()
        .payload["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["definition"].as_object().unwrap().clone())
        .collect()
}

#[test]
fn a_fork_writes_parent_and_forked_from_and_no_rewind_before_its_prompt() {
    for worktree in [None, Some(worktree())] {
        let mut session = Session::new(
            vec![Scripted::text("one-done"), Scripted::text("two-done")],
            None,
        );
        session.inbox.send(delivery("one")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        session
            .log
            .append(
                &Event::SessionNamed(SessionNamed {
                    name: Some("parent name".into()),
                    by: contract::events::NamedBy::Person,
                }),
                None,
                None,
            )
            .unwrap();
        session.inbox.send(delivery("two")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let from = point(&session.dir, 1);
        let (b, tx, _, dir) = start(
            &session,
            FORK,
            from.clone(),
            Continuation::Fork {
                parent: Parent {
                    session_id: SessionId("s_test".into()),
                    delegate_id: JobId("j_fork".into()),
                },
            },
            worktree.clone(),
            Vec::new(),
            vec![Scripted::text("fork-done")],
        );
        let lines = log::read(&dir).unwrap();
        assert_eq!(kinds(&lines), ["session_started"]);
        assert_eq!(
            lines[0].payload["parent"],
            json!({"session_id":"s_test", "delegate_id":"j_fork"})
        );
        assert_eq!(
            lines[0].payload["forked_from"],
            json!({"session_id":"s_test", "seq": from.seq.0})
        );
        assert!(lines[0].payload.get("rewind").is_none());
        assert_eq!(
            lines[0].payload.get("worktree").cloned(),
            worktree.map(|w| serde_json::to_value(w).unwrap())
        );
        let _b = turn(b, &tx, &task());
        let lines = log::read(&dir).unwrap();
        assert_eq!(
            kinds(&lines),
            [
                "session_started",
                "turn_started",
                "step_started",
                "assistant_message_started",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed"
            ]
        );
        assert!(
            lines
                .iter()
                .all(|line| line.session_id == SessionId(FORK.into()))
        );
        let old = log::read(&session.dir).unwrap();
        assert!(
            lines
                .iter()
                .all(|line| !old.iter().any(|parent| parent == line))
        );
        assert!(!lines.iter().any(|line| {
            serde_json::to_string(&line.payload)
                .unwrap()
                .contains("\"one\"")
        }));
    }
}

fn worktree() -> Worktree {
    Worktree {
        path: "/w/fork".into(),
        branch: "fork".into(),
        repository: Some("/w".into()),
    }
}

#[test]
fn a_forks_first_request_is_the_parents_through_its_history_then_its_first_turn() {
    let session = parent();
    let own = tool("own_tool");
    let own_definition = own.definition();
    let (b, tx, provider, dir) = fork(
        &session,
        point(&session.dir, 2),
        vec![own],
        vec![Scripted::text("fork-done")],
    );
    let _b = turn(b, &tx, &task());
    let requests = provider.requests();
    let first = &requests[0];
    let parent = &session.provider.requests()[2];
    assert_eq!(first.system_prompt, parent.system_prompt);
    assert_eq!(first.thinking, parent.thinking);
    assert_eq!(first.tool_choice, parent.tool_choice);
    assert_eq!(first.cache_lifetime, parent.cache_lifetime);
    assert_eq!(first.previous_end, parent.previous_end);
    assert_eq!(first.cache_key, "s_test");
    let k = parent.conversation.len();
    assert_eq!(first.conversation[..k], parent.conversation[..]);
    assert_eq!(first.conversation.len(), k + 1);
    assert_eq!(user(&first.conversation[k]), task());
    assert_eq!(
        first.sent_tools.as_ref().unwrap(),
        &definitions(&session.dir, "start")
    );
    assert_eq!(first.tools, vec![own_definition]);
    let lines = log::read(&dir).unwrap();
    assert_eq!(kinds(&lines), FORK_TURN);
    let input = &lines
        .iter()
        .find(|line| line.kind == "turn_started")
        .unwrap()
        .payload["input"];
    assert_eq!(input[0]["content"][0]["text"], task());
}

#[test]
fn a_fork_of_a_rewound_parent_keys_on_the_original_root() {
    let session = parent();
    let (mid, tx, _, mid_dir) = start(
        &session,
        "s_mid",
        point(&session.dir, 0),
        Continuation::Rewind {
            note: "continued".into(),
            summary: None,
            name: None,
        },
        None,
        Vec::new(),
        vec![Scripted::text("mid-done")],
    );
    let _mid = turn(mid, &tx, "middle");
    let (b, tx, provider, _) = fork(
        &session,
        point(&mid_dir, 0),
        Vec::new(),
        vec![Scripted::text("fork-done")],
    );
    let _b = turn(b, &tx, &task());
    assert_eq!(provider.requests()[0].cache_key, "s_test");
}

#[test]
fn a_session_start_context_sits_between_the_prefix_and_fork_task() {
    let session = parent();
    let hooks = StartHooks::new();
    hooks.answer(start_added("fiber.test/start", "hook says hi"));
    let (b, tx, provider, dir) = fork(
        &session,
        point(&session.dir, 2),
        Vec::new(),
        vec![Scripted::text("fork-done")],
    );
    let _b = turn(b.hooks(hooks), &tx, &task());
    let parent = &session.provider.requests()[2];
    let first = &provider.requests()[0];
    let k = parent.conversation.len();
    assert_eq!(first.conversation[..k], parent.conversation[..]);
    assert_eq!(first.conversation.len(), k + 2);
    assert_eq!(user(&first.conversation[k]), "hook says hi");
    assert_eq!(user(&first.conversation[k + 1]), task());
    let lines = log::read(&dir).unwrap();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "context_added",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed"
        ]
    );
    assert_eq!(lines[1].payload["hook"], "session_start");
    assert_eq!(lines[1].payload["text"], "hook says hi");
    assert_eq!(lines[2].payload["input"][0]["content"][0]["text"], task());
}

#[test]
fn an_isolated_forks_workspace_notice_follows_start_context_without_changing_the_prefix() {
    let session = parent();
    let hooks = StartHooks::new();
    hooks.answer(start_added("fiber.test/start", "hook says hi"));
    let (b, tx, provider, dir) = fork(
        &session,
        point(&session.dir, 2),
        Vec::new(),
        vec![Scripted::text("fork-done")],
    );
    let notice = r#loop::fork_workspace("/fork", "/parent");
    let task = fork_task(&SessionId("s_test".into()), 1, "check it", &notice);
    let _b = turn(b.hooks(hooks), &tx, &task);
    let parent = &session.provider.requests()[2];
    let first = &provider.requests()[0];
    let k = parent.conversation.len();
    // First-turn context stays between the inherited prefix and task.
    assert_eq!(first.conversation[..k], parent.conversation[..]);
    assert_eq!(first.conversation.len(), k + 2);
    assert_eq!(user(&first.conversation[k]), "hook says hi");
    assert_eq!(user(&first.conversation[k + 1]), task);
    let lines = log::read(&dir).unwrap();
    assert_eq!(
        kinds(&lines),
        [
            "session_started",
            "context_added",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed"
        ]
    );
    assert_eq!(lines[2].payload["input"][0]["content"][0]["text"], task);
}

struct MessageHooks(Mutex<Option<BeforeMessageAnswer>>);

impl Hooks for MessageHooks {
    fn before_message(&self, _: &contract::hook::BeforeMessage<'_>) -> BeforeMessageAnswer {
        self.0.lock().unwrap().take().unwrap()
    }
    fn after_tool(&self, _: &contract::hook::AfterToolCall<'_>) -> contract::hook::AfterToolAnswer {
        contract::hook::AfterToolAnswer {
            outcome: contract::hook::AfterToolOutcome::Unchanged,
            changed_by: Vec::new(),
            notices: Vec::new(),
        }
    }
    fn before_tool(
        &self,
        _: &contract::hook::BeforeToolCall<'_>,
    ) -> contract::hook::BeforeToolAnswer {
        contract::hook::BeforeToolAnswer {
            outcome: contract::hook::BeforeToolOutcome::Unchanged,
            notices: Vec::new(),
        }
    }
    fn check_tool(&self, _: &contract::hook::BeforeToolCall<'_>) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }
    fn session_start(&self, _: &contract::hook::SessionStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }
    fn turn_start(&self, _: &contract::hook::TurnStart<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }
    fn before_model_call(
        &self,
        _: &contract::hook::BeforeModelCall<'_>,
    ) -> contract::hook::CheckAnswer {
        contract::hook::CheckAnswer {
            outcome: contract::hook::CheckOutcome::Passed,
            notices: Vec::new(),
        }
    }
    fn turn_end(&self, _: &contract::hook::TurnEnd<'_>) -> contract::hook::ContextAnswer {
        contract::hook::ContextAnswer {
            outcome: contract::hook::ContextOutcome::Added(Vec::new()),
            notices: Vec::new(),
        }
    }
    fn before_handoff(
        &self,
        _: &contract::hook::BeforeHandoff<'_>,
    ) -> contract::hook::BeforeHandoffAnswer {
        fakes::no_note()
    }
    fn deliver_to(&self, _: mpsc::Sender<Delivery>) {}
}

#[test]
fn a_before_message_replacement_replaces_the_fork_task() {
    let session = parent();
    let hooks = Arc::new(MessageHooks(Mutex::new(Some(BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Passed {
            replaced: Some(Replaced {
                content: vec![ContentPart::Text {
                    text: "replaced task".into(),
                }],
                changed_by: vec!["fiber.test/scrub".into()],
            }),
            context: Vec::new(),
        },
        notices: Vec::new(),
    }))));
    let (b, tx, provider, dir) = fork(
        &session,
        point(&session.dir, 2),
        Vec::new(),
        vec![Scripted::text("fork-done")],
    );
    let _b = turn(b.hooks(hooks), &tx, &task());
    let requests = provider.requests();
    let first = &requests[0];
    let parent = &session.provider.requests()[2];
    let k = parent.conversation.len();
    assert_eq!(first.conversation[..k], parent.conversation[..]);
    assert_eq!(first.conversation.len(), k + 1);
    assert_eq!(user(&first.conversation[k]), "replaced task");
    let lines = log::read(&dir).unwrap();
    assert_eq!(kinds(&lines), FORK_TURN);
    let input = &lines
        .iter()
        .find(|line| line.kind == "turn_started")
        .unwrap()
        .payload["input"];
    assert_eq!(input.as_array().unwrap().len(), 1);
    assert_eq!(input[0]["type"], "message");
    assert_eq!(
        input[0]["content"],
        json!([{"type":"text", "text":"replaced task"}])
    );
    assert_eq!(input[0]["changed_by"], json!(["fiber.test/scrub"]));
    assert!(
        !lines.iter().any(|line| {
            serde_json::to_string(&line.payload)
                .unwrap()
                .contains("you are a fork of session")
        }),
        "{lines:?}"
    );
    assert!(!format!("{requests:?}").contains("you are a fork of session"));
}

#[test]
fn a_before_message_refusal_sends_nothing_and_logs_nothing() {
    let session = parent();
    let hooks = Arc::new(MessageHooks(Mutex::new(Some(BeforeMessageAnswer {
        outcome: BeforeMessageOutcome::Refused(contract::hook::Refusal {
            extension: "fiber.test/guard".into(),
            reason: "no way".into(),
        }),
        notices: Vec::new(),
    }))));
    let (b, tx, provider, dir) = fork(&session, point(&session.dir, 2), Vec::new(), Vec::new());
    let mut b = b.hooks(hooks);
    let (acked, ack) = mpsc::channel();
    let mut prompt = support::message(&task());
    prompt.sender.command_id = Some(contract::CommandId("c_fork_task".into()));
    tx.send(Delivery::Prompt(
        prompt,
        Ack(Box::new(move |answer| acked.send(answer).unwrap())),
    ))
    .unwrap();
    tx.send(Delivery::Close(support::ignore())).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        done.send(b.turn()).unwrap();
    });
    let deadline = Deadline::after(DEADLINE);
    let rejection = deadline
        .recv_or_fail(&ack, "the fork task was refused")
        .unwrap_err();
    assert_eq!(rejection.code, ErrorCode::MessageRefused);
    assert_eq!(
        rejection.message,
        "Extension fiber.test/guard refused the message: no way"
    );
    assert_eq!(
        deadline
            .recv_or_fail(&finished, "the refused fork closed")
            .unwrap(),
        None
    );
    assert_eq!(kinds(&log::read(&dir).unwrap()), ["session_started"]);
    assert!(provider.requests().is_empty());
}

#[test]
fn a_job_the_parent_left_running_is_no_orphan_in_the_fork() {
    let mut session = Session::new(
        vec![Scripted::text("one-done"), Scripted::text("two-done")],
        None,
    );
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session
        .log
        .append(
            &Event::JobStarted(JobStarted {
                job_id: JobId("j_orphan".into()),
                tool: None,
                extension: None,
                description: "running".into(),
                output_path: "jobs/j_orphan".into(),
            }),
            None,
            None,
        )
        .unwrap();
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let (b, tx, provider, dir) = fork(
        &session,
        point(&session.dir, 1),
        Vec::new(),
        vec![Scripted::text("fork-done")],
    );
    assert_eq!(kinds(&log::read(&dir).unwrap()), ["session_started"]);
    let _b = turn(b, &tx, &task());
    let lines = log::read(&dir).unwrap();
    assert_eq!(kinds(&lines), FORK_TURN);
    assert!(!lines.iter().any(|line| line.kind == "job_completed"));
    assert!(!format!("{:?}", provider.requests()[0].conversation).contains("j_orphan"));
    assert_eq!(provider.requests().len(), 1);
}

fn prepared() -> ReloadPrepared {
    ReloadPrepared {
        prompt: PromptPatch {
            system: Some("Reloaded system bytes.".into()),
            append: None,
            extensions: Vec::new(),
            extension_dirs: Vec::new(),
            extension_sections: Vec::new(),
            skills_disabled: Vec::new(),
            cache_lifetime: CacheLifetime::FiveMinutes,
            deferrals: Default::default(),
        },
        tools: vec![("builtin".into(), tool("probe"))],
        extensions: vec![LoadedExtension {
            name: "fresh".into(),
            version: "2".into(),
            replaces: Vec::new(),
        }],
        notices: Vec::new(),
        server_sync: r#loop::ServerSync {
            servers: contract::events::ReloadedServers {
                kept: Vec::new(),
                restarted: Vec::new(),
                started: Vec::new(),
                stopped: Vec::new(),
            },
            server_records: Vec::new(),
            server_failures: Vec::new(),
            new_prompts: None,
        },
        hooks: None,
        reviewer: Err(contract::shapes::Failure {
            code: ErrorCode::NoModel,
            message: "no reviewer".into(),
            retry_after_ms: None,
            provider: None,
        }),
        reviewer_observe: None,
        credential_files: Vec::new(),
        settings: ReloadSettings {
            reviewer_notes: "new notes".into(),
            limits: Default::default(),
            retry: Default::default(),
            handoff: Default::default(),
            budget: None,
            idle_exit: None,
            warm_cap: None,
        },
        installed: Box::new(|| {}),
    }
}

#[test]
fn a_fork_after_a_reload_sends_the_reloaded_tools_and_runs_one() {
    for after_point in [false, true] {
        let mut session = Session::new(
            vec![
                Scripted::text("one-done"),
                Scripted::text("two-done"),
                Scripted::text("three-done"),
            ],
            None,
        );
        session.inbox.send(delivery("one")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        session.looped = Some(
            session
                .looped
                .take()
                .unwrap()
                .reloader(Arc::new(|_| Ok(prepared()))),
        );
        if after_point {
            session.inbox.send(delivery("two")).unwrap();
            assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        }
        session
            .inbox
            .send(Delivery::Reload(support::ignore()))
            .unwrap();
        session.inbox.send(delivery("reloaded")).unwrap();
        assert_eq!(session.turn(), Some(TurnOutcome::Completed));
        let own = tool("probe");
        let own_definition = own.definition();
        let (b, tx, provider, dir) = fork(
            &session,
            point(&session.dir, 1),
            vec![own],
            vec![
                calls_reply("probe it", &[("probe", paris())]),
                Scripted::text("fork-done"),
            ],
        );
        let _b = turn(b, &tx, &task());
        let first = &provider.requests()[0];
        let reason = if after_point { "start" } else { "reload" };
        assert_eq!(
            first.sent_tools.as_ref().unwrap(),
            &definitions(&session.dir, reason)
        );
        assert_eq!(
            first
                .sent_tools
                .as_ref()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "probe"),
            !after_point
        );
        assert_eq!(first.tools, vec![own_definition]);
        let lines = log::read(&dir).unwrap();
        assert_eq!(
            kinds(&lines),
            [
                "session_started",
                "turn_started",
                "step_started",
                "assistant_message_started",
                "text_completed",
                "tool_call_requested",
                "usage_recorded",
                "assistant_message_completed",
                "tool_call_started",
                "tool_call_completed",
                "step_started",
                "assistant_message_started",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
            ]
        );
        let completion = lines
            .iter()
            .find(|line| line.kind == "tool_call_completed")
            .unwrap();
        assert_eq!(completion.payload["status"], "completed");
    }
}
