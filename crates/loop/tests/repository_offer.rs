//! The offer of a repository's code before a process's first model request
//! (`docs/extensions.md`, "Code a repository ships"): with a person to
//! answer, one `repository_code_offered` and a wait for the `reply`; with
//! nobody, a notice per item or a failed run.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use contract::commands::{Reply, ReplyAnswer};
use contract::events::{
    Clients, Decision, Event, ExtensionExec, FiberExited, OfferDecision, OfferedItem, OfferedKind,
};
use contract::inbox::{Ack, Answer, Delivery};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::shapes::{Failure, Process, Usage};
use contract::{Envelope, ErrorCode, RequestId};
use fakes::Deadline;
use fakes::Scripted;
use log::Watcher;
use serde_json::Value;

use support::{DEADLINE, History, Session, delivery, deny, ignore, read_until, spawn_run, steer};

/// A fake repository: a fixed list of unapproved items, every call recorded.
#[derive(Default)]
struct Fake {
    list: Mutex<Vec<Unapproved>>,
    decides: Mutex<Vec<(String, OfferDecision)>>,
    gathers: AtomicUsize,
    /// The 1-based `decide` call that fails, once.
    fail_at: Mutex<Option<usize>>,
    /// Names whose next `decide` finds the content changed.
    obsolete: Mutex<Vec<String>>,
    /// What `unapproved` fails with.
    gather_fails: Mutex<Option<Failure>>,
}

impl Fake {
    fn new(list: Vec<Unapproved>) -> Arc<Self> {
        Arc::new(Self {
            list: Mutex::new(list),
            ..Self::default()
        })
    }

    fn decides(&self) -> Vec<(String, OfferDecision)> {
        self.decides.lock().unwrap().clone()
    }

    fn gathers(&self) -> usize {
        self.gathers.load(Ordering::SeqCst)
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
        self.gathers.fetch_add(1, Ordering::SeqCst);
        if let Some(failure) = self.gather_fails.lock().unwrap().clone() {
            return Err(failure);
        }
        Ok(self.list.lock().unwrap().clone())
    }

    fn decide(&self, item: &OfferedItem, decision: OfferDecision) -> Result<Decided, Failure> {
        let count = {
            let mut decides = self.decides.lock().unwrap();
            decides.push((item.name.clone(), decision));
            decides.len()
        };
        let mut fail_at = self.fail_at.lock().unwrap();
        if *fail_at == Some(count) {
            *fail_at = None;
            return Err(failure(
                ErrorCode::IoFailed,
                &format!("could not approve mcp_server {}: boom", item.name),
            ));
        }
        let mut obsolete = self.obsolete.lock().unwrap();
        if let Some(at) = obsolete.iter().position(|name| *name == item.name) {
            obsolete.remove(at);
            for listed in self.list.lock().unwrap().iter_mut() {
                if listed.offered.name == item.name {
                    listed.offered.hash.push_str("-new");
                }
            }
            return Ok(Decided::Obsolete);
        }
        Ok(Decided::Recorded)
    }
}

fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.to_owned(),
        retry_after_ms: None,
        provider: None,
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

#[track_caller]
fn ended(finished: &mpsc::Receiver<Result<(), r#loop::Error>>) -> Result<(), r#loop::Error> {
    Deadline::after(DEADLINE)
        .recv(finished)
        .expect("run ended in time")
}

/// The run `finished` reports, with its summary: what `fiber_exited` reads.
fn ran(finished: &mpsc::Receiver<r#loop::Ran>) -> r#loop::Ran {
    Deadline::after(DEADLINE)
        .recv(finished)
        .expect("run ended in time")
}

/// An acknowledgement that reports its answer.
fn probe() -> (Ack, mpsc::Receiver<Answer>) {
    let (tx, rx) = mpsc::channel();
    (
        Ack(Box::new(move |answer| {
            tx.send(answer).unwrap_or(());
        })),
        rx,
    )
}

#[track_caller]
fn answered(rx: &mpsc::Receiver<Answer>) -> Answer {
    Deadline::after(DEADLINE)
        .recv(rx)
        .expect("the reply was answered")
}

#[track_caller]
fn rejection(rx: &mpsc::Receiver<Answer>) -> (ErrorCode, String) {
    let rejected = answered(rx).expect_err("the reply was rejected");
    (rejected.code, rejected.message)
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
    let (ack, rx) = probe();
    session
        .inbox
        .send(Delivery::Reply(
            Reply {
                request_id: request.clone(),
                answer,
            },
            ack,
        ))
        .unwrap();
    rx
}

fn close(session: &Session) {
    session.inbox.send(Delivery::Close(ignore())).unwrap();
}

fn until_kind(watcher: Watcher, kind: &'static str) -> (Watcher, Vec<Envelope>) {
    read_until(watcher, kind, move |line| line.kind == kind)
}

fn request_of(line: &Envelope) -> RequestId {
    RequestId(line.payload["request_id"].as_str().unwrap().to_owned())
}

fn durable_kinds(session: &Session) -> Vec<String> {
    log::read(&session.dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect()
}

fn durable(session: &Session, kind: &str) -> Vec<Envelope> {
    log::read(&session.dir)
        .unwrap()
        .into_iter()
        .filter(|line| line.kind == kind)
        .collect()
}

fn offered_items(list: &[Unapproved]) -> Value {
    serde_json::to_value(list.iter().map(|u| &u.offered).collect::<Vec<_>>()).unwrap()
}

/// The text of every message in a `turn_started`'s input, in order.
fn input_texts(line: &Envelope) -> Vec<String> {
    line.payload["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["content"][0]["text"].as_str().map(str::to_owned))
        .collect()
}

/// Every line `watcher` holds now, in order.
fn drain(watcher: &mut Watcher) -> Vec<Envelope> {
    let mut lines = Vec::new();
    while let Ok(Some(line)) = watcher.try_recv() {
        lines.push(line);
    }
    lines
}

/// The messages of the `repository_code_skipped` notices among `lines`.
fn notices(lines: &[Envelope]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.kind == "notice" && line.payload["code"] == "repository_code_skipped")
        .map(|line| line.payload["message"].as_str().unwrap().to_owned())
        .collect()
}

use contract::events::OfferDecision::{Approve, Never, Skip};

#[test]
fn a_prompt_raises_one_offer_before_the_preamble_and_waits_for_the_reply() {
    let list = vec![
        item(OfferedKind::Extension, "fiber.test/e", false),
        item(OfferedKind::Hook, "fmt", false),
        server("db"),
    ];
    let code = Fake::new(list.clone());
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();

    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    assert!(
        !lines.iter().any(|l| l.kind == "preamble_built"),
        "the offer comes before the preamble"
    );
    let offered = lines.last().unwrap();
    assert_eq!(offered.payload["items"], offered_items(&list));
    let request = request_of(offered);
    assert!(request.0.starts_with("r_"), "{}", request.0);

    let (watcher, lines) = read_until(watcher, "a waiting session_status", |line| {
        line.kind == "session_status" && line.payload["state"] == "waiting"
    });
    let status = lines.last().unwrap();
    assert_eq!(status.payload["waiting"]["kind"], "offer");
    assert_eq!(status.payload["waiting"]["request_id"], request.0.as_str());
    assert!(
        session.provider.requests().is_empty(),
        "no model request while the offer waits"
    );

    // The reply is accepted only once its resolved line is in the log.
    let dir = session.dir.clone();
    let (tx, written) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Reply(
            Reply {
                request_id: request.clone(),
                answer: decisions(&[Approve, Skip, Never]),
            },
            Ack(Box::new(move |answer| {
                let resolved = log::read(&dir)
                    .unwrap()
                    .iter()
                    .any(|line| line.kind == "repository_code_resolved");
                tx.send((answer.is_ok(), resolved)).unwrap();
            })),
        ))
        .unwrap();
    assert_eq!(
        Deadline::after(DEADLINE).recv(&written).unwrap(),
        (true, true),
        "accepted after repository_code_resolved"
    );
    let (_, lines) = until_kind(watcher, "turn_completed");
    let resolved = &durable(&session, "repository_code_resolved")[0];
    assert_eq!(resolved.payload["request_id"], request.0.as_str());
    assert_eq!(
        resolved.payload["decisions"],
        serde_json::json!(["approve", "skip", "never"])
    );
    assert!(lines.iter().any(|l| l.kind == "preamble_built"));
    assert_eq!(
        code.decides(),
        [
            ("fiber.test/e".to_owned(), Approve),
            ("db".to_owned(), Never)
        ]
    );
    assert_eq!(session.provider.requests().len(), 1);
    close(&session);
    ended(&finished).unwrap();
    // The complete, ordered durable kinds: the offer waits, resolves, then
    // the turn runs.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered repository_code_resolved preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn a_reply_that_does_not_fit_is_rejected_and_the_offer_stays_pending() {
    let code = Fake::new(vec![server("a"), server("b")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());

    let unfit = (
        ErrorCode::InvalidArguments,
        "That answer does not fit the pending request.".to_owned(),
    );
    let short = send_reply(&session, &request, decisions(&[Approve]));
    assert_eq!(rejection(&short), unfit);
    let long = send_reply(&session, &request, decisions(&[Approve, Approve, Approve]));
    assert_eq!(rejection(&long), unfit);
    let approval = send_reply(
        &session,
        &request,
        ReplyAnswer::Approval {
            decision: Decision::Allow,
            feedback: None,
            remember: None,
        },
    );
    assert_eq!(rejection(&approval), unfit);
    let other = send_reply(
        &session,
        &RequestId("r_other".into()),
        decisions(&[Approve, Approve]),
    );
    assert_eq!(rejection(&other).0, ErrorCode::StaleRequest);
    assert!(code.decides().is_empty(), "nothing recorded for a misfit");

    let right = send_reply(&session, &request, decisions(&[Approve, Skip]));
    assert!(answered(&right).is_ok());
    let (_, _) = until_kind(watcher, "repository_code_resolved");
    let again = send_reply(&session, &request, decisions(&[Approve, Skip]));
    assert_eq!(rejection(&again).0, ErrorCode::StaleRequest);
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(durable(&session, "repository_code_resolved").len(), 1);
    // The complete, ordered durable kinds.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered repository_code_resolved preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn a_decision_the_seam_fails_rejects_the_reply_and_a_second_reply_resolves() {
    let code = Fake::new(vec![server("a"), server("b"), server("c")]);
    *code.fail_at.lock().unwrap() = Some(2);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());

    let first = send_reply(&session, &request, decisions(&[Approve, Approve, Approve]));
    assert_eq!(
        rejection(&first),
        (
            ErrorCode::IoFailed,
            "could not approve mcp_server b: boom".to_owned()
        )
    );
    assert!(durable(&session, "repository_code_resolved").is_empty());
    assert_eq!(
        code.decides(),
        [("a".to_owned(), Approve), ("b".to_owned(), Approve)]
    );

    let second = send_reply(&session, &request, decisions(&[Approve, Approve, Approve]));
    assert!(answered(&second).is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    assert_eq!(code.decides().len(), 5);
    assert_eq!(durable(&session, "repository_code_resolved").len(), 1);
    close(&session);
    ended(&finished).unwrap();
    // The complete, ordered durable kinds.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered repository_code_resolved preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn content_that_changed_under_an_answer_is_offered_again_alone() {
    let code = Fake::new(vec![server("a"), server("b")]);
    code.obsolete.lock().unwrap().push("b".into());
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let first = request_of(lines.last().unwrap());
    let reply = send_reply(&session, &first, decisions(&[Approve, Approve]));
    assert!(answered(&reply).is_ok());

    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    assert!(!lines.iter().any(|l| l.kind == "preamble_built"));
    let again = lines.last().unwrap();
    let second = request_of(again);
    assert_ne!(second, first);
    let items = again.payload["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["name"], "b");
    assert_eq!(items[0]["hash"], "h_b-new");
    let reply = send_reply(&session, &second, decisions(&[Approve]));
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    close(&session);
    ended(&finished).unwrap();
    // The complete, ordered durable kinds: the first offer resolves, the
    // changed item is offered again alone, then the turn runs.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered repository_code_resolved repository_code_offered repository_code_resolved preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn deliveries_while_an_offer_waits_go_where_they_belong() {
    let code = Fake::new(vec![server("a")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());

    // Turn input waits for the turn; a stale wake is dropped; news and a
    // stale reply are answered at once.
    session.inbox.send(steer("s")).unwrap();
    session.inbox.send(delivery("two")).unwrap();
    session.inbox.send(Delivery::Cancelled).unwrap();
    session
        .inbox
        .send(Delivery::ExtensionExec(ExtensionExec {
            extension: "fiber.test/notes".into(),
            program: "git".into(),
            args: Vec::new(),
            cwd: "/w".into(),
            process: Process {
                exit_code: Some(0),
                signal: None,
                timed_out: false,
            },
        }))
        .unwrap();
    let stale = send_reply(
        &session,
        &RequestId("r_other".into()),
        decisions(&[Approve]),
    );
    assert_eq!(rejection(&stale).0, ErrorCode::StaleRequest);
    let (watcher, _) = until_kind(watcher, "extension_exec");
    assert!(durable(&session, "repository_code_resolved").is_empty());

    let reply = send_reply(&session, &request, decisions(&[Skip]));
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    let started = durable(&session, "turn_started");
    assert_eq!(started.len(), 1, "one turn takes everything");
    assert_eq!(input_texts(&started[0]), ["go", "s", "two"]);
    close(&session);
    ended(&finished).unwrap();
    // The complete, ordered durable kinds.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered extension_exec repository_code_resolved preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn never_marked_items_and_items_answered_in_this_process_are_not_offered() {
    let mut never = server("n");
    never.never = true;
    let code = Fake::new(vec![never]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let mut all = session.log.watch();
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (_, _) = until_kind(watcher, "turn_completed");
    close(&session);
    ended(&finished).unwrap();
    assert!(durable(&session, "repository_code_offered").is_empty());
    assert!(notices(&drain(&mut all)).is_empty());
    // The complete, ordered durable kinds: nothing offered, the turn runs.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn a_required_item_a_person_skips_fails_nothing() {
    let code = Fake::new(vec![item(OfferedKind::McpServer, "db", true)]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let reply = send_reply(
        &session,
        &request_of(lines.last().unwrap()),
        decisions(&[Skip]),
    );
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    close(&session);
    ended(&finished).unwrap();
    assert!(code.decides().is_empty());
    // The complete, ordered durable kinds.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered repository_code_resolved preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

/// What one unattended run wrote and returned.
type Unattended = (Session, Vec<Envelope>, Result<(), r#loop::Error>);

/// Starts one prompt with nobody to answer: the log watcher and the run's
/// result channel, with `go` already sent.
fn start_unattended(
    code: &Arc<Fake>,
    clients: Option<u32>,
    answerable: bool,
) -> (Session, Watcher, mpsc::Receiver<Result<(), r#loop::Error>>) {
    let mut session = session(code, clients, vec![Scripted::text("Hello.")]).answerable(answerable);
    let all = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    (session, all, finished)
}

/// Closes a run that may already have ended on its own. A run that fails at
/// once drops the inbox's receiver, so the send errors; the caller's
/// `ended` still asserts the run returned.
fn close_if_running(session: &Session) {
    session.inbox.send(Delivery::Close(ignore())).unwrap_or(());
}

/// Runs one prompt with nobody to answer: every line written, and what
/// `run` returned.
fn unattended(code: &Arc<Fake>, clients: Option<u32>, answerable: bool) -> Unattended {
    let (session, mut all, finished) = start_unattended(code, clients, answerable);
    close_if_running(&session);
    let ran = ended(&finished);
    let lines = drain(&mut all);
    (session, lines, ran)
}

#[test]
fn closing_after_a_failed_run_has_ended_is_not_a_send_failure() {
    let code = Fake::new(vec![item(OfferedKind::McpServer, "db", true)]);
    let (session, _all, finished) = start_unattended(&code, Some(0), true);
    // Hold the close until the run has returned and dropped its inbox.
    let ran = ended(&finished);
    assert!(session.inbox.send(Delivery::Close(ignore())).is_err());
    close_if_running(&session);
    ran.expect_err("a required item fails the run");
}

fn skipped(name: &str, words: &str) -> String {
    format!(
        "The repository declares the {words} `{name}`, which nobody approved: it was not loaded. Run `fiber approve` in the repository to approve it."
    )
}

#[test]
fn with_nobody_to_answer_each_item_is_skipped_with_a_notice() {
    for (clients, answerable) in [(Some(0), true), (None, true), (Some(1), false)] {
        let code = Fake::new(vec![
            item(OfferedKind::Extension, "fiber.test/e", false),
            item(OfferedKind::Hook, "fmt", false),
            server("db"),
        ]);
        let (session, lines, ran) = unattended(&code, clients, answerable);
        ran.unwrap();
        assert!(durable(&session, "repository_code_offered").is_empty());
        assert_eq!(
            notices(&lines),
            [
                skipped("fiber.test/e", "extension"),
                skipped("fmt", "hook"),
                skipped("db", "MCP server"),
            ],
            "{clients:?} {answerable}"
        );
        let kinds: Vec<&str> = lines.iter().map(|l| l.kind.as_str()).collect();
        let notice = kinds.iter().position(|k| *k == "notice").unwrap();
        let preamble = kinds.iter().position(|k| *k == "preamble_built").unwrap();
        assert!(notice < preamble, "{kinds:?}");
        assert_eq!(durable(&session, "turn_completed").len(), 1);
        assert!(code.decides().is_empty());
        // The complete, ordered durable kinds.
        assert_eq!(
            durable_kinds(&session).join(" "),
            "session_started preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed",
            "{clients:?} {answerable}"
        );
    }
}

#[test]
fn with_nobody_to_answer_a_required_item_fails_the_run_with_its_kinds_code() {
    for (kind, expected) in [
        (OfferedKind::Extension, ErrorCode::ExtensionUnapproved),
        (OfferedKind::Hook, ErrorCode::HookUnapproved),
        (OfferedKind::McpServer, ErrorCode::McpServerUnapproved),
    ] {
        for answerable in [true, false] {
            let code = Fake::new(vec![
                server("plain"),
                item(kind, "needed", true),
                item(OfferedKind::Hook, "also", true),
            ]);
            let clients = if answerable { Some(0) } else { Some(1) };
            let (session, lines, ran) = unattended(&code, clients, answerable);
            let error = ran.expect_err("a required item fails the run");
            assert_eq!(error.code(), expected);
            let message = error.to_string();
            assert!(message.contains("`needed`"), "{message}");
            assert!(message.contains("`also`"), "{message}");
            assert!(message.contains("fiber approve"), "{message}");
            assert_eq!(notices(&lines), [skipped("plain", "MCP server")]);
            assert!(durable(&session, "turn_started").is_empty());
            assert!(session.provider.requests().is_empty());
            // The complete, ordered durable kinds.
            assert_eq!(durable_kinds(&session).join(" "), "session_started");
        }
    }
}

#[test]
fn a_second_turn_gathers_nothing_more() {
    let code = Fake::new(vec![server("a")]);
    let mut session = session(
        &code,
        Some(1),
        vec![Scripted::text("Hello."), Scripted::text("Again.")],
    );
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let reply = send_reply(
        &session,
        &request_of(lines.last().unwrap()),
        decisions(&[Approve]),
    );
    assert!(answered(&reply).is_ok());
    let (watcher, _) = until_kind(watcher, "turn_completed");
    let (gathers, decides) = (code.gathers(), code.decides().len());
    session.inbox.send(delivery("more")).unwrap();
    let (_, _) = until_kind(watcher, "turn_completed");
    assert_eq!((code.gathers(), code.decides().len()), (gathers, decides));
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(durable(&session, "repository_code_offered").len(), 1);
    // The complete, ordered durable kinds: one offer, then two turns.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered repository_code_resolved preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn without_a_seam_nothing_is_offered() {
    let mut session = Session::new(vec![Scripted::text("Hello.")], None);
    session
        .log
        .append(&Event::Clients(Clients { count: 1 }), None, None)
        .unwrap();
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (_, _) = until_kind(watcher, "turn_completed");
    close(&session);
    ended(&finished).unwrap();
    assert!(durable(&session, "repository_code_offered").is_empty());
    // The complete, ordered durable kinds: without a seam the turn runs.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn waiting_on_an_offer_is_idle() {
    let code = Fake::new(vec![server("a")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let delay = Duration::from_secs(60);
    session.looped = Some(session.looped.take().unwrap().idle_exit(Some(delay)));
    let clock = Arc::clone(&session.clock);
    let watcher = session.log.watch();
    // The run's summary, not the log, feeds the exit below.
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(looped.run()).unwrap());
    // The offer's idle clock starts when it is raised, not when the idle
    // wait before the prompt began.
    clock.advance_when_parked(
        Some(clock.now() + delay),
        None,
        Duration::from_secs(10),
        &Deadline::after(DEADLINE),
    );
    let (ack, prompt_answer) = probe();
    session
        .inbox
        .send(Delivery::Prompt(support::message("go"), ack))
        .unwrap();
    let (_, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let deadline = clock.now() + delay;
    let mark = clock.advance_when_parked(
        Some(deadline),
        None,
        Duration::from_millis(59_999),
        &Deadline::after(DEADLINE),
    );
    // A clock move reaches the loop as a wake, which the wait discards.
    session.inbox.send(Delivery::Cancelled).unwrap();
    assert!(
        clock.await_parked_since(&mark, Some(deadline), fakes::Deadline::after(DEADLINE)),
        "1 ms early the offer still waits"
    );
    assert!(finished.try_recv().is_err());
    clock.advance_when_parked(
        Some(deadline),
        None,
        Duration::from_millis(1),
        &Deadline::after(DEADLINE),
    );
    session.inbox.send(Delivery::Cancelled).unwrap();
    let run = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("run ended in time");
    run.result.unwrap();
    let kinds = durable_kinds(&session);
    assert!(!kinds.iter().any(|k| k == "repository_code_resolved"));
    assert!(!kinds.iter().any(|k| k == "preamble_built"));
    assert!(
        matches!(
            Deadline::after(DEADLINE).recv(&prompt_answer),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ),
        "the prompt's ack is dropped uncalled"
    );
    r#loop::fiber_exited(&session.log, &run.summary, Ok(()), false, None).unwrap();
    let exited = durable(&session, "fiber_exited");
    assert_eq!(exited[0].payload["suspended_on"], request.0.as_str());
    // The complete, ordered durable kinds: the offer waits, then the idle
    // exit suspends on it.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered fiber_exited"
    );
}

#[test]
fn close_while_an_offer_waits_skips_its_items_and_runs_the_turn() {
    let code = Fake::new(vec![server("a"), item(OfferedKind::Hook, "fmt", false)]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let mut all = session.log.watch();
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, _) = until_kind(watcher, "repository_code_offered");
    close(&session);
    let (watcher, _) = until_kind(watcher, "preamble_built");
    // The status leaves `waiting` once the preamble is built.
    let (_, _) = read_until(watcher, "a session_status that is not waiting", |line| {
        line.kind == "session_status" && line.payload["state"] != "waiting"
    });
    ended(&finished).unwrap();
    assert!(durable(&session, "repository_code_resolved").is_empty());
    assert_eq!(
        notices(&drain(&mut all)),
        [skipped("a", "MCP server"), skipped("fmt", "hook")]
    );
    assert_eq!(durable(&session, "turn_completed").len(), 1);
    // The complete, ordered durable kinds: the close skips the offer, then
    // the turn runs.
    assert_eq!(
        durable_kinds(&session).join(" "),
        "session_started repository_code_offered preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn the_failure_names_each_required_item_and_how_to_approve() {
    let code = Fake::new(vec![item(OfferedKind::McpServer, "db", true)]);
    let (session, lines, ran) = unattended(&code, Some(0), true);
    assert_eq!(
        ran.expect_err("a required item fails the run").to_string(),
        "The repository requires the MCP server `db`, which nobody approved, and nobody could be asked. Run `fiber approve` in the repository to approve it."
    );
    // The complete, ordered live kinds of the failed run, setting the
    // `session_status` line aside: whether the helper's `close_if_running`
    // lands before the failed run ends is timing.
    let live: Vec<&str> = lines
        .iter()
        .map(|line| line.kind.as_str())
        .filter(|kind| *kind != "session_status")
        .collect();
    assert_eq!(live.join(" "), "");
    assert!(
        lines.iter().any(|line| line.kind == "session_status"),
        "at least one session_status was written"
    );
    assert_eq!(durable_kinds(&session).join(" "), "session_started");
    let code = Fake::new(vec![
        item(OfferedKind::McpServer, "db", true),
        item(OfferedKind::Hook, "fmt", true),
    ]);
    // The complete, ordered live kinds of the failed run.
    let (session, lines, ran) = unattended(&code, Some(0), true);
    assert_eq!(
        ran.expect_err("a required item fails the run").to_string(),
        "The repository requires the MCP server `db` and the hook `fmt`, which nobody approved, and nobody could be asked. Run `fiber approve` in the repository to approve them."
    );
    // The complete, ordered live kinds of the failed run, setting the
    // `session_status` line aside, as above.
    let live: Vec<&str> = lines
        .iter()
        .map(|line| line.kind.as_str())
        .filter(|kind| *kind != "session_status")
        .collect();
    assert_eq!(live.join(" "), "");
    assert!(
        lines.iter().any(|line| line.kind == "session_status"),
        "at least one session_status was written"
    );
    assert_eq!(durable_kinds(&session).join(" "), "session_started");
}

// Resume: a pending offer is raised again, the session's skips hold, and an
// approval suspended in an earlier process survives an offer's exit.

use contract::events::{
    AskStep, Empty, InputItem, PermissionRequested, RepositoryCodeOffered, RepositoryCodeResolved,
    RuleScope, StandingRule, ToolCallRequested, TurnStarted,
};
use contract::shapes::{ContentPart, DeclaredEffects, Effect, Origin, Sender};
use contract::{ActionId, CommandId, TurnId};
use r#loop::Loop;

impl support::History {
    fn write(&self, event: &Event, turn: Option<&str>, action: Option<&str>) {
        self.append(
            event,
            turn.map(|t| TurnId(t.into())),
            action.map(|a| ActionId(a.into())),
        );
    }

    /// A process starts.
    fn start_process(&self, resumed: bool) {
        r#loop::fiber_started(&self.log, "0.0.0", resumed).unwrap();
    }

    /// The process exits as `main` ends one, after the run `summary`
    /// folds: the exit over the live loop's own lines.
    fn exit_process(&self, summary: &r#loop::Summary, ran: Result<(), Failure>) -> Value {
        r#loop::fiber_exited(&self.log, summary, ran, false, None).unwrap();
        Self::exited_payload(&self.dir)
    }

    /// A hand-built process's exit line, suspending on `suspended`: the
    /// fixture an earlier process left behind for the next resume to read.
    /// The exit's computation over live lines is covered beside the
    /// summary (`crates/loop/src/process_tests.rs`), with the same event
    /// patterns these fixtures build; here only the fixture line matters.
    fn exit_fixture(&self, suspended: Option<&str>) -> Value {
        let exited = Event::FiberExited(FiberExited {
            exit_code: 0,
            usage: Usage::default(),
            final_message: None,
            error: None,
            suspended_on: suspended.map(|suspended| RequestId(suspended.into())),
            questions: None,
        });
        self.log.append(&exited, None, None).unwrap();
        Self::exited_payload(&self.dir)
    }

    fn exited_payload(dir: &std::path::Path) -> Value {
        let exited = log::read(dir)
            .unwrap()
            .into_iter()
            .rfind(|line| line.kind == "fiber_exited")
            .unwrap();
        Value::Object(exited.payload)
    }

    fn clients(&self, count: u32) {
        self.write(&Event::Clients(Clients { count }), None, None);
    }

    /// A resumed loop reading `code`, and its inbox.
    fn resume(&self, code: &Arc<Fake>, answerable: bool) -> (Loop, mpsc::Sender<Delivery>) {
        // The fold reads the log as the last process left it, then this
        // process starts, as `main` does.
        let folded = r#loop::resumed(&self.dir).unwrap();
        self.start_process(true);
        let (tx, rx) = mpsc::channel();
        let prompt_clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
        let looped = Loop::resume(
            r#loop::Session {
                log: Arc::clone(&self.log),
                provider: Arc::clone(&self.provider) as Arc<dyn contract::provider::Provider>,
                model: r#loop::Model {
                    reference: support::MODEL.into(),
                    cost: None,
                    subscription: false,
                },
                prompt: r#loop::PromptInputs::new(
                    self.root.path().to_path_buf(),
                    "/bin/sh".into(),
                    self.dir.join("events.jsonl").display().to_string(),
                    prompt_clock,
                    fakes::CONTEXT_WINDOW,
                ),
                inbox: rx,
                tools: Vec::new(),
                permissions: r#loop::Permissions {
                    workspace: self.workspace.clone(),
                    credentials: self.credentials.clone(),
                    credential_files: Vec::new(),
                    rules: Arc::new(support::FakeRules::empty()),
                },
            },
            folded,
        )
        .unwrap()
        .repository_code(Arc::clone(code) as Arc<dyn RepositoryCode>)
        .answerable(answerable);
        (looped, tx)
    }

    fn of(&self, kind: &str) -> Vec<Envelope> {
        self.lines()
            .into_iter()
            .filter(|line| line.kind == kind)
            .collect()
    }

    /// An earlier process that exited while its offer `request` of `items`
    /// waited.
    fn exited_on_offer(&self, request: &str, items: &[Unapproved]) {
        self.start_process(false);
        self.write(&offered_event(request, items), None, None);
        let exited = self.exit_fixture(Some(request));
        assert_eq!(exited["suspended_on"], request);
    }

    /// An earlier process that exited with its turn `t_1` suspended on the
    /// standing ask `r_9` for the call `a_1`.
    fn exited_on_approval(&self) {
        self.exited_on_offer_then_approval(&[]);
    }

    /// As [`History::exited_on_approval`], with the offer `r_old` of
    /// `items` raised first and left unresolved, unless `items` is empty.
    fn exited_on_offer_then_approval(&self, items: &[Unapproved]) {
        self.start_process(false);
        if !items.is_empty() {
            self.write(&offered_event("r_old", items), None, None);
        }
        self.write(
            &Event::TurnStarted(TurnStarted {
                input: vec![InputItem::Message {
                    content: vec![ContentPart::Text { text: "one".into() }],
                    sender: Sender {
                        origin: Origin::Driver,
                        command_id: Some(CommandId("c_one".into())),
                    },
                    changed_by: None,
                }],
            }),
            Some("t_1"),
            None,
        );
        self.write(
            &Event::AssistantMessageStarted(Empty {}),
            Some("t_1"),
            Some("a_0"),
        );
        self.write(
            &Event::ToolCallRequested(ToolCallRequested {
                name: "read".into(),
                arguments: serde_json::json!({}),
                provider_id: None,
                repair: None,
                ran_by: None,
                provider_item: None,
            }),
            Some("t_1"),
            Some("a_1"),
        );
        self.write(&standing_ask(), Some("t_1"), Some("a_1"));
        let exited = self.exit_fixture(Some("r_9"));
        assert_eq!(exited["suspended_on"], "r_9");
    }
}

fn offered_event(request: &str, items: &[Unapproved]) -> Event {
    Event::RepositoryCodeOffered(RepositoryCodeOffered {
        request_id: RequestId(request.into()),
        items: items.iter().map(|u| u.offered.clone()).collect(),
    })
}

fn resolved_event(request: &str, decisions: &[OfferDecision]) -> Event {
    Event::RepositoryCodeResolved(RepositoryCodeResolved {
        request_id: RequestId(request.into()),
        decisions: decisions.to_vec(),
    })
}

fn standing_ask() -> Event {
    Event::PermissionRequested(PermissionRequested {
        always_reviewed: false,
        request_id: RequestId("r_9".into()),
        declared: DeclaredEffects {
            effects: vec![Effect::Executes],
            reversible: true,
            paths: None,
        },
        step: AskStep::StandingAsk {
            standing_rule: StandingRule {
                scope: RuleScope::Project,
                prefix: "run tests".into(),
            },
        },
    })
}

fn run_on(looped: Loop) -> mpsc::Receiver<r#loop::Ran> {
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let ran = looped.run();
        done.send(ran).unwrap();
    });
    finished
}

fn reply_on(
    inbox: &mpsc::Sender<Delivery>,
    request: &str,
    answer: ReplyAnswer,
) -> mpsc::Receiver<Answer> {
    let (ack, rx) = probe();
    inbox
        .send(Delivery::Reply(
            Reply {
                request_id: RequestId(request.into()),
                answer,
            },
            ack,
        ))
        .unwrap();
    rx
}

/// The run `finished` reports, asserting it ended well: what
/// `fiber_exited` reads.
fn finished_ok(finished: &mpsc::Receiver<r#loop::Ran>) -> r#loop::Ran {
    let run = ran(finished);
    run.result.as_ref().unwrap();
    run
}

#[test]
fn a_pending_offer_is_raised_again_before_any_prompt_when_a_client_is_attached() {
    let items = [server("a"), server("b")];
    let code = Fake::new(items.to_vec());
    let history = History::new(vec![Scripted::text("Hello.")]);
    history.exited_on_offer("r_old", &items);
    history.clients(1);
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped);

    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let again = lines.last().unwrap();
    assert_eq!(again.payload["request_id"], "r_old");
    assert_eq!(again.payload["items"], offered_items(&items));
    let reply = reply_on(&inbox, "r_old", decisions(&[Approve, Skip]));
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "repository_code_resolved");
    inbox.send(Delivery::Close(ignore())).unwrap();
    let run = finished_ok(&finished);
    // No suspended turn: nothing was raised or run but the offer.
    assert!(history.of("permission_requested").is_empty());
    assert!(history.of("turn_started").is_empty());
    assert_eq!(history.of("repository_code_offered").len(), 2);

    // Resolved after a re-raise, it is not raised by a later resume.
    let exited = history.exit_process(&run.summary, Ok(()));
    assert!(exited.get("suspended_on").is_none());
    code.list.lock().unwrap().clear();
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped);
    inbox.send(delivery("go")).unwrap();
    let (_, _) = until_kind(watcher, "turn_completed");
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    assert_eq!(history.of("repository_code_offered").len(), 2);
    // The complete, ordered durable kinds: raised again and resolved, then
    // a later resume runs its turn with nothing left to offer.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started repository_code_offered fiber_exited fiber_started repository_code_offered repository_code_resolved fiber_exited fiber_started preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn a_pending_offer_is_not_raised_again_with_nobody_to_answer() {
    let items = [server("a")];
    let code = Fake::new(items.to_vec());
    let history = History::new(vec![Scripted::text("Hello.")]);
    history.exited_on_offer("r_old", &items);
    history.clients(1);
    let mut all = history.log.watch();
    let (looped, inbox) = history.resume(&code, false);
    let finished = run_on(looped);
    inbox.send(delivery("go")).unwrap();
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    assert_eq!(history.of("repository_code_offered").len(), 1);
    assert!(history.of("repository_code_resolved").is_empty());
    assert_eq!(notices(&drain(&mut all)), [skipped("a", "MCP server")]);
    assert_eq!(history.of("turn_completed").len(), 1);
    // The complete, ordered durable kinds.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started repository_code_offered fiber_exited fiber_started preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn a_skip_from_an_earlier_process_holds_for_the_same_content_only() {
    for (hash, offered) in [("h_a", false), ("h_a2", true)] {
        let mut now = server("a");
        now.offered.hash = hash.into();
        let code = Fake::new(vec![now]);
        let history = History::new(vec![Scripted::text("Hello.")]);
        history.start_process(false);
        history.write(&offered_event("r_1", &[server("a")]), None, None);
        history.write(&resolved_event("r_1", &[Skip]), None, None);
        history.exit_fixture(None);
        history.clients(1);
        let watcher = history.log.watch();
        let (looped, inbox) = history.resume(&code, true);
        let finished = run_on(looped);
        inbox.send(delivery("go")).unwrap();
        let watcher = if offered {
            let (watcher, lines) = until_kind(watcher, "repository_code_offered");
            let again = lines.last().unwrap();
            assert_eq!(again.payload["items"][0]["hash"], "h_a2");
            let reply = reply_on(&inbox, request_of(again).0.as_str(), decisions(&[Skip]));
            assert!(answered(&reply).is_ok());
            watcher
        } else {
            watcher
        };
        let (_, _) = until_kind(watcher, "turn_completed");
        inbox.send(Delivery::Close(ignore())).unwrap();
        finished_ok(&finished);
        let expected = if offered { 2 } else { 1 };
        assert_eq!(
            history.of("repository_code_offered").len(),
            expected,
            "{hash}"
        );
        // The complete, ordered durable kinds: the same content is never
        // offered again; changed content is offered again alone.
        let expected = if hash == "h_a" {
            "session_started fiber_started repository_code_offered repository_code_resolved fiber_exited fiber_started preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
        } else {
            "session_started fiber_started repository_code_offered repository_code_resolved fiber_exited fiber_started repository_code_offered repository_code_resolved preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
        };
        assert_eq!(
            history
                .lines()
                .iter()
                .map(|line| line.kind.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            expected,
            "{hash}"
        );
    }
}

#[test]
fn content_changed_under_a_pending_offer_is_offered_fresh_after_it_resolves() {
    let items = [server("a"), server("b")];
    let code = Fake::new(items.to_vec());
    code.obsolete.lock().unwrap().push("b".into());
    let history = History::new(vec![Scripted::text("Hello.")]);
    history.exited_on_offer("r_old", &items);
    history.clients(1);
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped);
    let (watcher, _) = until_kind(watcher, "repository_code_offered");
    let reply = reply_on(&inbox, "r_old", decisions(&[Approve, Approve]));
    assert!(answered(&reply).is_ok());
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let fresh = lines.last().unwrap();
    assert_ne!(fresh.payload["request_id"], "r_old");
    let names: Vec<&Value> = fresh.payload["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| &i["name"])
        .collect();
    assert_eq!(names, ["b"]);
    assert_eq!(fresh.payload["items"][0]["hash"], "h_b-new");
    let reply = reply_on(&inbox, request_of(fresh).0.as_str(), decisions(&[Approve]));
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "repository_code_resolved");
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    // The complete, ordered durable kinds.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started repository_code_offered fiber_exited fiber_started repository_code_offered repository_code_resolved repository_code_offered repository_code_resolved"
    );
}

#[test]
fn an_offer_already_resolved_is_not_raised_again() {
    let code = Fake::new(Vec::new());
    let history = History::new(vec![Scripted::text("Hello.")]);
    history.start_process(false);
    history.write(&offered_event("r_1", &[server("a")]), None, None);
    history.write(&resolved_event("r_1", &[Approve]), None, None);
    history.exit_fixture(None);
    history.clients(1);
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped);
    inbox.send(delivery("go")).unwrap();
    let (_, _) = until_kind(watcher, "turn_completed");
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    assert_eq!(history.of("repository_code_offered").len(), 1);
    // The complete, ordered durable kinds: resolved long ago, never again.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started repository_code_offered repository_code_resolved fiber_exited fiber_started preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn an_offer_comes_before_a_suspended_approval_and_both_are_answered_in_order() {
    let code = Fake::new(vec![server("a")]);
    let history = History::new(vec![Scripted::text("Done.")]);
    history.exited_on_approval();
    history.clients(1);
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped);
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    assert!(!lines.iter().any(|l| l.kind == "permission_requested"));
    let offer = request_of(lines.last().unwrap());
    let reply = reply_on(&inbox, offer.0.as_str(), decisions(&[Approve]));
    assert!(answered(&reply).is_ok());
    let (watcher, lines) = until_kind(watcher, "permission_requested");
    assert_eq!(lines.last().unwrap().payload["request_id"], "r_9");
    let reply = reply_on(&inbox, "r_9", deny(None));
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    let resolved = history.of("permission_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decided_by"], "person");
    // The complete, ordered durable kinds: the offer resolves first, then
    // the suspended approval.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started turn_started assistant_message_started tool_call_requested permission_requested fiber_exited fiber_started repository_code_offered repository_code_resolved preamble_built opening_message permission_requested permission_resolved tool_call_completed step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn an_approvals_reply_sent_while_the_offer_waits_answers_it_after() {
    let code = Fake::new(vec![server("a")]);
    let history = History::new(vec![Scripted::text("Done.")]);
    history.exited_on_approval();
    history.clients(1);
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped);
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let offer = request_of(lines.last().unwrap());
    let approval = reply_on(&inbox, "r_9", deny(None));
    let reply = reply_on(&inbox, offer.0.as_str(), decisions(&[Approve]));
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    assert!(
        answered(&approval).is_ok(),
        "the held reply answers the approval"
    );
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    let resolved = history.of("permission_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["request_id"], "r_9");
    assert_eq!(resolved[0].payload["decided_by"], "person");
    // The complete, ordered durable kinds.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started turn_started assistant_message_started tool_call_requested permission_requested fiber_exited fiber_started repository_code_offered repository_code_resolved preamble_built opening_message permission_requested permission_resolved tool_call_completed step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn an_idle_exit_on_the_offer_keeps_the_suspended_approval_for_the_next_resume() {
    let code = Fake::new(vec![server("a")]);
    let history = History::new(vec![Scripted::text("Done.")]);
    history.exited_on_approval();
    history.clients(1);
    let delay = Duration::from_secs(60);
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped.idle_exit(Some(delay)));
    let (_, lines) = until_kind(watcher, "repository_code_offered");
    let offer = request_of(lines.last().unwrap());
    let deadline = history.clock.now() + delay;
    history
        .clock
        .advance_when_parked(Some(deadline), None, delay, &Deadline::after(DEADLINE));
    inbox.send(Delivery::Cancelled).unwrap();
    let run = finished_ok(&finished);
    let raised = history.of("permission_requested");
    assert_eq!(raised.len(), 2, "the approval is written again");
    assert_eq!(raised[1].payload["request_id"], "r_9");
    assert_eq!(raised[1].turn_id, Some(TurnId("t_1".into())));
    assert_eq!(raised[1].action_id, Some(ActionId("a_1".into())));
    let exited = history.exit_process(&run.summary, Ok(()));
    assert_eq!(exited["suspended_on"], "r_9");

    // The next resume raises the offer again, then the approval.
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped);
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    assert_eq!(request_of(lines.last().unwrap()), offer);
    let reply = reply_on(&inbox, offer.0.as_str(), decisions(&[Approve]));
    assert!(answered(&reply).is_ok());
    let (watcher, _) = until_kind(watcher, "permission_requested");
    let reply = reply_on(&inbox, "r_9", deny(None));
    assert!(answered(&reply).is_ok());
    let (_, lines) = until_kind(watcher, "turn_completed");
    assert_eq!(lines.last().unwrap().turn_id, Some(TurnId("t_1".into())));
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    // The complete, ordered durable kinds.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started turn_started assistant_message_started tool_call_requested permission_requested fiber_exited fiber_started repository_code_offered permission_requested fiber_exited fiber_started repository_code_offered repository_code_resolved preamble_built opening_message permission_requested permission_resolved tool_call_completed step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn a_failed_offer_step_keeps_the_suspended_approval_and_a_repaired_resume_finishes_the_turn() {
    // A gather failure with a person to answer, and a required item with
    // nobody to ask.
    for answerable in [true, false] {
        let code = if answerable {
            let code = Fake::new(Vec::new());
            *code.gather_fails.lock().unwrap() = Some(failure(
                ErrorCode::ConfigInvalid,
                "`repository_extensions` path `/abs` is absolute.",
            ));
            code
        } else {
            Fake::new(vec![item(OfferedKind::McpServer, "db", true)])
        };
        let history = History::new(vec![Scripted::text("Done.")]);
        history.exited_on_approval();
        history.clients(1);
        let (looped, inbox) = history.resume(&code, answerable);
        let run = ran(&run_on(looped));
        let error = run.result.expect_err("the step fails the run");
        let expected = if answerable {
            ErrorCode::ConfigInvalid
        } else {
            ErrorCode::McpServerUnapproved
        };
        assert_eq!(error.code(), expected);
        drop(inbox);
        let raised = history.of("permission_requested");
        assert_eq!(raised.len(), 2, "the approval is written again");
        let exited =
            history.exit_process(&run.summary, Err(failure(error.code(), &error.to_string())));
        assert_eq!(exited["suspended_on"], "r_9");

        // Repaired: nothing is left to offer.
        let repaired = Fake::new(Vec::new());
        let watcher = history.log.watch();
        let (looped, inbox) = history.resume(&repaired, answerable);
        let finished = run_on(looped);
        let watcher = if answerable {
            let (watcher, _) = until_kind(watcher, "permission_requested");
            let reply = reply_on(&inbox, "r_9", deny(None));
            assert!(answered(&reply).is_ok());
            watcher
        } else {
            watcher
        };
        let (_, lines) = until_kind(watcher, "turn_completed");
        assert_eq!(lines.last().unwrap().turn_id, Some(TurnId("t_1".into())));
        inbox.send(Delivery::Close(ignore())).unwrap();
        finished_ok(&finished);
        // The complete, ordered durable kinds: the failed step keeps the
        // suspended approval, and the repaired resume finishes its turn.
        assert_eq!(
            history
                .lines()
                .iter()
                .map(|line| line.kind.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            "session_started fiber_started turn_started assistant_message_started tool_call_requested permission_requested fiber_exited fiber_started permission_requested fiber_exited fiber_started preamble_built opening_message permission_requested permission_resolved tool_call_completed step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed",
            "answerable: {answerable}"
        );
    }
}

#[test]
fn a_pending_offer_waits_for_a_client_at_the_prompt() {
    let items = [server("a")];
    let code = Fake::new(items.to_vec());
    let history = History::new(vec![Scripted::text("Hello."), Scripted::text("Again.")]);
    history.exited_on_offer("r_old", &items);
    let delay = Duration::from_secs(60);

    // No client: nothing raised, a reply naming the folded offer is stale,
    // and the prompt takes the no-answer path.
    let mut all = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped);
    let stale = reply_on(&inbox, "r_old", decisions(&[Approve]));
    let (code_of, _) = rejection(&stale);
    assert_eq!(code_of, ErrorCode::StaleRequest);
    inbox.send(delivery("go")).unwrap();
    inbox.send(Delivery::Close(ignore())).unwrap();
    let run = finished_ok(&finished);
    assert_eq!(history.of("repository_code_offered").len(), 1);
    assert_eq!(notices(&drain(&mut all)), [skipped("a", "MCP server")]);
    history.exit_process(&run.summary, Ok(()));

    // A client attached after the loop started and before the prompt sees
    // the same offer raised again at the prompt.
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    let finished = run_on(looped.idle_exit(Some(delay)));
    assert!(
        history.clock.await_parked(
            history.clock.now() + delay,
            fakes::Deadline::after(DEADLINE)
        ),
        "the resumed loop waits for a prompt"
    );
    assert_eq!(history.of("repository_code_offered").len(), 1);
    history.clients(1);
    inbox.send(delivery("two")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    assert_eq!(lines.last().unwrap().payload["request_id"], "r_old");
    let reply = reply_on(&inbox, "r_old", decisions(&[Approve]));
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    // The complete, ordered durable kinds: the no-answer run skips, then
    // the attached run raises the same offer again.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started repository_code_offered fiber_exited fiber_started preamble_built opening_message turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed fiber_exited fiber_started repository_code_offered repository_code_resolved preamble_built turn_started step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn an_idle_exit_on_a_raised_again_offer_leaves_it_for_the_next_resume() {
    let items = [server("a")];
    let code = Fake::new(items.to_vec());
    let history = History::new(Vec::new());
    history.exited_on_offer("r_old", &items);
    history.clients(1);
    let delay = Duration::from_secs(60);
    for round in 0..2 {
        let watcher = history.log.watch();
        let (looped, inbox) = history.resume(&code, true);
        let finished = run_on(looped.idle_exit(Some(delay)));
        let (_, lines) = until_kind(watcher, "repository_code_offered");
        assert_eq!(lines.last().unwrap().payload["request_id"], "r_old");
        let deadline = history.clock.now() + delay;
        history
            .clock
            .advance_when_parked(Some(deadline), None, delay, &Deadline::after(DEADLINE));
        inbox.send(Delivery::Cancelled).unwrap();
        let run = finished_ok(&finished);
        assert!(history.of("repository_code_resolved").is_empty());
        let exited = history.exit_process(&run.summary, Ok(()));
        assert_eq!(exited["suspended_on"], "r_old");
        // The complete, ordered durable kinds: each idle exit leaves the
        // offer for the next resume.
        let expected = if round == 0 {
            "session_started fiber_started repository_code_offered fiber_exited fiber_started repository_code_offered fiber_exited"
        } else {
            "session_started fiber_started repository_code_offered fiber_exited fiber_started repository_code_offered fiber_exited fiber_started repository_code_offered fiber_exited"
        };
        assert_eq!(
            history
                .lines()
                .iter()
                .map(|line| line.kind.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            expected,
            "round: {round}"
        );
    }
}

#[test]
fn an_answer_to_the_offer_sent_before_it_is_raised_again_is_taken_first() {
    let items = [server("a")];
    let code = Fake::new(items.to_vec());
    let history = History::new(vec![Scripted::text("Done.")]);
    history.exited_on_offer_then_approval(&items);
    history.clients(1);
    let watcher = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    // Sent before the resumed loop runs: held, then taken by the offer.
    let early = reply_on(&inbox, "r_old", decisions(&[Approve]));
    let finished = run_on(looped);
    assert!(answered(&early).is_ok());
    let (watcher, lines) = until_kind(watcher, "permission_requested");
    let raised: Vec<&Envelope> = lines
        .iter()
        .filter(|l| l.kind == "repository_code_offered")
        .collect();
    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].payload["request_id"], "r_old");
    assert!(lines.iter().any(|l| l.kind == "repository_code_resolved"));
    let reply = reply_on(&inbox, "r_9", deny(None));
    assert!(answered(&reply).is_ok());
    let (_, _) = until_kind(watcher, "turn_completed");
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&finished);
    // The complete, ordered durable kinds.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started repository_code_offered turn_started assistant_message_started tool_call_requested permission_requested fiber_exited fiber_started repository_code_offered repository_code_resolved preamble_built opening_message permission_requested permission_resolved tool_call_completed step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}

#[test]
fn a_close_sent_before_the_offer_is_raised_again_leaves_nobody_to_answer() {
    let items = [server("a")];
    let code = Fake::new(items.to_vec());
    let history = History::new(vec![Scripted::text("Done.")]);
    history.exited_on_offer_then_approval(&items);
    history.clients(1);
    let mut all = history.log.watch();
    let (looped, inbox) = history.resume(&code, true);
    inbox.send(Delivery::Close(ignore())).unwrap();
    finished_ok(&run_on(looped));
    let lines = drain(&mut all);
    assert_eq!(notices(&lines), [skipped("a", "MCP server")]);
    assert!(history.of("repository_code_resolved").is_empty());
    // With nobody to answer, the approval is denied by cancel.
    let resolved = history.of("permission_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].payload["decided_by"], "cancel");
    assert_eq!(history.of("turn_completed").len(), 1);
    // The complete, ordered durable kinds.
    assert_eq!(
        history
            .lines()
            .iter()
            .map(|line| line.kind.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        "session_started fiber_started repository_code_offered turn_started assistant_message_started tool_call_requested permission_requested fiber_exited fiber_started repository_code_offered preamble_built opening_message permission_requested permission_resolved tool_call_completed step_started assistant_message_started text_completed usage_recorded assistant_message_completed turn_completed"
    );
}
