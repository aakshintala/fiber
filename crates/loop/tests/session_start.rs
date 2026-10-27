//! `session_start` at a resume, a suspended resume and a rewind
//! (`docs/extensions.md`, "The hook points"): once per process, with the
//! reason it started, its contexts appended after the rebuilt conversation.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::sync::{Arc, mpsc};
use std::thread;

use contract::events::TurnOutcome;
use contract::inbox::Delivery;
use contract::provider::Input;
use contract::rules::{Rule, RuleDecision, StandingRules};
use contract::tool::Tool;
use contract::{Envelope, Seq, SessionId};
use fakes::{Deadline, Scripted, ScriptedProvider};
use support::{DEADLINE, Session, StartHooks, delivery, start_added, start_failed};

fn hook(hooks: Arc<StartHooks>, session: &mut Session) {
    session.looped = session
        .looped
        .take()
        .map(|looped| looped.hooks(hooks as Arc<dyn contract::hook::Hooks>));
}

/// Runs the session's turn, keeping the loop's error instead of unwrapping
/// it, so a hook failure is asserted and not panicked on.
fn turn_raw(session: &mut Session) -> Result<Option<TurnOutcome>, r#loop::Error> {
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn();
        done.send((looped, outcome)).unwrap();
    });
    let (looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the turn ended in time");
    session.looped = Some(looped);
    outcome
}

/// The user texts of `conversation`, in order, skipping anything else.
fn user_texts(conversation: &[Input]) -> Vec<String> {
    conversation
        .iter()
        .filter_map(|input| match input {
            Input::User { text, .. } => Some(text.clone()),
            Input::Assistant { .. }
            | Input::Reasoning { .. }
            | Input::ToolCall { .. }
            | Input::ToolResult { .. }
            | Input::ToolSearchResult { .. } => None,
        })
        .collect()
}

fn standing(tool: &str, decision: RuleDecision, prefix: &str) -> StandingRules {
    StandingRules {
        global: vec![Rule {
            decision,
            tool: tool.into(),
            prefix: prefix.into(),
            added: None,
            session_id: None,
        }],
        project: Vec::new(),
    }
}

#[test]
fn a_resumed_session_runs_session_start_again_with_reason_resume() {
    let mut session = Session::new(
        vec![Scripted::text("First."), Scripted::text("Second.")],
        None,
    );
    let hooks = StartHooks::new();
    hooks.answer(start_added("fiber.test/inbox", "at the start"));
    hooks.answer(start_added("fiber.test/inbox", "on resume"));
    hook(Arc::clone(&hooks), &mut session);
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let workspace = session.workspace.display().to_string();
    assert_eq!(
        hooks.seen(),
        vec![("Start".to_owned(), "s_test".to_owned(), workspace.clone())]
    );

    session.resume(Vec::new());
    hook(Arc::clone(&hooks), &mut session);
    // A resume that takes no turn runs no hook.
    assert_eq!(hooks.calls(), 1);
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls(), 2);
    assert_eq!(
        hooks.seen()[1],
        ("Resume".to_owned(), "s_test".to_owned(), workspace)
    );
    drop(session.looped.take());

    // The resume's context appends after the rebuilt conversation and
    // before the prompt: the request equals the rebuild of the durable
    // lines.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    let opening = user_texts(&requests[0].conversation)[0].clone();
    assert_eq!(
        user_texts(&requests[1].conversation),
        vec![
            opening,
            "at the start".to_owned(),
            "one".to_owned(),
            "on resume".to_owned(),
            "two".to_owned(),
        ]
    );
}

/// A session suspended on an approval, with `script` answering the calls
/// after the resume: the first turn parks on the `shell` ask and the
/// process exits on it, as a shutdown does.
fn suspended_session(script: Vec<Scripted>) -> (Session, Arc<support::TestTool>) {
    let ask = support::shell(Some("npm publish"), None);
    let mut session = Session::with_tools(script, None, vec![ask.clone() as Arc<dyn Tool>]);
    session
        .rules
        .set(standing("shell", RuleDecision::Ask, "npm publish"));
    session.inbox.send(delivery("hi")).unwrap();
    let inbox = session.inbox.clone();
    let cancel = Arc::clone(&session.cancel);
    let asked = support::on_request(&session, move |_| {
        // As the door's stopper does: the shutdown, then the wake.
        cancel.shutdown(143);
        inbox.send(Delivery::Cancelled).unwrap();
    });
    // The wait ends with no turn outcome, on the pending request.
    assert_eq!(session.turn(), None);
    asked.join().unwrap();
    r#loop::fiber_exited(
        &session.log,
        session.looped.as_ref().unwrap().summary(),
        Ok(()),
        true,
        Some(143),
    )
    .unwrap();
    (session, ask)
}

/// Answers the re-raised approval once the resumed turn raises it again.
fn allow_on_reraise(session: &Session) -> thread::JoinHandle<()> {
    let inbox = session.inbox.clone();
    support::on_request(session, move |request| {
        inbox
            .send(support::reply_delivery(request, support::allow()))
            .unwrap();
    })
}

fn new_kinds(session: &Session, before: usize) -> Vec<String> {
    support::kinds(&log::read(&session.dir).unwrap()[before..])
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_suspended_resume_writes_a_passing_hook_context_after_the_batch_results() {
    let (mut session, _) = suspended_session(vec![
        support::calls_reply("", &[("shell", support::paris())]),
        Scripted::text("Done."),
    ]);
    let tools = || vec![support::shell(Some("npm publish"), None) as Arc<dyn Tool>];
    session.resume(tools());
    let hooks = StartHooks::new();
    hooks.answer(start_added("fiber.test/inbox", "resumed context"));
    hook(Arc::clone(&hooks), &mut session);
    let lines_before = log::read(&session.dir).unwrap().len();
    let allowed = allow_on_reraise(&session);
    assert_eq!(
        turn_raw(&mut session).unwrap(),
        Some(TurnOutcome::Completed)
    );
    allowed.join().unwrap();
    assert_eq!(
        hooks.seen()[0].0,
        "Resume",
        "the suspended finish runs the hook: {:?}",
        hooks.seen()
    );

    // No `context_added` sits between the call and its result: the
    // contexts wait for the next step, after the results and after
    // `step_started`.
    assert_eq!(
        new_kinds(&session, lines_before),
        vec![
            "preamble_built",
            "permission_requested",
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "context_added",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let context = log::read(&session.dir).unwrap()[lines_before..]
        .iter()
        .find(|line| line.kind == "context_added")
        .unwrap()
        .clone();
    assert_eq!(context.payload["hook"], "session_start");
    assert_eq!(context.payload["text"], "resumed context");

    // The step's request holds the context after the results: the live
    // conversation equals the rebuild of the final log.
    let requests = session.requests();
    assert_eq!(requests.len(), 2);
    let conversation = &requests[1].conversation;
    assert!(matches!(&conversation[2], Input::ToolCall { .. }));
    assert!(matches!(&conversation[3], Input::ToolResult { .. }));
    assert_eq!(user_texts(conversation)[2], "resumed context");
    drop(session.looped.take());
}

#[test]
fn a_blocking_session_start_failure_at_a_suspended_resume_keeps_the_suspension() {
    let (mut session, _) = suspended_session(vec![
        support::calls_reply("", &[("shell", support::paris())]),
        Scripted::text("Done."),
        Scripted::text("Finished."),
    ]);
    let tools = || vec![support::shell(Some("npm publish"), None) as Arc<dyn Tool>];
    session.resume(tools());
    let hooks = StartHooks::new();
    hooks.answer(start_failed("fiber.test/guard", "boom"));
    hooks.answer(start_added("fiber.test/inbox", "second try"));
    hook(Arc::clone(&hooks), &mut session);
    let lines_before = log::read(&session.dir).unwrap().len();
    let Err(error) = turn_raw(&mut session) else {
        panic!("the finish fails");
    };
    assert_eq!(error.code(), contract::ErrorCode::HookFailed);
    assert_eq!(
        error.to_string(),
        "The `session_start` hook of extension fiber.test/guard failed, \
         so the session did not start: boom"
    );
    // The suspension is intact: the rebuilt preamble and the re-raised
    // request were written, none of the suspended calls processed.
    assert_eq!(
        new_kinds(&session, lines_before),
        vec!["preamble_built", "permission_requested"]
    );
    // The dead process's exit reads its summary, not the log: snapshot it
    // before the loop is dropped.
    let summary = session.looped.as_ref().unwrap().summary().clone();
    drop(session.looped.take());

    // As the failed process's exit does: the suspension is recorded
    // again, so the next resume finds it.
    r#loop::fiber_exited(
        &session.log,
        &summary,
        Err(contract::shapes::Failure {
            code: error.code(),
            message: error.to_string(),
            retry_after_ms: None,
            provider: None,
        }),
        true,
        None,
    )
    .unwrap();
    // The next resume runs the hook again and finishes the turn.
    session.resume(tools());
    hook(Arc::clone(&hooks), &mut session);
    let allowed = allow_on_reraise(&session);
    assert_eq!(
        turn_raw(&mut session).unwrap(),
        Some(TurnOutcome::Completed)
    );
    allowed.join().unwrap();
    assert_eq!(hooks.calls(), 2);
    drop(session.looped.take());
}

#[test]
fn a_shutdown_inside_the_hook_at_a_suspended_resume_finishes_nothing() {
    let (mut session, _) = suspended_session(vec![
        support::calls_reply("", &[("shell", support::paris())]),
        Scripted::text("Done."),
    ]);
    let tools = || vec![support::shell(Some("npm publish"), None) as Arc<dyn Tool>];
    session.resume(tools());
    let hooks = StartHooks::new();
    hooks.answer(start_added("fiber.test/inbox", "late"));
    hook(Arc::clone(&hooks), &mut session);
    let (signalled, parked) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    hooks.pause(signalled, wait);
    let lines_before = log::read(&session.dir).unwrap().len();
    let mut looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn();
        done.send((looped, outcome)).unwrap();
    });
    Deadline::after(DEADLINE)
        .recv(&parked)
        .expect("the hook parked");
    session.cancel.shutdown(143);
    release.send(()).unwrap();
    let (looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the finish ended in time");
    session.looped = Some(looped);
    assert_eq!(outcome.unwrap(), None);
    // None of the suspended calls processed, and the log is still
    // suspended: the rebuilt preamble and the re-raised request.
    assert_eq!(
        new_kinds(&session, lines_before),
        vec!["preamble_built", "permission_requested"]
    );
    drop(session.looped.take());
}

#[test]
fn a_rewound_loop_runs_session_start_with_reason_rewind() {
    let mut session = Session::new(
        vec![
            Scripted::text("First."),
            Scripted::text("Second."),
            Scripted::text("Third."),
        ],
        None,
    );
    let hooks = StartHooks::new();
    hooks.answer(start_added("fiber.test/inbox", "at the start"));
    hooks.answer(start_added("fiber.test/inbox", "after the rewind"));
    hook(Arc::clone(&hooks), &mut session);
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls(), 1);
    drop(session.looped.take());

    // B continues A's session from turn 2's start, as `rewind` does.
    let lines: Vec<Envelope> = log::read(&session.dir).unwrap();
    let mut starts = lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .map(|line| line.seq.unwrap().0);
    starts.next().unwrap();
    let from = contract::shapes::Point {
        session_id: SessionId("s_test".into()),
        seq: Seq(starts.next().unwrap() - 1),
    };
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock = Arc::clone(&session.clock) as Arc<dyn contract::clock::Clock>;
    let log = Arc::new(
        log::Log::create(
            &home,
            contract::SessionId("s_rewound".into()),
            clock.clone(),
        )
        .unwrap(),
    );
    let provider = Arc::new(ScriptedProvider::new(vec![Scripted::text("Third.")]));
    let (tx, rx) = mpsc::channel();
    let mut prompt = r#loop::PromptInputs::new(
        home,
        "/bin/sh".into(),
        log.dir().join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let mut rewound = r#loop::Loop::rewound(
        r#loop::Session {
            log,
            provider: provider.clone(),
            model: r#loop::Model {
                reference: support::MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: Vec::new(),
            permissions: r#loop::Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules: session.rules.clone(),
            },
        },
        r#loop::Rewound {
            factory: None,
            from,
            worktree: None,
            how: r#loop::Continuation::Rewind {
                note: "continued".into(),
                summary: None,
                name: r#loop::latest_name(&session.dir),
            },
        },
    )
    .unwrap()
    .hooks(Arc::clone(&hooks) as Arc<dyn contract::hook::Hooks>);
    tx.send(delivery("three")).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = rewound.turn().unwrap();
        done.send((rewound, outcome)).unwrap();
    });
    let (_, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls(), 2);
    assert_eq!(hooks.seen()[1].0, "Rewind");
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let texts = user_texts(&requests[0].conversation);
    assert_eq!(texts[texts.len() - 2], "after the rewind");
    assert_eq!(texts[texts.len() - 1], "three");
}

#[test]
fn a_forked_loop_runs_session_start_with_reason_fork() {
    let mut session = Session::new(
        vec![
            Scripted::text("First."),
            Scripted::text("Second."),
            Scripted::text("Third."),
        ],
        None,
    );
    let hooks = StartHooks::new();
    hooks.answer(start_added("fiber.test/inbox", "at the start"));
    hooks.answer(start_added("fiber.test/inbox", "after the fork"));
    hook(Arc::clone(&hooks), &mut session);
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    session.inbox.send(delivery("two")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls(), 1);
    drop(session.looped.take());

    // B continues A's session from turn 2's start, as `rewind` does.
    let lines: Vec<Envelope> = log::read(&session.dir).unwrap();
    let mut starts = lines
        .iter()
        .filter(|line| line.kind == "turn_started")
        .map(|line| line.seq.unwrap().0);
    starts.next().unwrap();
    let from = contract::shapes::Point {
        session_id: SessionId("s_test".into()),
        seq: Seq(starts.next().unwrap() - 1),
    };
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock = Arc::clone(&session.clock) as Arc<dyn contract::clock::Clock>;
    let log = Arc::new(
        log::Log::create(
            &home,
            contract::SessionId("s_rewound".into()),
            clock.clone(),
        )
        .unwrap(),
    );
    let provider = Arc::new(ScriptedProvider::new(vec![Scripted::text("Third.")]));
    let (tx, rx) = mpsc::channel();
    let mut prompt = r#loop::PromptInputs::new(
        home,
        "/bin/sh".into(),
        log.dir().join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let mut rewound = r#loop::Loop::rewound(
        r#loop::Session {
            log,
            provider: provider.clone(),
            model: r#loop::Model {
                reference: support::MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools: Vec::new(),
            permissions: r#loop::Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules: session.rules.clone(),
            },
        },
        r#loop::Rewound {
            factory: None,
            from,
            worktree: None,
            how: r#loop::Continuation::Fork {
                parent: contract::events::Parent {
                    session_id: SessionId("s_test".into()),
                    delegate_id: contract::JobId("j_fork".into()),
                },
            },
        },
    )
    .unwrap()
    .hooks(Arc::clone(&hooks) as Arc<dyn contract::hook::Hooks>);
    let task = r#loop::fork_task(&SessionId("s_test".into()), 1, "check it", "");
    tx.send(delivery(&task)).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = rewound.turn().unwrap();
        done.send((rewound, outcome)).unwrap();
    });
    let (_, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(hooks.calls(), 2);
    assert_eq!(hooks.seen()[1].0, "Fork");
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    let texts = user_texts(&requests[0].conversation);
    assert_eq!(texts[texts.len() - 2], "after the fork");
    assert_eq!(texts[texts.len() - 1], task);
}
