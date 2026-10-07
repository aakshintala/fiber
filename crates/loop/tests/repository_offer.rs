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
    Clients, Decision, Event, ExtensionExec, OfferDecision, OfferedItem, OfferedKind,
};
use contract::inbox::{Ack, Answer, Delivery};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::shapes::{Failure, Process};
use contract::{Envelope, ErrorCode, RequestId};
use fakes::Scripted;
use log::Watcher;
use serde_json::Value;

use support::{DEADLINE, Session, delivery, ignore, read_until, steer};

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

/// Runs `run` on its own thread.
fn spawn_run(session: &mut Session) -> mpsc::Receiver<Result<(), r#loop::Error>> {
    let looped = session.looped.take().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let result = looped.run();
        done.send(result).unwrap();
    });
    finished
}

fn ended(finished: &mpsc::Receiver<Result<(), r#loop::Error>>) -> Result<(), r#loop::Error> {
    finished.recv_timeout(DEADLINE).expect("run ended in time")
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

fn answered(rx: &mpsc::Receiver<Answer>) -> Answer {
    rx.recv_timeout(DEADLINE).expect("the reply was answered")
}

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
        written.recv_timeout(DEADLINE).unwrap(),
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
}

/// What one unattended run wrote and returned.
type Unattended = (Session, Vec<Envelope>, Result<(), r#loop::Error>);

/// Runs one prompt with nobody to answer: every line written, and what
/// `run` returned.
fn unattended(code: &Arc<Fake>, clients: Option<u32>, answerable: bool) -> Unattended {
    let mut session = session(code, clients, vec![Scripted::text("Hello.")]).answerable(answerable);
    let mut all = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    close(&session);
    let ran = ended(&finished);
    let lines = drain(&mut all);
    (session, lines, ran)
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
}

#[test]
fn waiting_on_an_offer_is_idle() {
    let code = Fake::new(vec![server("a")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let delay = Duration::from_secs(60);
    session.looped = Some(session.looped.take().unwrap().idle_exit(Some(delay)));
    let clock = Arc::clone(&session.clock);
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    assert!(
        clock.await_parked(clock.now() + delay, DEADLINE),
        "the idle wait parks"
    );
    // The offer's idle clock starts when it is raised, not when the idle
    // wait before the prompt began.
    clock.advance(Duration::from_secs(10));
    let (ack, prompt_answer) = probe();
    session
        .inbox
        .send(Delivery::Prompt(support::message("go"), ack))
        .unwrap();
    let (_, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let deadline = clock.now() + delay;
    let mark = clock
        .mark_parked(deadline, DEADLINE)
        .expect("the offer waits until its own deadline");
    clock.advance(Duration::from_millis(59_999));
    // A clock move reaches the loop as a wake, which the wait discards.
    session.inbox.send(Delivery::Cancelled).unwrap();
    assert!(
        clock.await_parked_since(&mark, Some(deadline), DEADLINE),
        "1 ms early the offer still waits"
    );
    assert!(finished.try_recv().is_err());
    clock.advance(Duration::from_millis(1));
    session.inbox.send(Delivery::Cancelled).unwrap();
    ended(&finished).unwrap();
    let kinds = durable_kinds(&session);
    assert!(!kinds.iter().any(|k| k == "repository_code_resolved"));
    assert!(!kinds.iter().any(|k| k == "preamble_built"));
    assert!(
        matches!(
            prompt_answer.recv_timeout(DEADLINE),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ),
        "the prompt's ack is dropped uncalled"
    );
    r#loop::fiber_exited(&session.log, &session.dir, Ok(()), false, None).unwrap();
    let exited = durable(&session, "fiber_exited");
    assert_eq!(exited[0].payload["suspended_on"], request.0.as_str());
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
}

#[test]
fn the_failure_names_each_required_item_and_how_to_approve() {
    let code = Fake::new(vec![item(OfferedKind::McpServer, "db", true)]);
    let (_, _, ran) = unattended(&code, Some(0), true);
    assert_eq!(
        ran.expect_err("a required item fails the run").to_string(),
        "The repository requires the MCP server `db`, which nobody approved, and nobody could be asked. Run `fiber approve` in the repository to approve it."
    );
    let code = Fake::new(vec![
        item(OfferedKind::McpServer, "db", true),
        item(OfferedKind::Hook, "fmt", true),
    ]);
    let (_, _, ran) = unattended(&code, Some(0), true);
    assert_eq!(
        ran.expect_err("a required item fails the run").to_string(),
        "The repository requires the MCP server `db` and the hook `fmt`, which nobody approved, and nobody could be asked. Run `fiber approve` in the repository to approve them."
    );
}
