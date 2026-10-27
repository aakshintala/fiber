//! What the session loads once its offer step has run, before its first
//! preamble (`docs/extensions.md`, "Offering"): the post-offer load runs
//! once per process, writes its events before `preamble_built`, and never
//! runs in a process that started with an inherited preamble.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::commands::{Reply, ReplyAnswer};
use contract::events::{
    Clients, Event, McpServerFailed, Notice, OfferDecision, OfferedItem, OfferedKind, ServerFailure,
};
use contract::inbox::{Ack, Answer, Delivery};
use contract::provider::Input;
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::shapes::Failure;
use contract::{ErrorCode, RequestId};
use fakes::Deadline;
use fakes::{Scripted, ScriptedProvider};
use log::Watcher;
use serde_json::json;

use support::{
    DEADLINE, MODEL, Session, TestTool, delivery, ignore, read_until, spawn_run, tool_call_reply,
};

/// A fake repository: a fixed list of unapproved items.
#[derive(Default)]
struct Fake {
    list: Mutex<Vec<Unapproved>>,
    log: Mutex<Option<Arc<log::Log>>>,
    /// After this many `unapproved` calls, append `Clients { count: 0 }`
    /// before answering: the last client disconnects mid-offer.
    drop_clients_after: Mutex<Option<usize>>,
    gathers: AtomicUsize,
}

impl Fake {
    fn new(list: Vec<Unapproved>) -> Arc<Self> {
        Arc::new(Self {
            list: Mutex::new(list),
            ..Self::default()
        })
    }

    fn set_log(&self, log: &Arc<log::Log>) {
        *self.log.lock().unwrap() = Some(Arc::clone(log));
    }
}

impl RepositoryCode for Fake {
    fn file(&self, _: &str) -> Result<String, contract::shapes::Failure> {
        Err(contract::shapes::Failure {
            code: contract::ErrorCode::IoFailed,
            message: "This fake does not read offered files.".into(),
            retry_after_ms: None,
            provider: None,
        })
    }
    fn unapproved(&self) -> Result<Vec<Unapproved>, Failure> {
        let n = self.gathers.fetch_add(1, Ordering::SeqCst) + 1;
        if self.drop_clients_after.lock().unwrap().as_ref() == Some(&n)
            && let Some(log) = self.log.lock().unwrap().clone()
        {
            log.append(&Event::Clients(Clients { count: 0 }), None, None)
                .unwrap();
        }
        Ok(self.list.lock().unwrap().clone())
    }

    fn decide(&self, _: &OfferedItem, _: OfferDecision) -> Result<Decided, Failure> {
        Ok(Decided::Recorded)
    }
}

fn item(kind: OfferedKind, name: &str, required: bool) -> Unapproved {
    Unapproved {
        offered: OfferedItem {
            files: vec![],
            kind,
            name: name.to_owned(),
            hash: format!("h_{name}"),
            required,
            summary: format!("{name}: what an install shows"),
            version: None,
            diff: None,
        },
        never: false,
    }
}

fn server(name: &str) -> Unapproved {
    item(OfferedKind::McpServer, name, false)
}

fn key_of(name: &str) -> r#loop::OfferKey {
    r#loop::OfferKey {
        kind: OfferedKind::McpServer,
        name: name.to_owned(),
        hash: format!("h_{name}"),
    }
}

/// A session whose loop reads `code`, with a `clients` line counting
/// `clients` when there is one.
fn session(code: &Arc<Fake>, clients: Option<u32>, script: Vec<Scripted>) -> Session {
    let mut session = Session::new(script, None);
    let code: Arc<dyn RepositoryCode> = Arc::clone(code) as Arc<dyn RepositoryCode>;
    session.looped = Some(session.looped.take().unwrap().repository_code(code));
    if let Some(count) = clients {
        session
            .log
            .append(&Event::Clients(Clients { count }), None, None)
            .unwrap();
    }
    session
}

fn with_load(session: &mut Session, load: r#loop::LoadApproved) {
    session.looped = Some(session.looped.take().unwrap().approved_code(load));
}

/// A closure that counts its calls, records its outcome, and returns
/// `tools` and `events` (or `failure`).
fn loader(
    calls: &Arc<AtomicUsize>,
    seen: &Arc<Mutex<Option<r#loop::OfferOutcome>>>,
    tools: Vec<(String, Arc<dyn contract::tool::Tool>)>,
    events: Vec<Event>,
    failure: Option<Failure>,
) -> r#loop::LoadApproved {
    let calls = Arc::clone(calls);
    let seen = Arc::clone(seen);
    Arc::new(move |outcome: &r#loop::OfferOutcome| {
        calls.fetch_add(1, Ordering::SeqCst);
        *seen.lock().unwrap() = Some(outcome.clone());
        match failure.clone() {
            Some(failure) => Err(failure),
            None => Ok(r#loop::Approved {
                tools: tools.clone(),
                events: events.clone(),
            }),
        }
    })
}

fn counter() -> (Arc<AtomicUsize>, Arc<Mutex<Option<r#loop::OfferOutcome>>>) {
    (Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(None)))
}

#[track_caller]
fn ended(finished: &mpsc::Receiver<Result<(), r#loop::Error>>) -> Result<(), r#loop::Error> {
    Deadline::after(DEADLINE)
        .recv(finished)
        .expect("run ended in time")
}

fn decisions(list: &[OfferDecision]) -> ReplyAnswer {
    ReplyAnswer::Decisions {
        decisions: list.to_vec(),
    }
}

fn send_reply(
    session: &Session,
    request: &RequestId,
    answer: ReplyAnswer,
) -> mpsc::Receiver<Answer> {
    let (tx, rx) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Reply(
            Reply {
                request_id: request.clone(),
                answer,
            },
            Ack(Box::new(move |answer| {
                tx.send(answer).unwrap_or(());
            })),
        ))
        .unwrap();
    rx
}

fn close(session: &Session) {
    session.inbox.send(Delivery::Close(ignore())).unwrap();
}

fn until_kind(watcher: Watcher, kind: &'static str) -> (Watcher, Vec<contract::Envelope>) {
    read_until(watcher, kind, move |line| line.kind == kind)
}

fn request_of(line: &contract::Envelope) -> RequestId {
    RequestId(line.payload["request_id"].as_str().unwrap().to_owned())
}

fn durable_kinds(session: &Session) -> Vec<String> {
    log::read(&session.dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect()
}

/// The ordered kinds under test, with the lines the contract writes on
/// their own trigger set aside (`docs/testing.md`, "Event streams"):
/// `clients`, `session_status` and `attention`.
fn ordered<'a>(kinds: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    kinds
        .into_iter()
        .filter(|kind| *kind != "clients" && *kind != "session_status" && *kind != "attention")
        .collect()
}

fn failed_server(server: &str) -> Event {
    Event::McpServerFailed(McpServerFailed {
        server: server.to_owned(),
        reason: ServerFailure::StartFailed,
        will_restart: false,
        error: Failure {
            code: ErrorCode::McpServerUnavailable,
            message: format!("The MCP server `{server}` failed to start: refused."),
            retry_after_ms: None,
            provider: None,
        },
    })
}

fn loaded_tool(name: &'static str) -> (String, Arc<dyn contract::tool::Tool>) {
    (
        "db".to_owned(),
        Arc::new(TestTool::reads(name, "Loaded.")) as Arc<dyn contract::tool::Tool>,
    )
}

#[test]
fn an_offer_answered_approve_loads_before_the_preamble() {
    let code = Fake::new(vec![server("db")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let (calls, seen) = counter();
    with_load(
        &mut session,
        loader(
            &calls,
            &seen,
            vec![loaded_tool("late_tool")],
            vec![failed_server("db")],
            None,
        ),
    );
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let rx = send_reply(&session, &request, decisions(&[OfferDecision::Approve]));
    assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
    let (_, lines) = until_kind(watcher, "turn_completed");
    assert!(lines.iter().any(|l| l.kind == "preamble_built"));
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The log order: the resolved offer, then the loaded events, then the
    // first preamble.
    let kinds = durable_kinds(&session);
    let resolved = kinds
        .iter()
        .position(|k| k == "repository_code_resolved")
        .unwrap();
    let failed = kinds.iter().position(|k| k == "mcp_server_failed").unwrap();
    let built = kinds.iter().position(|k| k == "preamble_built").unwrap();
    assert!(resolved < failed && failed < built, "{kinds:?}");
    // The complete ordered list: one offer, the loaded failure, then
    // the first preamble, so a duplicated, missing or reordered event
    // fails (`docs/testing.md`, "Event streams").
    assert_eq!(
        ordered(kinds.iter().map(String::as_str)),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "mcp_server_failed",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    // The first provider request declares the loaded tool.
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].tools.iter().any(|t| t.name == "late_tool"),
        "{:?}",
        requests[0]
            .tools
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );
    let outcome = seen.lock().unwrap().clone().unwrap();
    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.reported, vec![key_of("db")]);
}

#[test]
fn a_rejected_reply_skips_nothing_and_a_retry_approves_both() {
    struct FailOnce {
        list: Mutex<Vec<Unapproved>>,
        failed: AtomicBool,
    }
    impl RepositoryCode for FailOnce {
        fn file(&self, _: &str) -> Result<String, contract::shapes::Failure> {
            Err(contract::shapes::Failure {
                code: contract::ErrorCode::IoFailed,
                message: "This fake does not read offered files.".into(),
                retry_after_ms: None,
                provider: None,
            })
        }
        fn unapproved(&self) -> Result<Vec<Unapproved>, Failure> {
            Ok(self.list.lock().unwrap().clone())
        }
        fn decide(&self, item: &OfferedItem, decision: OfferDecision) -> Result<Decided, Failure> {
            // The first approval of `b` fails, so the reply carrying
            // `a`'s skip is rejected; the retry approves both.
            if item.name == "b"
                && decision == OfferDecision::Approve
                && !self.failed.swap(true, Ordering::SeqCst)
            {
                return Err(Failure {
                    code: ErrorCode::McpServerUnavailable,
                    message: "b refused the approval.".to_owned(),
                    retry_after_ms: None,
                    provider: None,
                });
            }
            Ok(Decided::Recorded)
        }
    }
    let code = Arc::new(FailOnce {
        list: Mutex::new(vec![server("a"), server("b")]),
        failed: AtomicBool::new(false),
    });
    let mut session = Session::new(vec![Scripted::text("Hello.")], None);
    session.looped = Some(
        session
            .looped
            .take()
            .unwrap()
            .repository_code(Arc::clone(&code) as Arc<dyn RepositoryCode>),
    );
    session
        .log
        .append(&Event::Clients(Clients { count: 1 }), None, None)
        .unwrap();
    let (calls, seen) = counter();
    with_load(
        &mut session,
        loader(
            &calls,
            &seen,
            vec![loaded_tool("late_tool")],
            Vec::new(),
            None,
        ),
    );
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    // Skip `a`, approve `b`: `b`'s approval fails, so the reply is
    // rejected and `a` is not skipped.
    let first = send_reply(
        &session,
        &request,
        decisions(&[OfferDecision::Skip, OfferDecision::Approve]),
    );
    assert!(Deadline::after(DEADLINE).recv(&first).unwrap().is_err());
    // The retry approves both: `a` loads instead of staying skipped.
    let second = send_reply(
        &session,
        &request,
        decisions(&[OfferDecision::Approve, OfferDecision::Approve]),
    );
    assert!(
        Deadline::after(DEADLINE).recv(&second).unwrap().is_ok(),
        "the retry resolves the offer"
    );
    let (_, lines) = until_kind(watcher, "turn_completed");
    assert!(lines.iter().any(|l| l.kind == "preamble_built"));
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let outcome = seen.lock().unwrap().clone().unwrap();
    assert!(outcome.skipped.is_empty(), "{outcome:?}");
    assert_eq!(outcome.reported, vec![key_of("a"), key_of("b")]);
    // One offer line: the rejected reply reuses the pending offer, and
    // the retry resolves it, so nothing is offered twice.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].tools.iter().any(|t| t.name == "late_tool"),
        "a loads: the first request declares the loaded tool"
    );
}

#[test]
fn headless_with_nothing_to_offer_loads_once_before_the_preamble() {
    let code = Fake::new(Vec::new());
    let mut session = session(&code, None, vec![Scripted::text("Hello.")]);
    let (calls, _) = counter();
    with_load(
        &mut session,
        loader(
            &calls,
            &Arc::new(Mutex::new(None)),
            vec![loaded_tool("late_tool")],
            Vec::new(),
            None,
        ),
    );
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (_, lines) = until_kind(watcher, "turn_completed");
    assert!(lines.iter().any(|l| l.kind == "preamble_built"));
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.iter().any(|t| t.name == "late_tool"));
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn two_turns_load_once() {
    let code = Fake::new(Vec::new());
    let mut session = session(
        &code,
        None,
        vec![Scripted::text("One."), Scripted::text("Two.")],
    );
    let (calls, _) = counter();
    with_load(
        &mut session,
        loader(
            &calls,
            &Arc::new(Mutex::new(None)),
            Vec::new(),
            Vec::new(),
            None,
        ),
    );
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("one")).unwrap();
    let (watcher, _) = until_kind(watcher, "turn_completed");
    session.inbox.send(delivery("two")).unwrap();
    let (_, _) = until_kind(watcher, "turn_completed");
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Both turns in order: the second builds no preamble.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_failing_load_fails_the_run_before_any_request() {
    let code = Fake::new(Vec::new());
    let mut session = session(&code, None, vec![Scripted::text("Hello.")]);
    let (calls, _) = counter();
    with_load(
        &mut session,
        loader(
            &calls,
            &Arc::new(Mutex::new(None)),
            Vec::new(),
            Vec::new(),
            Some(Failure {
                code: ErrorCode::McpRequiredServerFailed,
                message: "The required MCP server `db` failed.".into(),
                retry_after_ms: None,
                provider: None,
            }),
        ),
    );
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let r#loop::Error::RepositoryCode(failure) = ended(&finished).unwrap_err() else {
        panic!("the failing load fails the run");
    };
    assert_eq!(failure.code, ErrorCode::McpRequiredServerFailed);
    assert!(session.provider.requests().is_empty(), "no model request");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The failing load writes nothing: no preamble, no turn.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        ["session_started"]
    );
}

#[test]
fn a_loaded_tool_never_replaces_a_registered_one() {
    let registered = Arc::new(TestTool::reads("same", "Old."));
    let mut session = Session::open(
        vec![tool_call_reply("", &["same"]), Scripted::text("Hello.")],
        Vec::new(),
        vec![Arc::clone(&registered) as Arc<dyn contract::tool::Tool>],
        r#loop::Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
    );
    let code = Fake::new(Vec::new());
    let code: Arc<dyn RepositoryCode> = code as Arc<dyn RepositoryCode>;
    session.looped = Some(session.looped.take().unwrap().repository_code(code));
    let (calls, _) = counter();
    with_load(
        &mut session,
        loader(
            &calls,
            &Arc::new(Mutex::new(None)),
            vec![
                (
                    "db".to_owned(),
                    Arc::new(TestTool::reads("same", "New.")) as Arc<dyn contract::tool::Tool>,
                ),
                (
                    "db".to_owned(),
                    Arc::new(TestTool::reads("fresh", "Fresh.")) as Arc<dyn contract::tool::Tool>,
                ),
            ],
            Vec::new(),
            None,
        ),
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let lines = session.lines();
    // The complete ordered list: the tool step calls the registered
    // tool, then the text step answers, so a dropped, added or
    // reordered event fails (`docs/testing.md`, "Event streams").
    assert_eq!(
        ordered(lines.iter().map(|line| line.kind.as_str())),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "tool_call_arguments_delta",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = lines
        .into_iter()
        .find(|line| line.kind == "preamble_built")
        .unwrap();
    assert!(built.payload.get("replaced").is_none());
    let declared = built.payload["tools"].as_array().unwrap();
    let same = declared.iter().find(|t| t["name"] == "same").unwrap();
    assert_eq!(same["registered_by"], "builtin");
    let fresh = declared.iter().find(|t| t["name"] == "fresh").unwrap();
    assert_eq!(fresh["registered_by"], "db");
    // The registered tool answers the call, and the fresh tool is
    // declared to the model alongside it.
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].tools.iter().any(|t| t.name == "same"));
    assert!(requests[0].tools.iter().any(|t| t.name == "fresh"));
    match requests[1].conversation.last() {
        Some(Input::ToolResult {
            text,
            is_error: false,
            ..
        }) => assert_eq!(text, "Old."),
        other => panic!("the second request carries the registered tool's result: {other:?}"),
    }
}

#[test]
fn the_post_offer_load_receives_the_held_names() {
    let registered = Arc::new(TestTool::reads("same", "Old."));
    let mut session = Session::open(
        vec![Scripted::text("Hello.")],
        Vec::new(),
        vec![Arc::clone(&registered) as Arc<dyn contract::tool::Tool>],
        r#loop::Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
    );
    let code = Fake::new(Vec::new());
    let code: Arc<dyn RepositoryCode> = code as Arc<dyn RepositoryCode>;
    session.looped = Some(session.looped.take().unwrap().repository_code(code));
    let (calls, seen) = counter();
    with_load(
        &mut session,
        loader(&calls, &seen, Vec::new(), Vec::new(), None),
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // The loader sees the names registered before the load, so a later
    // load can leave their declared rows alone.
    let seen = seen.lock().unwrap();
    let outcome = seen.as_ref().unwrap();
    assert_eq!(outcome.held, ["same".to_owned()]);
    // The complete ordered list: a text-only turn after the load, so a
    // dropped, added or reordered event fails (`docs/testing.md`,
    // "Event streams").
    assert_eq!(
        ordered(session.lines().iter().map(|line| line.kind.as_str())),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn two_loaded_tools_of_one_name_replace_in_order() {
    let mut session = Session::open(
        vec![Scripted::text("Hello.")],
        Vec::new(),
        Vec::new(),
        r#loop::Model {
            reference: MODEL.into(),
            cost: None,
            subscription: false,
        },
    );
    let code = Fake::new(Vec::new());
    let code: Arc<dyn RepositoryCode> = code as Arc<dyn RepositoryCode>;
    session.looped = Some(session.looped.take().unwrap().repository_code(code));
    let (calls, _) = counter();
    with_load(
        &mut session,
        loader(
            &calls,
            &Arc::new(Mutex::new(None)),
            vec![
                (
                    "db".to_owned(),
                    Arc::new(TestTool::reads("dup", "First.")) as Arc<dyn contract::tool::Tool>,
                ),
                (
                    "db2".to_owned(),
                    Arc::new(TestTool::reads("dup", "Second.")) as Arc<dyn contract::tool::Tool>,
                ),
            ],
            Vec::new(),
            None,
        ),
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert_eq!(
        ordered(lines.iter().map(|line| line.kind.as_str())),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = lines
        .into_iter()
        .find(|line| line.kind == "preamble_built")
        .unwrap();
    // A collision inside the load still replaces and records: only
    // names held before the load stand.
    assert_eq!(
        built.payload["replaced"],
        json!([{"name": "dup", "from": "db", "to": "db2"}])
    );
    let declared = built.payload["tools"].as_array().unwrap();
    let dup = declared.iter().find(|t| t["name"] == "dup").unwrap();
    assert_eq!(dup["registered_by"], "db2");
}

#[test]
fn without_a_load_nothing_changes() {
    let code = Fake::new(Vec::new());
    let mut session = session(&code, None, vec![Scripted::text("Hello.")]);
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let lines = session.lines();
    assert!(
        lines.iter().any(|l| l.kind == "preamble_built"),
        "the preamble still builds"
    );
    assert_eq!(
        ordered(lines.iter().map(|line| line.kind.as_str())),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn a_resumed_loop_loads_once_before_its_first_preamble() {
    let code = Fake::new(Vec::new());
    let mut session = session(
        &code,
        None,
        vec![Scripted::text("Hello."), Scripted::text("Again.")],
    );
    let (calls, _) = counter();
    with_load(
        &mut session,
        loader(
            &calls,
            &Arc::new(Mutex::new(None)),
            Vec::new(),
            Vec::new(),
            None,
        ),
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // A new process over the same log loads again, once, writing its
    // events before its own first preamble.
    session.resume(Vec::new());
    let resumed_code = Fake::new(Vec::new());
    let resumed_code: Arc<dyn RepositoryCode> = resumed_code as Arc<dyn RepositoryCode>;
    let notice = Event::Notice(Notice {
        code: ErrorCode::RepositoryCodeSkipped,
        message: "resumed load".into(),
        extension: None,
    });
    session.looped = Some(
        session
            .looped
            .take()
            .unwrap()
            .repository_code(resumed_code)
            .approved_code(loader(
                &calls,
                &Arc::new(Mutex::new(None)),
                Vec::new(),
                vec![notice],
                None,
            )),
    );
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // The resumed load's event lands before its own first preamble: the
    // notice is ephemeral, so it reads from the event stream, which holds
    // the resumed turn's lines.
    assert_eq!(
        durable_kinds(&session)
            .iter()
            .filter(|k| *k == "preamble_built")
            .count(),
        2
    );
    // The stream drains one turn at a time: turn one's lines, then the
    // resumed turn's, whose notice precedes its preamble.
    let first: Vec<String> = session.lines().iter().map(|l| l.kind.clone()).collect();
    assert_eq!(
        ordered(first.iter().map(String::as_str)),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let kinds: Vec<String> = session.lines().iter().map(|l| l.kind.clone()).collect();
    // The resumed turn carries the load's notice before its own first
    // preamble, with no new session start.
    assert_eq!(
        ordered(kinds.iter().map(String::as_str)),
        [
            "fiber_started",
            "notice",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let built = kinds.iter().position(|k| k == "preamble_built").unwrap();
    let notice_at = kinds.iter().position(|k| k == "notice").unwrap();
    assert!(notice_at < built, "{kinds:?}");
}

#[test]
fn the_load_sees_skips_reports_and_resumed_skips() {
    let code = Fake::new(vec![server("db"), server("o2")]);
    let mut session = session(
        &code,
        Some(1),
        vec![Scripted::text("Hello."), Scripted::text("Again.")],
    );
    let (calls, seen) = counter();
    with_load(
        &mut session,
        loader(&calls, &seen, Vec::new(), Vec::new(), None),
    );
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let rx = send_reply(
        &session,
        &request,
        decisions(&[OfferDecision::Skip, OfferDecision::Never]),
    );
    assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    close(&session);
    ended(&finished).unwrap();
    // A skipped item is in `skipped` and `reported`; an approved or never
    // one is in `reported` only.
    let outcome = seen.lock().unwrap().clone().unwrap();
    assert_eq!(outcome.skipped, vec![key_of("db")]);
    assert_eq!(outcome.reported, vec![key_of("db"), key_of("o2")]);
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    // The never is recorded, so a resumed offer sees only the skip: drop
    // it from the fake as the recorded decision would.
    code.list.lock().unwrap().retain(|u| u.offered.name == "db");
    // A resumed loop over the log passes the skip with no new offer.
    let folded = r#loop::resumed(&session.dir).unwrap();
    assert_eq!(folded.skipped(), vec![key_of("db")]);
    session.resume(Vec::new());
    session.looped = Some(
        session
            .looped
            .take()
            .unwrap()
            .repository_code(Arc::clone(&code) as Arc<dyn RepositoryCode>)
            .approved_code(loader(&calls, &seen, Vec::new(), Vec::new(), None)),
    );
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let outcome = seen.lock().unwrap().clone().unwrap();
    assert_eq!(outcome.skipped, vec![key_of("db")]);
    // Both processes in order: the resumed turn builds its own
    // preamble, with no new session start.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_started",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn headless_a_noted_item_is_reported_but_not_skipped() {
    let code = Fake::new(vec![server("db")]);
    let mut session = session(&code, None, vec![Scripted::text("Hello.")]);
    let (calls, seen) = counter();
    with_load(
        &mut session,
        loader(&calls, &seen, Vec::new(), Vec::new(), None),
    );
    session.inbox.send(delivery("go")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let outcome = seen.lock().unwrap().clone().unwrap();
    assert!(outcome.skipped.is_empty());
    assert_eq!(outcome.reported, vec![key_of("db")]);
    let lines = session.lines();
    assert!(
        lines.iter().any(|l| l.kind == "notice"),
        "the headless notice is written"
    );
    assert_eq!(
        ordered(lines.iter().map(|line| line.kind.as_str())),
        [
            "session_started",
            "notice",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
}

#[test]
fn pre_existing_never_is_reported_and_fails_nothing() {
    for required in [false, true] {
        let mut unapproved = server("db");
        unapproved.never = true;
        unapproved.offered.required = required;
        let code = Fake::new(vec![unapproved]);
        let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
        let (calls, seen) = counter();
        with_load(
            &mut session,
            loader(&calls, &seen, Vec::new(), Vec::new(), None),
        );
        session.inbox.send(delivery("go")).unwrap();
        assert_eq!(
            session.turn(),
            Some(contract::events::TurnOutcome::Completed),
            "required: {required}"
        );
        // No offer is raised for it, and the run does not fail.
        assert!(
            log::read(&session.dir)
                .unwrap()
                .iter()
                .all(|l| l.kind != "repository_code_offered"),
            "required: {required}"
        );
        let outcome = seen.lock().unwrap().clone().unwrap();
        assert_eq!(outcome.reported, vec![key_of("db")], "required: {required}");
        let lines = session.lines();
        assert_eq!(
            lines.iter().filter(|l| l.kind == "clients").count(),
            1,
            "one clients line per attach: {required}"
        );
        assert_eq!(
            ordered(lines.iter().map(|line| line.kind.as_str())),
            [
                "session_started",
                "preamble_built",
                "opening_message",
                "turn_started",
                "step_started",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
            ],
            "required: {required}"
        );
    }
}

#[test]
fn a_disconnect_right_after_a_skip_notices_and_fails_nothing() {
    for required in [false, true] {
        let code = Fake::new(vec![item(OfferedKind::McpServer, "db", required)]);
        *code.drop_clients_after.lock().unwrap() = Some(2);
        let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
        code.set_log(&session.log);
        let (calls, seen) = counter();
        with_load(
            &mut session,
            loader(&calls, &seen, Vec::new(), Vec::new(), None),
        );
        let watcher = session.log.watch();
        let finished = spawn_run(&mut session);
        session.inbox.send(delivery("go")).unwrap();
        let (watcher, lines) = until_kind(watcher, "repository_code_offered");
        let request = request_of(lines.last().unwrap());
        let rx = send_reply(&session, &request, decisions(&[OfferDecision::Skip]));
        assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
        // The next round's check runs headless over the same content: no
        // second notice, and a skipped `required` item does not fail.
        let (_, _) = until_kind(watcher, "turn_completed");
        close(&session);
        ended(&finished).unwrap();
        // No second notice for the skipped content: notices are
        // ephemeral, so they read from the event stream.
        let lines = session.lines();
        assert!(
            lines.iter().all(|l| l.kind != "notice"),
            "required: {required}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.kind == "clients").count(),
            2,
            "the attach and the mid-offer disconnect: {required}"
        );
        assert_eq!(
            ordered(lines.iter().map(|line| line.kind.as_str())),
            [
                "session_started",
                "repository_code_offered",
                "repository_code_resolved",
                "preamble_built",
                "opening_message",
                "turn_started",
                "step_started",
                "assistant_message_started",
                "assistant_message_delta",
                "assistant_message_delta",
                "text_completed",
                "usage_recorded",
                "assistant_message_completed",
                "turn_completed",
            ],
            "required: {required}"
        );
        let outcome = seen.lock().unwrap().clone().unwrap();
        assert_eq!(outcome.skipped, vec![key_of("db")], "required: {required}");
    }
}

#[test]
fn a_headless_resume_over_a_skip_notices_and_fails_nothing() {
    let code = Fake::new(vec![item(OfferedKind::McpServer, "db", true)]);
    let mut session = session(
        &code,
        Some(1),
        vec![Scripted::text("Hello."), Scripted::text("Again.")],
    );
    let (calls, seen) = counter();
    with_load(
        &mut session,
        loader(&calls, &seen, Vec::new(), Vec::new(), None),
    );
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let rx = send_reply(&session, &request, decisions(&[OfferDecision::Skip]));
    assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    close(&session);
    ended(&finished).unwrap();
    // Resume with no client over the same content: no notice, no failure.
    session
        .log
        .append(&Event::Clients(Clients { count: 0 }), None, None)
        .unwrap();
    session.resume(Vec::new());
    session.looped = Some(
        session
            .looped
            .take()
            .unwrap()
            .repository_code(Arc::clone(&code) as Arc<dyn RepositoryCode>)
            .approved_code(loader(&calls, &seen, Vec::new(), Vec::new(), None)),
    );
    session.inbox.send(delivery("again")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    // The stream drains one turn at a time: neither turn notices the
    // skipped content again.
    let first = session.lines();
    let second = session.lines();
    assert_eq!(
        first.iter().filter(|l| l.kind == "clients").count(),
        1,
        "one clients line per attach"
    );
    assert_eq!(
        ordered(first.iter().map(|l| l.kind.as_str())),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    assert_eq!(
        second.iter().filter(|l| l.kind == "clients").count(),
        1,
        "the resume writes its own clients line"
    );
    assert_eq!(
        ordered(second.iter().map(|l| l.kind.as_str())),
        [
            "fiber_started",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    for lines in [first, second] {
        assert!(
            lines.iter().all(|l| l.kind != "notice"),
            "no second notice for the skipped content: {}",
            lines
                .iter()
                .map(|l| l.kind.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let outcome = seen.lock().unwrap().clone().unwrap();
    assert_eq!(outcome.skipped, vec![key_of("db")]);
}

#[test]
fn a_headless_resume_over_a_changed_required_item_fails() {
    // Control: the same item with a changed hash still fails the resumed
    // run with its kind's code. The warmup runs over an empty offer so no
    // reply is needed; the changed item lands only for the resume.
    let warm = Fake::new(Vec::new());
    let mut session = session(
        &warm,
        Some(1),
        vec![Scripted::text("Warmup."), Scripted::text("Hello.")],
    );
    // A real completed turn first, so the resume folds a whole session.
    session.inbox.send(delivery("warmup")).unwrap();
    assert_eq!(
        session.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let mut changed = item(OfferedKind::McpServer, "db", true);
    changed.offered.hash.push_str("-new");
    let code = Fake::new(vec![changed]);
    // The log holds a resolved skip of the old content.
    session
        .log
        .append(
            &Event::RepositoryCodeOffered(contract::events::RepositoryCodeOffered {
                request_id: RequestId("r_old".into()),
                items: vec![contract::events::OfferedItem {
                    files: vec![],
                    kind: OfferedKind::McpServer,
                    name: "db".into(),
                    hash: "h_db".into(),
                    required: true,
                    summary: String::new(),
                    version: None,
                    diff: None,
                }],
            }),
            None,
            None,
        )
        .unwrap();
    session
        .log
        .append(
            &Event::RepositoryCodeResolved(contract::events::RepositoryCodeResolved {
                request_id: RequestId("r_old".into()),
                decisions: vec![OfferDecision::Skip],
            }),
            None,
            None,
        )
        .unwrap();
    session
        .log
        .append(&Event::Clients(Clients { count: 0 }), None, None)
        .unwrap();
    let (calls, _) = counter();
    session.resume(Vec::new());
    session.looped = Some(
        session
            .looped
            .take()
            .unwrap()
            .repository_code(Arc::clone(&code) as Arc<dyn RepositoryCode>)
            .approved_code(loader(
                &calls,
                &Arc::new(Mutex::new(None)),
                Vec::new(),
                Vec::new(),
                None,
            )),
    );
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let r#loop::Error::RepositoryCode(failure) = ended(&finished).unwrap_err() else {
        panic!("the changed required item fails the resumed run");
    };
    assert_eq!(failure.code, ErrorCode::McpServerUnapproved);
    // The warmup turn, then the resumed run's offer and its exit line:
    // a changed required item fails before any preamble.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "repository_code_offered",
            "repository_code_resolved",
            "fiber_started",
        ]
    );
}

/// A rewound loop over a log holding a preamble build, with the counting
/// load attached: the load never runs in that process.
fn start_rewound(
    session: &Session,
    script: Vec<Scripted>,
    calls: &Arc<AtomicUsize>,
) -> (
    r#loop::Loop,
    mpsc::Sender<Delivery>,
    Arc<ScriptedProvider>,
    std::path::PathBuf,
) {
    let lines = log::read(&session.dir).unwrap();
    let at = lines.last().unwrap().seq.unwrap().0;
    assert!(
        lines.iter().any(|l| l.kind == "preamble_built"),
        "the parent log holds a build"
    );
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock = session.clock.clone();
    let log = Arc::new(
        log::Log::create(
            &home,
            contract::SessionId("s_rewound1".into()),
            clock.clone(),
        )
        .unwrap(),
    );
    let dir = log.dir().to_path_buf();
    let provider = Arc::new(ScriptedProvider::new(script));
    let (tx, rx) = mpsc::channel();
    let clock: Arc<dyn contract::clock::Clock> = clock;
    let mut prompt = r#loop::PromptInputs::new(
        home,
        "/bin/sh".into(),
        dir.join("events.jsonl").display().to_string(),
        clock,
        fakes::CONTEXT_WINDOW,
    );
    prompt.credential = Some("work".into());
    let tools: Vec<(String, Arc<dyn contract::tool::Tool>)> = Vec::new();
    let note = r#loop::rewind_note(&session.dir, contract::Seq(at)).unwrap();
    let looped = r#loop::Loop::rewound(
        r#loop::Session {
            log,
            provider: Arc::clone(&provider) as Arc<dyn contract::provider::Provider>,
            model: r#loop::Model {
                reference: MODEL.into(),
                cost: None,
                subscription: false,
            },
            prompt,
            inbox: rx,
            tools,
            permissions: r#loop::Permissions {
                workspace: session.workspace.display().to_string(),
                credentials: session.credentials.clone(),
                credential_files: Vec::new(),
                rules: session.rules.clone(),
            },
        },
        r#loop::Rewound {
            factory: None,
            from: contract::shapes::Point {
                session_id: contract::SessionId("s_test".into()),
                seq: contract::Seq(at),
                project: None,
            },
            worktree: None,
            how: r#loop::Continuation::Rewind {
                note,
                summary: None,
                name: r#loop::latest_name(&session.dir),
            },
        },
    )
    .unwrap()
    .approved_code(loader(
        calls,
        &Arc::new(Mutex::new(None)),
        vec![loaded_tool("late_tool")],
        Vec::new(),
        None,
    ));
    (looped, tx, provider, dir)
}

fn run_turn(mut looped: r#loop::Loop, tx: &mpsc::Sender<Delivery>, text: &str) -> r#loop::Loop {
    tx.send(delivery(text)).unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    let (looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(contract::events::TurnOutcome::Completed));
    looped
}

fn tool_names(request: &contract::provider::ModelRequest) -> Vec<String> {
    request.tools.iter().map(|t| t.name.clone()).collect()
}

#[test]
fn a_rewound_session_loads_nothing() {
    let mut parent = Session::new(vec![Scripted::text("one-done")], None);
    parent.inbox.send(delivery("one")).unwrap();
    assert_eq!(
        parent.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let baseline = tool_names(&parent.provider.requests()[0]);
    let (calls, _) = counter();
    let (rewound, tx, provider, dir) =
        start_rewound(&parent, vec![Scripted::text("three-done")], &calls);
    let _rewound = run_turn(rewound, &tx, "three");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let kinds: Vec<String> = log::read(&dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert!(
        !kinds.iter().any(|k| k == "preamble_built"),
        "no build in this process: {kinds:?}"
    );
    // The inherited build runs with no new preamble.
    assert_eq!(
        ordered(kinds.iter().map(String::as_str)),
        [
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    // The first request sends the logged build's tools verbatim, without
    // the load's.
    let names = tool_names(&provider.requests()[0]);
    assert_eq!(names, baseline);
    assert!(!names.iter().any(|n| n == "late_tool"));
}

#[test]
fn a_switch_before_the_first_prompt_loads_nothing() {
    let mut parent = Session::new(vec![Scripted::text("one-done")], None);
    parent.inbox.send(delivery("one")).unwrap();
    assert_eq!(
        parent.turn(),
        Some(contract::events::TurnOutcome::Completed)
    );
    let (calls, _) = counter();
    let (rewound, tx, provider, dir) =
        start_rewound(&parent, vec![Scripted::text("three-done")], &calls);
    // A switch while idle rebuilds the preamble, without the load: a
    // switch to the same settings would write nothing, so this one names
    // another model.
    let b_provider = Arc::clone(&provider);
    let prepare: r#loop::Prepare = Arc::new(move |args, _, _| {
        Ok(r#loop::Prepared {
            provider: Arc::clone(&b_provider) as Arc<dyn contract::provider::Provider>,
            model: r#loop::Model {
                reference: args.model.clone(),
                cost: None,
                subscription: false,
            },
            thinking: None,
            chosen: None,
            credential: Some("work".into()),
            cache_lifetime: contract::events::CacheLifetime::OneHour,
            context_window: fakes::CONTEXT_WINDOW,
            addendum: None,
            handoff: r#loop::HandoffSettings::default(),
            reviewer: Err(Failure {
                code: ErrorCode::NoModel,
                message: "no reviewer".into(),
                retry_after_ms: None,
                provider: None,
            }),
            web_search: r#loop::Hosted::Keep,
            notice: None,
            applied: None,
            credential_files: Vec::new(),
            observe: None,
            reviewer_observe: None,
        })
    });
    let rewound = rewound.switcher(prepare, r#loop::Switchable { chosen: None });
    tx.send(support::model("fake/model-2", None)).unwrap();
    let _rewound = run_turn(rewound, &tx, "three");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let kinds: Vec<String> = log::read(&dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert!(
        kinds.iter().any(|k| k == "preamble_built"),
        "the switch rebuilds: {kinds:?}"
    );
    assert_eq!(
        ordered(kinds.iter().map(String::as_str)),
        [
            "session_started",
            "model_changed",
            "preamble_built",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]
    );
    let names = tool_names(&provider.requests()[0]);
    assert!(!names.iter().any(|n| n == "late_tool"), "{names:?}");
}
