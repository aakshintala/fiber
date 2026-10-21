//! The late step: once this process's offer records an approval of an
//! extension, the late preparer installs before the first preamble
//! (`docs/extensions.md`, "Code a repository ships"). It writes
//! `extensions_loaded` with the prepared set and its notices, and no
//! `reloaded`.

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
    CacheLifetime, Clients, Event, LoadedExtension, McpServerFailed, Notice, OfferDecision,
    OfferedItem, OfferedKind, ServerFailure, TurnOutcome,
};
use contract::inbox::{Ack, Answer, Delivery, Rejection};
use contract::repository::{Decided, RepositoryCode, Unapproved};
use contract::shapes::Failure;
use contract::tool::Tool;
use contract::{ErrorCode, RequestId, Seq, SessionId};
use fakes::{Deadline, Scripted, ScriptedProvider};
use log::{Log, Watcher};
use serde_json::json;

use support::{
    DEADLINE, MODEL, Session, TestTool, delivery, ignore, of_kind, read_until, spawn_run,
};

/// The marker the late preparation's prompt patch carries.
const MARKER: &str = "late extension text";
/// The tool the late preparation registers.
const LATE_TOOL: &str = "late_tool";

/// A fake repository: a fixed list of unapproved items, recording every
/// approval unless `obsolete`, which answers approvals as gone stale.
struct Fake {
    list: Mutex<Vec<Unapproved>>,
    obsolete: AtomicBool,
}

impl Fake {
    fn new(list: Vec<Unapproved>) -> Arc<Self> {
        Arc::new(Self {
            list: Mutex::new(list),
            obsolete: AtomicBool::new(false),
        })
    }

    fn obsolete(list: Vec<Unapproved>) -> Arc<Self> {
        Arc::new(Self {
            list: Mutex::new(list),
            obsolete: AtomicBool::new(true),
        })
    }
}

impl RepositoryCode for Fake {
    fn file(&self, _: &str) -> Result<String, Failure> {
        Err(Failure {
            code: ErrorCode::IoFailed,
            message: "This fake does not read offered files.".into(),
            retry_after_ms: None,
            provider: None,
        })
    }

    fn unapproved(&self) -> Result<Vec<Unapproved>, Failure> {
        Ok(self.list.lock().unwrap().clone())
    }

    fn decide(&self, _: &OfferedItem, _: OfferDecision) -> Result<Decided, Failure> {
        if self.obsolete.load(Ordering::SeqCst) {
            return Ok(Decided::Obsolete);
        }
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

fn ext(name: &str) -> Unapproved {
    item(OfferedKind::Extension, name, false)
}

fn hook(name: &str) -> Unapproved {
    item(OfferedKind::Hook, name, false)
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

fn with_late(session: &mut Session, prepare: r#loop::PrepareReload) {
    session.looped = Some(session.looped.take().unwrap().late_extensions(prepare));
}

/// A late preparer returning the prepared set: the marker text, the late
/// tool, one loaded extension and one notice. `calls` counts preparations;
/// `installed` counts installations.
fn late_preparer(calls: &Arc<AtomicUsize>, installed: &Arc<AtomicUsize>) -> r#loop::PrepareReload {
    let calls = Arc::clone(calls);
    let installed = Arc::clone(installed);
    Arc::new(move |route: &r#loop::ReloadRoute| {
        assert_eq!(route.model, MODEL);
        assert_eq!(route.credential.as_deref(), Some("work"));
        calls.fetch_add(1, Ordering::SeqCst);
        let installed = Arc::clone(&installed);
        Ok(r#loop::ReloadPrepared {
            prompt: r#loop::PromptPatch {
                system: None,
                append: Some(MARKER.to_owned()),
                extensions: Vec::new(),
                extension_dirs: Vec::new(),
                extension_sections: Vec::new(),
                skills_disabled: Vec::new(),
                cache_lifetime: CacheLifetime::FiveMinutes,
            },
            tools: vec![(
                "repo".to_owned(),
                Arc::new(TestTool::reads(LATE_TOOL, "Loaded.")) as Arc<dyn Tool>,
            )],
            extensions: vec![LoadedExtension {
                name: "late".into(),
                version: "1".into(),
                replaces: Vec::new(),
            }],
            notices: vec![Notice {
                code: ErrorCode::ExtensionFailed,
                message: "The repository's `late` extension reported a warning.".into(),
                extension: Some("late".into()),
            }],
            server_sync: r#loop::ServerSync::default(),
            hooks: None,
            reviewer: Err(Failure {
                code: ErrorCode::NoModel,
                message: "no reviewer".into(),
                retry_after_ms: None,
                provider: None,
            }),
            reviewer_observe: None,
            credential_files: Vec::new(),
            settings: r#loop::ReloadSettings {
                reviewer_notes: "late notes".into(),
                limits: r#loop::BlockLimits::default(),
                retry: r#loop::Retry::default(),
                handoff: r#loop::HandoffSettings::default(),
                budget: None,
                idle_exit: None,
                warm_cap: None,
            },
            installed: Box::new(move || {
                installed.fetch_add(1, Ordering::SeqCst);
            }),
        })
    })
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
    send_on(&session.inbox, request, answer)
}

fn send_on(
    inbox: &mpsc::Sender<Delivery>,
    request: &RequestId,
    answer: ReplyAnswer,
) -> mpsc::Receiver<Answer> {
    let (tx, rx) = mpsc::channel();
    inbox
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

#[track_caller]
fn ended(finished: &mpsc::Receiver<Result<(), r#loop::Error>>) -> Result<(), r#loop::Error> {
    Deadline::after(DEADLINE)
        .recv(finished)
        .expect("run ended in time")
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

#[test]
fn an_approved_extension_installs_before_the_preamble() {
    let code = Fake::new(vec![ext("db")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let calls = Arc::new(AtomicUsize::new(0));
    let installed = Arc::new(AtomicUsize::new(0));
    with_late(&mut session, late_preparer(&calls, &installed));
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
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one preparation");
    assert_eq!(installed.load(Ordering::SeqCst), 1, "one installation");
    // The log order: the resolved offer, the late set, then the first
    // preamble; no `reloaded` anywhere. The prepared notice is
    // ephemeral, so it is pinned on the watcher's lines below.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "extensions_loaded",
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
    // The prepared notice lands between the late set and the preamble.
    let watched: Vec<&str> = lines.iter().map(|line| line.kind.as_str()).collect();
    let set = watched
        .iter()
        .position(|kind| *kind == "extensions_loaded")
        .unwrap();
    let notice = watched.iter().position(|kind| *kind == "notice").unwrap();
    let built = watched
        .iter()
        .position(|kind| *kind == "preamble_built")
        .unwrap();
    assert!(set < notice && notice < built, "{watched:?}");
    // `extensions_loaded` carries the prepared set.
    let loaded = of_kind(&lines, "extensions_loaded");
    assert_eq!(loaded.len(), 1);
    assert_eq!(
        loaded[0].payload["extensions"],
        json!([{ "name": "late", "version": "1" }])
    );
    // The first request carries the marker and the prepared tool.
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].system_prompt.contains(MARKER),
        "late patch stands for the first request"
    );
    assert!(
        requests[0].tools.iter().any(|t| t.name == LATE_TOOL),
        "{:?}",
        requests[0]
            .tools
            .iter()
            .map(|t| &t.name)
            .collect::<Vec<_>>()
    );
}

#[test]
fn approving_only_a_hook_or_a_server_installs_nothing() {
    let code = Fake::new(vec![hook("h"), server("s")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let calls = Arc::new(AtomicUsize::new(0));
    let installed = Arc::new(AtomicUsize::new(0));
    with_late(&mut session, late_preparer(&calls, &installed));
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let rx = send_reply(
        &session,
        &request,
        decisions(&[OfferDecision::Approve, OfferDecision::Approve]),
    );
    assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
    let (_, lines) = until_kind(watcher, "turn_completed");
    assert!(lines.iter().any(|l| l.kind == "preamble_built"));
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no preparation");
    assert_eq!(installed.load(Ordering::SeqCst), 0, "no installation");
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
}

#[test]
fn a_never_or_skipped_extension_installs_nothing() {
    let never = Unapproved {
        never: true,
        ..ext("never")
    };
    let code = Fake::new(vec![ext("db"), never]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let calls = Arc::new(AtomicUsize::new(0));
    let installed = Arc::new(AtomicUsize::new(0));
    with_late(&mut session, late_preparer(&calls, &installed));
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    // The offer holds only `db`: the never'd content is filtered out
    // without offering.
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let offered = lines.last().unwrap();
    assert_eq!(offered.payload["items"].as_array().unwrap().len(), 1);
    let request = request_of(offered);
    let rx = send_reply(&session, &request, decisions(&[OfferDecision::Skip]));
    assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
    let (_, lines) = until_kind(watcher, "turn_completed");
    assert!(lines.iter().any(|l| l.kind == "preamble_built"));
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no preparation");
    assert_eq!(installed.load(Ordering::SeqCst), 0, "no installation");
    assert!(
        !durable_kinds(&session)
            .iter()
            .any(|kind| kind == "extensions_loaded"),
        "no late step"
    );
}

#[test]
fn an_approval_answered_obsolete_installs_nothing() {
    let code = Fake::obsolete(vec![ext("db")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let calls = Arc::new(AtomicUsize::new(0));
    let installed = Arc::new(AtomicUsize::new(0));
    with_late(&mut session, late_preparer(&calls, &installed));
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
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no preparation");
    assert_eq!(installed.load(Ordering::SeqCst), 0, "no installation");
    assert!(
        !durable_kinds(&session)
            .iter()
            .any(|kind| kind == "extensions_loaded"),
        "no late step"
    );
}

#[test]
fn an_approval_without_a_late_preparer_changes_nothing() {
    let code = Fake::new(vec![ext("db")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
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
    assert!(
        !durable_kinds(&session)
            .iter()
            .any(|kind| kind == "extensions_loaded"),
        "no late step"
    );
}

#[test]
fn a_rejected_late_preparation_fails_the_run() {
    let code = Fake::new(vec![ext("db")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let calls = Arc::new(AtomicUsize::new(0));
    let prepared = calls.clone();
    session.looped = Some(session.looped.take().unwrap().late_extensions(Arc::new(
        move |_: &r#loop::ReloadRoute| {
            prepared.fetch_add(1, Ordering::SeqCst);
            Err(Rejection {
                code: ErrorCode::ExtensionRequiredFailed,
                message: "The repository's required `db` failed: refused.".into(),
            })
        },
    )));
    let installed = Arc::new(AtomicUsize::new(0));
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    session.inbox.send(delivery("go")).unwrap();
    let (_watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let rx = send_reply(&session, &request, decisions(&[OfferDecision::Approve]));
    assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
    let r#loop::Error::RepositoryCode(failure) = ended(&finished).unwrap_err() else {
        panic!("the rejected late step fails the run");
    };
    assert_eq!(failure.code, ErrorCode::ExtensionRequiredFailed);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one preparation");
    assert_eq!(installed.load(Ordering::SeqCst), 0, "no installation");
    assert!(session.provider.requests().is_empty(), "no model request");
    // The failing late step writes nothing: no preamble, no turn.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
        ]
    );
}

#[test]
fn a_reload_before_the_first_prompt_builds_twice_around_the_late_step() {
    let code = Fake::new(vec![ext("db")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let reload_installed = Arc::new(AtomicUsize::new(0));
    let seen = reload_installed.clone();
    let reload_tool = Arc::new(TestTool::reads("reload_tool", "Reloaded."));
    session.looped = Some(session.looped.take().unwrap().reloader(Arc::new(
        move |_: &r#loop::ReloadRoute| {
            let seen = seen.clone();
            Ok(r#loop::ReloadPrepared {
                prompt: r#loop::PromptPatch {
                    system: Some("Reloaded system bytes.".into()),
                    append: None,
                    extensions: Vec::new(),
                    extension_dirs: Vec::new(),
                    extension_sections: Vec::new(),
                    skills_disabled: Vec::new(),
                    cache_lifetime: CacheLifetime::FiveMinutes,
                },
                tools: vec![("repo".to_owned(), Arc::clone(&reload_tool) as Arc<dyn Tool>)],
                extensions: Vec::new(),
                notices: Vec::new(),
                server_sync: r#loop::ServerSync::default(),
                hooks: None,
                reviewer: Err(Failure {
                    code: ErrorCode::NoModel,
                    message: "no reviewer".into(),
                    retry_after_ms: None,
                    provider: None,
                }),
                reviewer_observe: None,
                credential_files: Vec::new(),
                settings: r#loop::ReloadSettings {
                    reviewer_notes: "reloaded notes".into(),
                    limits: r#loop::BlockLimits::default(),
                    retry: r#loop::Retry::default(),
                    handoff: r#loop::HandoffSettings::default(),
                    budget: None,
                    idle_exit: None,
                    warm_cap: None,
                },
                installed: Box::new(move || {
                    seen.fetch_add(1, Ordering::SeqCst);
                }),
            })
        },
    )));
    let calls = Arc::new(AtomicUsize::new(0));
    let installed = Arc::new(AtomicUsize::new(0));
    with_late(&mut session, late_preparer(&calls, &installed));
    let watcher = session.log.watch();
    let finished = spawn_run(&mut session);
    let (ack_tx, ack_rx) = mpsc::channel();
    session
        .inbox
        .send(Delivery::Reload(Ack(Box::new(move |answer| {
            ack_tx.send(answer).unwrap_or(());
        }))))
        .unwrap();
    session.inbox.send(delivery("go")).unwrap();
    let (watcher, _) = until_kind(watcher, "reloaded");
    assert!(
        Deadline::after(DEADLINE).recv(&ack_rx).unwrap().is_ok(),
        "the reload is accepted"
    );
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let rx = send_reply(&session, &request, decisions(&[OfferDecision::Approve]));
    assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
    let (_, lines) = until_kind(watcher, "turn_completed");
    assert!(lines.iter().any(|l| l.kind == "preamble_built"));
    close(&session);
    ended(&finished).unwrap();
    assert_eq!(reload_installed.load(Ordering::SeqCst), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one late preparation");
    assert_eq!(installed.load(Ordering::SeqCst), 1, "one late installation");
    // The reload's build, then the offer, then the late set and a second
    // build before the turn. The late notice is ephemeral, pinned below.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "reloaded",
            "extensions_loaded",
            "preamble_built",
            "repository_code_offered",
            "repository_code_resolved",
            "extensions_loaded",
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
    let watched: Vec<&str> = lines.iter().map(|line| line.kind.as_str()).collect();
    let set = watched
        .iter()
        .position(|kind| *kind == "extensions_loaded")
        .unwrap();
    let notice = watched.iter().position(|kind| *kind == "notice").unwrap();
    let built = watched
        .iter()
        .position(|kind| *kind == "preamble_built")
        .unwrap();
    assert!(set < notice && notice < built, "{watched:?}");
    let requests = session.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].system_prompt.contains(MARKER),
        "the late patch stands for the first request"
    );
    assert!(
        requests[0].tools.iter().any(|t| t.name == LATE_TOOL),
        "the late tool is declared"
    );
    assert!(
        requests[0].tools.iter().all(|t| t.name != "reload_tool"),
        "the late set replaces the reload's tools"
    );
}

/// Starts B from A's session: a new log beside A's, rewound from `from`
/// with `note`, reading `code` and preparing late with `prepare`.
fn start_rewound(
    session: &Session,
    from: contract::Seq,
    note: String,
    code: &Arc<Fake>,
    prepare: r#loop::PrepareReload,
    script: Vec<Scripted>,
) -> (
    r#loop::Loop,
    mpsc::Sender<Delivery>,
    Arc<ScriptedProvider>,
    Arc<Log>,
    std::path::PathBuf,
) {
    let home = session.dir.parent().unwrap().to_path_buf();
    let clock = session.clock.clone();
    let log = Arc::new(Log::create(&home, SessionId("s_late".into()), clock.clone()).unwrap());
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
    let code: Arc<dyn RepositoryCode> = Arc::clone(code) as Arc<dyn RepositoryCode>;
    let looped = r#loop::Loop::rewound(
        r#loop::Session {
            log: Arc::clone(&log),
            provider: provider.clone(),
            model: r#loop::Model {
                reference: MODEL.into(),
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
            from: contract::shapes::Point {
                session_id: SessionId("s_test".into()),
                seq: from,
            },
            worktree: None,
            how: r#loop::Continuation::Rewind {
                note,
                summary: None,
                name: None,
            },
        },
    )
    .unwrap()
    .repository_code(code)
    .late_extensions(prepare);
    (looped, tx, provider, log, dir)
}

#[test]
fn a_rewound_loop_drops_its_logged_preamble_for_the_late_step() {
    let code = Fake::new(vec![ext("db")]);
    let mut session = Session::new(vec![Scripted::text("one-done")], None);
    session.inbox.send(delivery("one")).unwrap();
    assert_eq!(session.turn(), Some(TurnOutcome::Completed));
    let at = log::read(&session.dir)
        .unwrap()
        .last()
        .unwrap()
        .seq
        .unwrap()
        .0;
    let note = r#loop::rewind_note(&session.dir, Seq(at)).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let installed = Arc::new(AtomicUsize::new(0));
    let (mut looped, tx, provider, b_log, dir) = start_rewound(
        &session,
        Seq(at),
        note,
        &code,
        late_preparer(&calls, &installed),
        vec![Scripted::text("two-done")],
    );
    b_log
        .append(&Event::Clients(Clients { count: 1 }), None, None)
        .unwrap();
    let watcher = b_log.watch();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let outcome = looped.turn().unwrap();
        done.send((looped, outcome)).unwrap();
    });
    tx.send(delivery("two")).unwrap();
    let (watcher, lines) = until_kind(watcher, "repository_code_offered");
    let request = request_of(lines.last().unwrap());
    let rx = send_on(&tx, &request, decisions(&[OfferDecision::Approve]));
    assert!(Deadline::after(DEADLINE).recv(&rx).unwrap().is_ok());
    let (_, lines) = until_kind(watcher, "turn_completed");
    let (_looped, outcome) = Deadline::after(DEADLINE)
        .recv(&finished)
        .expect("the turn ended in time");
    assert_eq!(outcome, Some(TurnOutcome::Completed));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one preparation");
    assert_eq!(installed.load(Ordering::SeqCst), 1, "one installation");
    // The logged preamble stays in A's log; B writes one preamble, after
    // the late set, before its first request. B inherits the opening, so
    // it writes none. The late notice is ephemeral, pinned below.
    let kinds: Vec<String> = log::read(&dir)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert_eq!(
        ordered(kinds.iter().map(String::as_str)),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "extensions_loaded",
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
    let watched: Vec<&str> = lines.iter().map(|line| line.kind.as_str()).collect();
    let set = watched
        .iter()
        .position(|kind| *kind == "extensions_loaded")
        .unwrap();
    let notice = watched.iter().position(|kind| *kind == "notice").unwrap();
    let built = watched
        .iter()
        .position(|kind| *kind == "preamble_built")
        .unwrap();
    assert!(set < notice && notice < built, "{watched:?}");
    let requests = provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].system_prompt.contains(MARKER),
        "the late patch stands for the first request"
    );
    assert!(
        requests[0].tools.iter().any(|t| t.name == LATE_TOOL),
        "the late tool is declared"
    );
}

#[test]
fn a_post_offer_server_load_comes_before_the_late_step() {
    let code = Fake::new(vec![ext("db")]);
    let mut session = session(&code, Some(1), vec![Scripted::text("Hello.")]);
    let load_calls = Arc::new(AtomicUsize::new(0));
    let seen = load_calls.clone();
    session.looped = Some(session.looped.take().unwrap().approved_code(Arc::new(
        move |_: &r#loop::OfferOutcome| {
            seen.fetch_add(1, Ordering::SeqCst);
            Ok(r#loop::Approved {
                tools: Vec::new(),
                events: vec![Event::McpServerFailed(McpServerFailed {
                    server: "db".into(),
                    reason: ServerFailure::StartFailed,
                    will_restart: false,
                    error: Failure {
                        code: ErrorCode::McpServerUnavailable,
                        message: "The MCP server `db` failed to start: refused.".into(),
                        retry_after_ms: None,
                        provider: None,
                    },
                })],
            })
        },
    )));
    let calls = Arc::new(AtomicUsize::new(0));
    let installed = Arc::new(AtomicUsize::new(0));
    with_late(&mut session, late_preparer(&calls, &installed));
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
    assert_eq!(load_calls.load(Ordering::SeqCst), 1, "one post-offer load");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "one late preparation");
    assert_eq!(installed.load(Ordering::SeqCst), 1, "one late installation");
    // The post-offer load's events come before the late set. The late
    // notice is ephemeral, pinned below.
    assert_eq!(
        ordered(durable_kinds(&session).iter().map(String::as_str)),
        [
            "session_started",
            "repository_code_offered",
            "repository_code_resolved",
            "mcp_server_failed",
            "extensions_loaded",
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
    let watched: Vec<&str> = lines.iter().map(|line| line.kind.as_str()).collect();
    let set = watched
        .iter()
        .position(|kind| *kind == "extensions_loaded")
        .unwrap();
    let notice = watched.iter().position(|kind| *kind == "notice").unwrap();
    let built = watched
        .iter()
        .position(|kind| *kind == "preamble_built")
        .unwrap();
    assert!(set < notice && notice < built, "{watched:?}");
}
