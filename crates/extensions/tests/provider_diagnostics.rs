//! Session diagnostics at the public API (`docs/state.md`, "Diagnostic
//! logs"): one timer scope per non-tool hook dispatch, and one observed
//! line per eligible Lua-provider HTTP request, without recording URLs.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::expect_used, reason = "test helpers; a failure is the test's")]
#![allow(clippy::panic, reason = "test helpers; a hang is the test's failure")]
#![allow(
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use common::{Setup, install_lua};
use config::{Config, ProjectKey, Sources};
use contract::SessionId;
use contract::clock::Clock;
use contract::clock::Wake;
use contract::diag::{DebugLog, HookChain, Op, ProviderRequest, Timing};
use contract::events::CallStatus;
use contract::events::HandoffTrigger;
use contract::hook::{
    AfterToolCall, AfterToolOutcome, BeforeHandoff, BeforeMessage, BeforeMessageOutcome,
    BeforeModelCall, BeforeToolCall, CheckOutcome, ContextOutcome, Hooks, SessionStart,
    StartReason, TurnEnd, TurnStart,
};
use contract::shapes::{ContentPart, DeclaredEffects, Tokens, Usage};
use contract::tool::Cancel;
use extensions::SessionExtensions;
use fakes::clock::FakeClock;
use fakes::{Deadline, NoLock};
use serde_json::{Map, json};

/// A cancel that never fires.
struct Never;

impl Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn Wake>) {}
}

/// A cancel the test fires.
struct Flag {
    fired: AtomicBool,
}

impl Flag {
    fn new() -> Self {
        Self {
            fired: AtomicBool::new(false),
        }
    }

    fn fire(&self) {
        self.fired.store(true, Ordering::SeqCst);
    }
}

impl Cancel for Flag {
    fn is_cancelled(&self) -> bool {
        self.fired.load(Ordering::SeqCst)
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn Wake>) {}
}

/// A sink that keeps every request it is given.
#[derive(Default)]
struct Sink {
    requests: Mutex<Vec<ProviderRequest>>,
}

impl DebugLog for Sink {
    fn debugging(&self) -> bool {
        true
    }

    fn provider_request(&self, request: &ProviderRequest) {
        self.requests.lock().unwrap().push(request.clone());
    }
}

/// The recorded timer scopes: each invocation's operation and tool.
type Scopes = Arc<Mutex<Vec<(Op, Option<String>)>>>;

/// A timing that records each invocation's operation and tool.
fn recording() -> (Timing, Scopes) {
    let calls: Scopes = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let timing: Timing = Arc::new(move |op, tool, _started| {
        seen.lock().unwrap().push((op, tool.map(str::to_owned)));
        Some(Box::new(()) as Box<dyn Send>)
    });
    (timing, calls)
}

#[track_caller]
fn load(setup: &Setup, clock: Arc<FakeClock>) -> Arc<SessionExtensions> {
    let config = Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap();
    let home = setup.home();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
        let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
    });
    Arc::new(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .expect("waited for the extensions to load"),
    )
}

/// Installs diagnostics and returns the session with its recorded calls.
fn diagnosed(session: Arc<SessionExtensions>, sink: Arc<Sink>) -> (Arc<SessionExtensions>, Scopes) {
    let (timing, calls) = recording();
    let session = Arc::new(
        Arc::try_unwrap(session)
            .unwrap_or_else(|_| panic!("the session is still shared"))
            .with_diagnostics(timing, sink),
    );
    (session, calls)
}

fn session_id() -> SessionId {
    SessionId("s_4c1d".into())
}

fn usage() -> Usage {
    Usage {
        tokens: Tokens {
            input: 10,
            cache_read: 0,
            cache_write: BTreeMap::new(),
            output: 5,
        },
        cost: None,
        subscription_cost: 0.0,
    }
}

fn hook(point: &str, phase: &str, returned: &str) -> String {
    format!(
        "fiber.hook(\"{point}\", {{ phase = \"{phase}\", timeout = 30000, on_failure = \"non-blocking\",\n\
           run = function(arg) return {returned} end }})\n"
    )
}

/// An extension registering every non-tool chain.
fn all_chains() -> String {
    [
        hook("session_start", "transform", "{ context = \"s\" }"),
        hook("turn_start", "transform", "{ context = \"t\" }"),
        hook("turn_end", "transform", "{ context = \"e\" }"),
        hook("before_message", "sanitize", "{}"),
        hook("before_model_call", "transform", "{}"),
        hook("before_handoff", "transform", "{ note = \"h\" }"),
    ]
    .join("\n")
}

fn dispatch_all(session: &Arc<SessionExtensions>) {
    let id = session_id();
    let used = usage();
    let content = vec![ContentPart::Text { text: "hi".into() }];
    fakes::within(
        "session.session_start",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(session);
            move || {
                session.session_start(&SessionStart {
                    reason: StartReason::Start,
                    session_id: &id,
                    workspace: "/w",
                    cancel: &Never,
                })
            }
        },
    );
    fakes::within(
        "session.turn_start",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(session);
            move || {
                session.turn_start(&TurnStart {
                    input: &[],
                    cancel: &Never,
                })
            }
        },
    );
    fakes::within(
        "session.turn_end",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(session);
            move || {
                session.turn_end(&TurnEnd {
                    reply: "done",
                    cancel: &Never,
                })
            }
        },
    );
    fakes::within(
        "session.before_message",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(session);
            move || {
                session.before_message(&BeforeMessage {
                    content: &content,
                    sender: None,
                })
            }
        },
    );
    fakes::within(
        "session.before_model_call",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(session);
            move || {
                session.before_model_call(&BeforeModelCall {
                    model: "p/m",
                    usage: &used,
                    cancel: &Never,
                })
            }
        },
    );
    fakes::within(
        "session.before_handoff",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(session);
            move || {
                session.before_handoff(&BeforeHandoff {
                    trigger: HandoffTrigger::Auto,
                    instructions: None,
                    conversation: &[],
                    cancel: &Never,
                })
            }
        },
    );
}

fn ops(calls: &Scopes) -> Vec<(Op, Option<String>)> {
    calls.lock().unwrap().clone()
}

#[test]
fn each_chain_emits_one_scope_per_dispatch() {
    let setup = Setup::new();
    install_lua(&setup, "one", &json!({}), &all_chains());
    install_lua(
        &setup,
        "two",
        &json!({}),
        &[
            hook("session_start", "transform", "{ context = \"s2\" }"),
            hook("turn_start", "transform", "{ context = \"t2\" }"),
            hook("turn_end", "transform", "{ context = \"e2\" }"),
            hook("before_message", "check", "{}"),
            hook("before_model_call", "transform", "{}"),
        ]
        .join("\n"),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let sink = Arc::new(Sink::default());
    let (session, calls) = diagnosed(session, sink);
    dispatch_all(&session);
    // One scope per entered public dispatch, however many extensions and
    // phases each chain ran: never one per process or callback.
    assert_eq!(
        ops(&calls),
        [
            (Op::Hook(HookChain::SessionStart), None),
            (Op::Hook(HookChain::TurnStart), None),
            (Op::Hook(HookChain::TurnEnd), None),
            (Op::Hook(HookChain::BeforeMessage), None),
            (Op::Hook(HookChain::BeforeModelCall), None),
            (Op::Hook(HookChain::BeforeHandoff), None),
        ]
    );
    // Two successive dispatches emit twice.
    let id = session_id();
    fakes::within(
        "session.session_start",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.session_start(&SessionStart {
                    reason: StartReason::Start,
                    session_id: &id,
                    workspace: "/w",
                    cancel: &Never,
                })
            }
        },
    );
    assert_eq!(ops(&calls).len(), 7);
    assert_eq!(ops(&calls)[6], (Op::Hook(HookChain::SessionStart), None));
}

#[test]
fn empty_chains_emit_one_scope_each() {
    let setup = Setup::new();
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    assert!(!session.has_hooks());
    let sink = Arc::new(Sink::default());
    let (session, calls) = diagnosed(session, sink);
    dispatch_all(&session);
    assert_eq!(
        ops(&calls),
        [
            (Op::Hook(HookChain::SessionStart), None),
            (Op::Hook(HookChain::TurnStart), None),
            (Op::Hook(HookChain::TurnEnd), None),
            (Op::Hook(HookChain::BeforeMessage), None),
            (Op::Hook(HookChain::BeforeModelCall), None),
            (Op::Hook(HookChain::BeforeHandoff), None),
        ]
    );
}

#[test]
fn a_nil_hook_runs_once_per_dispatch() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    let url = server.url();
    install_lua(
        &setup,
        "one",
        &json!({}),
        &format!(
            "fiber.hook(\"before_message\", {{ timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(msg) host.http({{ url = \"{url}/x\" }}) return nil end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let sink = Arc::new(Sink::default());
    let (session, calls) = diagnosed(session, sink);
    let content = vec![ContentPart::Text { text: "hi".into() }];
    let answer = fakes::within(
        "session.before_message",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.before_message(&BeforeMessage {
                    content: &content,
                    sender: None,
                })
            }
        },
    );
    assert!(matches!(
        answer.outcome,
        BeforeMessageOutcome::Passed { .. }
    ));
    // The nil return is the hook's answer, not a retry: its HTTP ran once
    // and the dispatch holds one scope.
    assert_eq!(server.requests().len(), 1);
    assert_eq!(ops(&calls), [(Op::Hook(HookChain::BeforeMessage), None)]);
}

#[test]
fn refusal_and_failure_return_inside_the_scope() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "one",
        &json!({}),
        &[
            "fiber.hook(\"before_message\", { timeout = 30000, on_failure = \"blocking\",\n\
               run = function(msg) return { refuse = \"no\" } end })",
            "fiber.hook(\"before_model_call\", { timeout = 30000, on_failure = \"blocking\",\n\
               run = function(call) error(\"boom\") end })",
        ]
        .join("\n"),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let sink = Arc::new(Sink::default());
    let (session, calls) = diagnosed(session, sink);
    let content = vec![ContentPart::Text { text: "hi".into() }];
    let refused = fakes::within(
        "session.before_message",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.before_message(&BeforeMessage {
                    content: &content,
                    sender: None,
                })
            }
        },
    );
    assert!(matches!(refused.outcome, BeforeMessageOutcome::Refused(_)));
    let used = usage();
    let failed = fakes::within(
        "session.before_model_call",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.before_model_call(&BeforeModelCall {
                    model: "p/m",
                    usage: &used,
                    cancel: &Never,
                })
            }
        },
    );
    assert!(matches!(failed.outcome, CheckOutcome::Failed(_)));
    assert_eq!(
        ops(&calls),
        [
            (Op::Hook(HookChain::BeforeMessage), None),
            (Op::Hook(HookChain::BeforeModelCall), None),
        ]
    );
}

#[test]
fn before_model_call_refusal_and_nonblocking_error_keep_their_scopes() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "one",
        &json!({}),
        &[
            "fiber.hook(\"before_model_call\", { timeout = 30000, on_failure = \"blocking\",\n\
               run = function(call) return { refuse = \"nope\" } end })",
            "fiber.hook(\"before_message\", { timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(msg) error(\"boom\") end })",
        ]
        .join("\n"),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let sink = Arc::new(Sink::default());
    let (session, calls) = diagnosed(session, sink);
    let used = usage();
    let refused = fakes::within(
        "session.before_model_call",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.before_model_call(&BeforeModelCall {
                    model: "p/m",
                    usage: &used,
                    cancel: &Never,
                })
            }
        },
    );
    assert!(matches!(refused.outcome, CheckOutcome::Refused(_)));
    let content = vec![ContentPart::Text { text: "hi".into() }];
    let passed = fakes::within(
        "session.before_message",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.before_message(&BeforeMessage {
                    content: &content,
                    sender: None,
                })
            }
        },
    );
    // A non-blocking failure drops the change, keeps a notice and passes.
    assert!(matches!(
        passed.outcome,
        BeforeMessageOutcome::Passed { .. }
    ));
    assert_eq!(passed.notices.len(), 1);
    assert_eq!(
        ops(&calls),
        [
            (Op::Hook(HookChain::BeforeModelCall), None),
            (Op::Hook(HookChain::BeforeMessage), None),
        ]
    );
}

#[test]
fn cancel_between_lifecycle_callbacks_keeps_one_scope() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    server.hold_from(1, Duration::from_secs(30));
    let url = server.url();
    install_lua(
        &setup,
        "one",
        &json!({}),
        &format!(
            "fiber.hook(\"turn_start\", {{ timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(ctx) host.http({{ url = \"{url}/first\" }}) return {{ context = \"first\" }} end }})\n\
             fiber.hook(\"turn_start\", {{ timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(ctx) return {{ context = \"second\" }} end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let sink = Arc::new(Sink::default());
    let (session, calls) = diagnosed(session, sink);
    let cancel = Arc::new(Flag::new());
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let session = Arc::clone(&session);
        let cancel = Arc::clone(&cancel);
        std::thread::spawn(move || {
            let answer = session.turn_start(&TurnStart {
                input: &[],
                cancel: cancel.as_ref(),
            });
            let _sent = tx.send(answer);
        })
    };
    assert!(
        server.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the first hook suspended on HTTP"
    );
    cancel.fire();
    server.release();
    let answer = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the cancelled chain");
    worker.join().expect("the dispatch thread joins");
    // The cancel lands between the two callbacks: the first ran, the
    // second never does, and a cancelled chain keeps no context.
    match &answer.outcome {
        ContextOutcome::Added(context) => {
            assert!(context.is_empty(), "a cancelled chain keeps no context");
        }
        ContextOutcome::Failed(failure) => panic!("the chain adds context: {failure:?}"),
    }
    assert_eq!(server.requests().len(), 1, "only the first hook ran");
    assert_eq!(ops(&calls), [(Op::Hook(HookChain::TurnStart), None)]);
}

#[test]
fn tool_hooks_execute_without_timing() {
    let setup = Setup::new();
    install_lua(
        &setup,
        "one",
        &json!({}),
        &[
            "fiber.hook(\"before_tool\", { timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(call) return { arguments = { x = 1 } } end })",
            "fiber.hook(\"after_tool\", { phase = \"transform\", timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(call) return { content = call.content .. \"|tagged\" } end })",
        ]
        .join("\n"),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let sink = Arc::new(Sink::default());
    let (session, calls) = diagnosed(session, sink);
    let declared = DeclaredEffects {
        effects: Vec::new(),
        paths: None,
        reversible: false,
        confined: false,
    };
    let arguments = Map::new();
    let rewritten = fakes::within(
        "session.before_tool",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.before_tool(&BeforeToolCall {
                    tool: "read",
                    arguments: &arguments,
                    declared: &declared,
                })
            }
        },
    );
    assert!(matches!(
        rewritten.outcome,
        contract::hook::BeforeToolOutcome::Rewritten { .. }
    ));
    let changed = fakes::within(
        "session.after_tool",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.after_tool(&AfterToolCall {
                    tool: "read",
                    arguments: &Map::new(),
                    status: CallStatus::Completed,
                    content: "x",
                    details: None,
                    process: None,
                    spilled: None,

                    not_regular: false,
                    delivery: None,
                })
            }
        },
    );
    assert!(matches!(changed.outcome, AfterToolOutcome::Changed { .. }));
    // Both tool chains ran their callbacks but took no P5 timer.
    assert!(ops(&calls).is_empty());
}

#[test]
fn a_session_without_diagnostics_dispatches_and_writes_no_logs() {
    let setup = Setup::new();
    install_lua(&setup, "one", &json!({}), &all_chains());
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    dispatch_all(&session);
    assert!(
        !setup.home().join("logs").exists(),
        "no diagnostics means no log files"
    );
}

/// A timer guard that stamps its start at invocation and its end at drop on
/// the fake clock.
struct Span {
    clock: Arc<FakeClock>,
    start: Instant,
    ends: Arc<Mutex<Vec<(Instant, Instant)>>>,
}

impl Drop for Span {
    fn drop(&mut self) {
        self.ends
            .lock()
            .unwrap()
            .push((self.start, self.clock.now()));
    }
}

#[test]
fn a_suspended_dispatch_spans_the_whole_call() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    server.hold_from(1, Duration::from_secs(30));
    let url = server.url();
    install_lua(
        &setup,
        "one",
        &json!({}),
        &format!(
            "fiber.hook(\"before_message\", {{ timeout = 30000, on_failure = \"non-blocking\",\n\
               run = function(msg) host.http({{ url = \"{url}/held\" }}) return {{}} end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock.clone());
    let ends: Arc<Mutex<Vec<(Instant, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let finished = Arc::clone(&ends);
    let stamped = clock.clone();
    let timed: Timing = Arc::new(move |_op, _tool, _started| {
        let guard = Span {
            clock: Arc::clone(&stamped),
            start: stamped.now(),
            ends: Arc::clone(&finished),
        };
        Some(Box::new(guard) as Box<dyn Send>)
    });
    let sink = Arc::new(Sink::default());
    let session = Arc::new(
        Arc::try_unwrap(session)
            .unwrap_or_else(|_| panic!("the session is still shared"))
            .with_diagnostics(timed, sink),
    );
    let mark = clock.now();
    let (id_tx, id_rx) = std::sync::mpsc::channel();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            let _sent = id_tx.send(std::thread::current().id());
            let content = vec![ContentPart::Text { text: "hi".into() }];
            let answer = session.before_message(&BeforeMessage {
                content: &content,
                sender: None,
            });
            let _sent = tx.send(answer);
        })
    };
    let id = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&id_rx)
        .expect("waited for the dispatch thread");
    assert!(
        server.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the hook suspended on HTTP"
    );
    // The dispatch waits on the fake clock past its own check before fake
    // time moves.
    // The dispatch waits on the fake clock past its own check before fake
    // time moves: once its callback suspends, the caller waits until the
    // callback's deadline plus the scheduler's grace for a suspended call
    // (`docs/extensions.md`, "How an extension runs"), a stable park the
    // transient deadline wait ends in before the HTTP receipt returns.
    assert!(
        clock.await_thread_parked(
            id,
            Some(mark + Duration::from_secs(31)),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the hook caller parked past its check on the suspended callback"
    );
    clock.advance_when_parked(
        Some(mark + Duration::from_secs(31)),
        Some(id),
        Duration::from_millis(2500),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    assert!(
        ends.lock().unwrap().is_empty(),
        "no completion while the dispatch is suspended"
    );
    server.release();
    let answer = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the dispatch");
    worker.join().expect("the dispatch thread joins");
    assert!(matches!(
        answer.outcome,
        BeforeMessageOutcome::Passed { .. }
    ));
    let spans = ends.lock().unwrap().clone();
    assert_eq!(spans.len(), 1, "exactly one completion when it returns");
    let (start, end) = spans[0];
    assert_eq!(start, mark);
    assert_eq!(
        end.duration_since(start),
        Duration::from_millis(2500),
        "the scope covers every phase"
    );
}

#[test]
fn a_hook_past_its_deadline_fails_inside_its_scope() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    server.hold_from(1, Duration::from_secs(30));
    let url = server.url();
    install_lua(
        &setup,
        "one",
        &json!({}),
        &format!(
            "fiber.hook(\"before_message\", {{ timeout = 1000, on_failure = \"blocking\",\n\
               run = function(msg) host.http({{ url = \"{url}/held\" }}) return {{}} end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock.clone());
    let sink = Arc::new(Sink::default());
    let (session, calls) = diagnosed(session, sink);
    let mark = clock.now();
    let (id_tx, id_rx) = std::sync::mpsc::channel();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = {
        let session = Arc::clone(&session);
        std::thread::spawn(move || {
            let _sent = id_tx.send(std::thread::current().id());
            let content = vec![ContentPart::Text { text: "hi".into() }];
            let answer = session.before_message(&BeforeMessage {
                content: &content,
                sender: None,
            });
            let _sent = tx.send(answer);
        })
    };
    let id = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&id_rx)
        .expect("waited for the dispatch thread");
    assert!(
        server.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the hook suspended on HTTP"
    );
    // The caller waits on the suspended callback past its own check: the
    // hook's deadline plus the scheduler's grace for a suspended call.
    assert!(
        clock.await_thread_parked(
            id,
            Some(mark + Duration::from_millis(2000)),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the hook caller parked past its check on the suspended callback"
    );
    // Past the hook's deadline but inside its grace: the caller takes the
    // timeout, the chain fails, and the scope still closes exactly once.
    clock.advance_when_parked(
        Some(mark + Duration::from_millis(2000)),
        Some(id),
        Duration::from_millis(1500),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let answer = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the timed-out dispatch");
    assert!(matches!(answer.outcome, BeforeMessageOutcome::Failed(_)));
    assert_eq!(ops(&calls), [(Op::Hook(HookChain::BeforeMessage), None)]);
    // The late reply is discarded: releasing the hold ends the worker
    // without hanging the suite.
    server.release();
    worker.join().expect("the dispatch thread joins");
}

// Provider purposes through the public API: each eligible Lua HTTP
// request records once with its provider and purpose, and nothing else
// records.

use contract::GenerationId;
use contract::signing::{SignRequest, Signer};
use extensions::{CredentialPair, HostScript, HttpEntry, LuaProvider, ModelsArg};

/// A sink that signals each record it is given.
struct Watching {
    requests: Mutex<Vec<ProviderRequest>>,
    arrived: std::sync::mpsc::Sender<ProviderRequest>,
}

impl Watching {
    fn new() -> (Arc<Self>, std::sync::mpsc::Receiver<ProviderRequest>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (
            Arc::new(Self {
                requests: Mutex::new(Vec::new()),
                arrived: tx,
            }),
            rx,
        )
    }

    fn records(&self) -> Vec<ProviderRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl DebugLog for Watching {
    fn debugging(&self) -> bool {
        true
    }

    fn provider_request(&self, request: &ProviderRequest) {
        self.requests.lock().unwrap().push(request.clone());
        let _ignored = self.arrived.send(request.clone());
    }
}

/// A directory holding `init.lua` and one provider file per entry.
fn ext_dir(init: Option<&str>, providers: &[(&str, &str)]) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-provider-observation");
    if let Some(init) = init {
        std::fs::write(dir.path().join("init.lua"), init).unwrap();
    }
    std::fs::create_dir_all(dir.path().join("providers")).unwrap();
    for (name, lua) in providers {
        std::fs::write(
            dir.path().join("providers").join(format!("{name}.lua")),
            lua,
        )
        .unwrap();
    }
    dir
}

fn observed_ext(
    dir: &fakes::TempDir,
    clock: Arc<FakeClock>,
    sink: &Arc<Watching>,
) -> Arc<extensions::LuaExtension> {
    Arc::new(
        extensions::LuaExtension::new("ext", dir.path(), dir.path(), clock)
            .with_diagnostics(sink.clone() as Arc<dyn DebugLog>),
    )
}

fn models_lua(url: &str) -> String {
    models_named_lua("p", url)
}

fn models_named_lua(name: &str, url: &str) -> String {
    format!(
        "fiber.provider(\"{name}\", {{ models = {{ timeout = 60000, run = function()\n\
           local reply = host.http({{ url = \"{url}/models\" }})\n\
           return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{url}\" }}}}\n\
         end }} }})\n"
    )
}

#[test]
fn provider_purposes_record_once_per_attempt() {
    let server = fakes::ProviderServer::start_responding(|request| match request.path.as_str() {
        "/models" => fakes::Response::status(200, "{}"),
        "/quota" => fakes::Response::status(200, "{\"windows\":[]}"),
        "/token" => fakes::Response::status(200, "{\"token\":\"t\",\"expires_at\":1893456000}"),
        "/cost" => fakes::Response::status(200, "0.5"),
        _ => fakes::Response::status(500, "unexpected"),
    })
    .unwrap();
    let url = server.url();
    let dir = ext_dir(
        None,
        &[
            (
                "p",
                &format!(
                    "fiber.provider(\"p\", {{\n\
                       models = {{ timeout = 60000, run = function()\n\
                         host.http({{ url = \"{url}/models\" }})\n\
                         return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{url}\" }}}}\n\
                       end }},\n\
                       quota = {{ timeout = 60000, run = function(q)\n\
                         host.http({{ url = \"{url}/quota\" }})\n\
                         return {{ windows = {{ }} }}\n\
                       end }},\n\
                       cost = {{ timeout = 60000, run = function(c)\n\
                         host.http({{ url = \"{url}/cost\" }})\n\
                         return 0.5\n\
                       end }},\n\
                     }})\n"
                ),
            ),
            (
                "c",
                &format!(
                    "fiber.provider(\"c\", {{\n\
                       credential = {{ timeout = 60000, run = function()\n\
                         host.http({{ method = \"POST\", url = \"{url}/token\" }})\n\
                         return {{ token = \"t\", expires_at = 1893456000 }}\n\
                       end }},\n\
                     }})\n"
                ),
            ),
        ],
    );
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let ext = observed_ext(&dir, clock, &sink);
    let models = LuaProvider::new(Arc::clone(&ext), "p");
    let credentials = LuaProvider::new(Arc::clone(&ext), "c");
    let listed = fakes::within(
        "models.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let models = Arc::clone(&models);
            move || models.list_models(&ModelsArg::default())
        },
    )
    .expect("discovery succeeds");
    assert_eq!(listed.0.len(), 1);
    let pair = CredentialPair {
        credential: "c".to_owned(),
        label: "l".to_owned(),
    };
    let quota = fakes::within("quota", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
        let models = Arc::clone(&models);
        let pair = pair.clone();
        move || models.quota(&pair, None)
    });
    assert!(matches!(
        quota,
        extensions::QuotaEntry::Reported(_) | extensions::QuotaEntry::NoQuota
    ));
    let token = fakes::within(
        "credential_token",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let credentials = Arc::clone(&credentials);
            let pair = pair.clone();
            move || credentials.credential_token(&pair)
        },
    );
    assert!(token.is_ok(), "the stored credential refresh succeeds");
    let cost = fakes::within(
        "models.cost",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let models = Arc::clone(&models);
            move || models.cost(&GenerationId("g1".into()), &url, None)
        },
    )
    .expect("the cost lookup succeeds");
    assert_eq!(cost, Some(0.5));
    // One line per actual HTTP request, with the provider's registered
    // name (not the extension's), the eligible purpose and attempt 1.
    let records = sink.records();
    let purposes: Vec<(String, String)> = records
        .iter()
        .map(|record| {
            (
                record.provider.clone(),
                serde_json::to_value(record.purpose)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect();
    assert_eq!(
        purposes,
        [
            ("p".to_owned(), "model_list".to_owned()),
            ("p".to_owned(), "quota".to_owned()),
            ("c".to_owned(), "token_refresh".to_owned()),
            ("p".to_owned(), "cost".to_owned()),
        ]
    );
    for record in &records {
        assert_eq!(record.attempt, 1);
        assert_eq!(record.status, Some(200));
    }
}

#[test]
fn two_calls_in_one_callback_emit_two_attempt_ones() {
    let server =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let url = server.url();
    let dir = ext_dir(
        None,
        &[(
            "p",
            &format!(
                "fiber.provider(\"p\", {{ models = {{ timeout = 60000, run = function()\n\
                   host.http({{ url = \"{url}/one\" }})\n\
                   host.http({{ url = \"{url}/two\" }})\n\
                   return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{url}\" }}}}\n\
                 end }} }})\n"
            ),
        )],
    );
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let ext = observed_ext(&dir, clock, &sink);
    let provider = LuaProvider::new(ext, "p");
    fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = Arc::clone(&provider);
            move || provider.list_models(&ModelsArg::default())
        },
    )
    .expect("two HTTP calls in one callback succeed");
    assert_eq!(server.requests().len(), 2);
    let records = sink.records();
    assert_eq!(records.len(), 2);
    for record in &records {
        assert_eq!(record.provider, "p");
        assert_eq!(record.attempt, 1);
        assert_eq!(
            serde_json::to_value(record.purpose).unwrap(),
            serde_json::Value::String("model_list".to_owned())
        );
    }
}

#[test]
fn cache_hits_and_scripted_replies_emit_nothing() {
    let server =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let url = server.url();
    let dir = ext_dir(None, &[("p", &models_lua(&url))]);
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let ext = observed_ext(&dir, clock, &sink);
    let provider = LuaProvider::new(Arc::clone(&ext), "p");
    fakes::within(
        "provider.models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = Arc::clone(&provider);
            move || provider.models(&ModelsArg::default())
        },
    )
    .expect("discovery succeeds");
    // Served from memory now: no HTTP, no record.
    fakes::within(
        "provider.models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = Arc::clone(&provider);
            move || provider.models(&ModelsArg::default())
        },
    )
    .expect("the cached list serves");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(sink.records().len(), 1);
    // A scripted reply is not an HTTP request: it emits nothing and still
    // returns its models.
    let script_dir = ext_dir(
        None,
        &[(
            "sq",
            "fiber.provider(\"sq\", { models = { timeout = 60000, run = function()\n\
               local reply = host.http({ url = \"http://scripted.invalid/models\" })\n\
               return json.decode(reply.body)\n\
             end } })\n",
        )],
    );
    let script = HostScript::new(
        vec![HttpEntry {
            request: serde_json::json!({"url": "http://scripted.invalid/models"}),
            reply: Ok((
                200,
                br#"[{"id":"m9","protocol":"openai-responses","base_url":"http://scripted.invalid"}]"#.to_vec(),
            )),
        }],
        Vec::new(),
        Vec::new(),
    );
    let scripted_ext = Arc::new(
        extensions::LuaExtension::new(
            "ext",
            script_dir.path(),
            script_dir.path(),
            FakeClock::new(),
        )
        .with_host_script(script)
        .with_diagnostics(sink.clone() as Arc<dyn DebugLog>),
    );
    let quoted = LuaProvider::new(scripted_ext, "sq");
    let (listed, _) = fakes::within(
        "quoted.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || quoted.list_models(&ModelsArg::default()),
    )
    .expect("the scripted reply parses");
    assert_eq!(
        listed
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["m9"]
    );
    assert_eq!(sink.records().len(), 1);
}

#[test]
fn sign_command_and_entry_load_send_unobserved() {
    let server =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let url = server.url();
    let dir = ext_dir(
        Some(&format!(
            "fiber.command(\"ping\", {{ timeout = 60000, description = \"ping\",\n\
               run = function(text) host.http({{ url = \"{url}/command\" }}) return \"pong\" end }})\n"
        )),
        &[
            (
                "s",
                &format!(
                    "host.http({{ url = \"{url}/load\" }})\n\
                     fiber.provider(\"s\", {{\n\
                       sign = {{ timeout = 60000, run = function(request)\n\
                         host.http({{ url = \"{url}/sign\" }})\n\
                         return {{}}\n\
                       end }},\n\
                     }})\n"
                ),
            ),
            ("p", &models_lua(&url)),
        ],
    );
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let ext = observed_ext(&dir, clock, &sink);
    // The entry script's command runs actual HTTP, unobserved.
    let pong = fakes::within(
        "ext.command",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let ext = Arc::clone(&ext);
            move || ext.command("ping", "")
        },
    )
    .expect("the command succeeds");
    assert_eq!(pong, "pong");
    // Signing sends actual HTTP, unobserved even with a sink installed.
    let signer = LuaProvider::new(Arc::clone(&ext), "s");
    let signed: Vec<(String, String)> = fakes::within(
        "signer.sign",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            move || {
                signer.sign(&SignRequest {
                    method: "GET",
                    url: "http://example.invalid/",
                    headers: &[],
                    body: b"",
                })
            }
        },
    )
    .expect("signing succeeds");
    assert!(signed.is_empty());
    // The provider file's top-level HTTP ran at first use, unobserved.
    let provider = LuaProvider::new(ext, "p");
    fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = Arc::clone(&provider);
            move || provider.list_models(&ModelsArg::default())
        },
    )
    .expect("discovery succeeds");
    let paths: Vec<String> = server
        .requests()
        .iter()
        .map(|request| request.path.clone())
        .collect();
    for path in ["/command", "/sign", "/load", "/models"] {
        assert!(paths.contains(&path.to_owned()), "sent {path}: {paths:?}");
    }
    let records = sink.records();
    assert_eq!(records.len(), 1, "only the models() call records");
    assert_eq!(records[0].provider, "p");
}

#[test]
fn hook_http_sends_unobserved_with_a_sink_installed() {
    let setup = Setup::new();
    let server =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let url = server.url();
    install_lua(
        &setup,
        "one",
        &json!({}),
        &format!(
            "fiber.hook(\"before_message\", {{ timeout = 60000, on_failure = \"non-blocking\",\n\
               run = function(msg) host.http({{ url = \"{url}/hook\" }}) return nil end }})\n"
        ),
    );
    let clock = FakeClock::new();
    let session = load(&setup, clock);
    let sink = Arc::new(Sink::default());
    let (session, _) = diagnosed(session, Arc::clone(&sink));
    let content = vec![ContentPart::Text { text: "hi".into() }];
    fakes::within(
        "session.before_message",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.before_message(&BeforeMessage {
                    content: &content,
                    sender: None,
                })
            }
        },
    );
    assert_eq!(server.requests().len(), 1);
    assert!(sink.requests.lock().unwrap().is_empty());
}

/// The serialized keys of one record, sorted: `to_value` holds them in
/// a map, so declaration order is pinned by the struct, not this helper.
fn wire_keys(record: &ProviderRequest) -> Vec<String> {
    let value = serde_json::to_value(record).unwrap();
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

#[test]
fn request_lines_carry_no_url_or_content() {
    let server = fakes::ProviderServer::start_routed(
        [("/models", fakes::Response::status(200, "{}"))],
        fakes::Response::status(500, "unrouted"),
    )
    .unwrap();
    let port = server.url().rsplit(':').next().unwrap().to_owned();
    // A query string, as a key in the query would: sent, never recorded.
    let query_url = format!("{}/models?token=SECRET-QUERY", server.url());
    let query_dir = ext_dir(None, &[("p", &models_lua(&query_url))]);
    let (sink, _) = Watching::new();
    let query_ext = observed_ext(&query_dir, FakeClock::new(), &sink);
    fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = LuaProvider::new(query_ext, "p");
            move || provider.list_models(&ModelsArg::default())
        },
    )
    .expect("the query-string request succeeds");
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].path.contains("SECRET-QUERY"),
        "the query was sent: {}",
        requests[0].path
    );
    // A userinfo-bearing loopback URL ureq accepts: sent, never recorded.
    let user_url = format!("http://USERINFO-SENTINEL:pass@127.0.0.1:{port}/models");
    let user_dir = ext_dir(None, &[("p", &models_lua(&user_url))]);
    let user_ext = observed_ext(&user_dir, FakeClock::new(), &sink);
    fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = LuaProvider::new(user_ext, "p");
            move || provider.list_models(&ModelsArg::default())
        },
    )
    .expect("the userinfo request succeeds");
    assert_eq!(server.requests().len(), 2);
    // The request names no resolver (IP literal, refused port): the
    // request fails, and the failure line still records no host.
    let dns_dir = ext_dir(
        None,
        &[("p", &models_lua("http://127.0.0.1:1/secret-host"))],
    );
    let dns_ext = observed_ext(&dns_dir, FakeClock::new(), &sink);
    let dns_err = fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = LuaProvider::new(dns_ext, "p");
            move || provider.list_models(&ModelsArg::default())
        },
    )
    .unwrap_err();
    assert_eq!(dns_err.code(), contract::ErrorCode::ExtensionFailed);
    let records = sink.records();
    assert_eq!(records.len(), 3);
    for record in &records[..2] {
        assert_eq!(
            wire_keys(record),
            [
                "attempt",
                "headers_ms",
                "provider",
                "purpose",
                "request_bytes",
                "response_bytes",
                "status",
                "total_ms",
            ]
        );
    }
    assert_eq!(
        wire_keys(&records[2]),
        [
            "attempt",
            "provider",
            "purpose",
            "request_bytes",
            "response_bytes",
            "total_ms",
        ]
    );
    // The exact key sets above already prove no `host`, `path`, `model`
    // or `first_token_ms` key, present or null. The sent values below
    // prove no URL content reaches the bytes either.
    for record in &records {
        let bytes = serde_json::to_vec(record).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        for sentinel in ["SECRET-QUERY", "USERINFO-SENTINEL", "secret-host"] {
            assert!(!text.contains(sentinel), "no {sentinel} in {text}");
        }
    }
}

/// A sink that records no debug lines: any record is a failure.
struct Dark;

impl DebugLog for Dark {
    fn debugging(&self) -> bool {
        false
    }

    fn provider_request(&self, _request: &ProviderRequest) {
        panic!("an info sink records nothing");
    }
}

#[test]
fn without_a_sink_or_below_debug_nothing_records() {
    let server =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let url = server.url();
    // No sink installed: the request succeeds and records nowhere.
    let plain_dir = ext_dir(None, &[("p", &models_lua(&url))]);
    let plain_ext = Arc::new(extensions::LuaExtension::new(
        "ext",
        plain_dir.path(),
        plain_dir.path(),
        FakeClock::new(),
    ));
    fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = LuaProvider::new(plain_ext, "p");
            move || provider.list_models(&ModelsArg::default())
        },
    )
    .expect("a request without a sink succeeds");
    assert_eq!(server.requests().len(), 1);
    // A sink that records no debug lines: the request succeeds and the
    // sink stays silent.
    let dark_dir = ext_dir(None, &[("p", &models_lua(&url))]);
    let dark_ext = Arc::new(
        extensions::LuaExtension::new("ext", dark_dir.path(), dark_dir.path(), FakeClock::new())
            .with_diagnostics(Arc::new(Dark)),
    );
    fakes::within(
        "provider.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let provider = LuaProvider::new(dark_ext, "p");
            move || provider.list_models(&ModelsArg::default())
        },
    )
    .expect("a request below debug succeeds");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn background_refresh_is_observed() {
    let server =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let url = server.url();
    let dir = ext_dir(None, &[("p", &models_lua(&url))]);
    let (sink, _) = Watching::new();
    let ext = observed_ext(&dir, FakeClock::new(), &sink);
    let provider = LuaProvider::new(ext, "p");
    let handle = provider
        .refresh(None, ModelsArg::default())
        .expect("a stale list refreshes");
    fakes::within(
        "handle.join",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || handle.join(),
    )
    .expect("the refresh thread joins")
    .expect("the background refresh succeeds");
    assert_eq!(server.requests().len(), 1);
    let records = sink.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].provider, "p");
    assert_eq!(
        serde_json::to_value(records[0].purpose).unwrap(),
        serde_json::Value::String("model_list".to_owned())
    );
}

#[test]
fn overlapping_callbacks_keep_their_own_attribution() {
    let held_a = fakes::ProviderServer::start([fakes::Response::status(200, "{}")]).unwrap();
    held_a.hold_from(1, Duration::from_secs(30));
    let held_p = fakes::ProviderServer::start([
        fakes::Response::status(200, "{}"),
        fakes::Response::status(200, "{}"),
    ])
    .unwrap();
    held_p.hold_from(1, Duration::from_secs(30));
    let open_q = fakes::ProviderServer::start_responding(|_| {
        fakes::Response::status(200, "{\"windows\":[]}")
    })
    .unwrap();
    let open_r =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let (url_a, url_p, url_q, url_r) = (held_a.url(), held_p.url(), open_q.url(), open_r.url());
    let dir_a = ext_dir(
        Some(&format!(
            "fiber.command(\"a\", {{ timeout = 60000, description = \"a\",\n\
               run = function(text) host.http({{ url = \"{url_a}/command\" }}) return \"a\" end }})\n\
             fiber.command(\"b\", {{ timeout = 60000, description = \"b\",\n\
               run = function(text) host.http({{ url = \"{url_a}/command\" }}) return \"b\" end }})\n"
        )),
        &[
            (
                "p",
                &format!(
                    "fiber.provider(\"p\", {{ models = {{ timeout = 60000, run = function()\n\
                       host.http({{ url = \"{url_p}/first\" }})\n\
                       host.http({{ url = \"{url_p}/second\" }})\n\
                       return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{url_p}\" }}}}\n\
                     end }} }})\n"
                ),
            ),
            (
                "q",
                &format!(
                    "fiber.provider(\"q\", {{ quota = {{ timeout = 60000, run = function(seen)\n\
                       host.http({{ url = \"{url_q}/quota\" }})\n\
                       return {{ windows = {{ }} }}\n\
                     end }} }})\n"
                ),
            ),
        ],
    );
    let dir_b = ext_dir(None, &[("r", &models_named_lua("r", &url_r))]);
    let clock = FakeClock::new();
    let (sink_a, _) = Watching::new();
    let (sink_b, _) = Watching::new();
    let ext_a = observed_ext(&dir_a, clock, &sink_a);
    let ext_b = observed_ext(&dir_b, FakeClock::new(), &sink_b);
    // Command A suspends on its held request.
    let (cmd_tx, cmd_rx) = std::sync::mpsc::channel();
    {
        let ext_a = Arc::clone(&ext_a);
        std::thread::spawn(move || {
            let _sent = cmd_tx.send(ext_a.command("a", ""));
        });
    }
    assert!(
        held_a.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "command A suspended on HTTP"
    );
    // Provider p suspends on its first held request while A waits.
    let provider_p = LuaProvider::new(Arc::clone(&ext_a), "p");
    let (models_tx, models_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _sent = models_tx.send(provider_p.list_models(&ModelsArg::default()));
    });
    assert!(
        held_p.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "provider p suspended on HTTP"
    );
    // Quota q runs on the same VM and completes; the second VM overlaps
    // with its own writer and provider.
    let pair = CredentialPair {
        credential: "c".to_owned(),
        label: "l".to_owned(),
    };
    let provider_q = LuaProvider::new(Arc::clone(&ext_a), "q");
    let quota = fakes::within("quota", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
        let provider_q = Arc::clone(&provider_q);
        let pair = pair.clone();
        move || provider_q.quota(&pair, None)
    });
    assert!(matches!(quota, extensions::QuotaEntry::Reported(_)));
    let provider_r = LuaProvider::new(ext_b, "r");
    fakes::within(
        "provider_r.models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || provider_r.models(&ModelsArg::default()),
    )
    .expect("the second VM lists its models");
    // Release p's requests in order, then the command: p's callback issues
    // its second HTTP after its resume.
    held_p.release();
    assert!(
        held_p.await_requests(2, Deadline::after(MUST_SUCCEED_WITHIN)),
        "provider p resumed into its second HTTP"
    );
    held_a.release();
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&models_rx)
        .expect("waited for p")
        .expect("p lists its models");
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&cmd_rx)
        .expect("waited for the command")
        .expect("the command completes");
    // Each request carries its own provider and purpose: two attempt-1
    // model-list lines for p, one quota line for q, nothing for commands.
    // The second VM's writer holds only its provider's line.
    let mut own: Vec<(String, String, u32)> = sink_a
        .records()
        .iter()
        .map(|record| {
            (
                record.provider.clone(),
                serde_json::to_value(record.purpose)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
                record.attempt,
            )
        })
        .collect();
    own.sort();
    assert_eq!(
        own,
        [
            ("p".to_owned(), "model_list".to_owned(), 1),
            ("p".to_owned(), "model_list".to_owned(), 1),
            ("q".to_owned(), "quota".to_owned(), 1),
        ]
    );
    let other: Vec<(String, String)> = sink_b
        .records()
        .iter()
        .map(|record| {
            (
                record.provider.clone(),
                serde_json::to_value(record.purpose)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect();
    assert_eq!(other, [("r".to_owned(), "model_list".to_owned())]);
    // A following command stays unobserved: no stale context leaks
    // between callbacks.
    fakes::within(
        "ext_a.command",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let ext_a = Arc::clone(&ext_a);
            move || ext_a.command("b", "")
        },
    )
    .expect("the following command succeeds");
    assert_eq!(sink_a.records().len(), 3);
}

// Session composition: discovery, background refresh and late VMs carry
// the installed sink.

fn diag_init() -> String {
    "fiber.hook(\"before_message\", { timeout = 60000, on_failure = \"non-blocking\",\n\
       run = function(msg) return { context = \"d\" } end })\n"
        .to_owned()
}

fn models_file(url: &str) -> String {
    format!(
        "fiber.provider(\"p\", {{ models = {{ timeout = 60000, run = function()\n\
           local reply = host.http({{ url = \"{url}/models\" }})\n\
           return {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{url}\" }}}}\n\
         end }} }})\n"
    )
}

/// Installs the hook+provider extension `diag` and the provider-only
/// extension `solo`, then loads the session. Provider files land after
/// install; manifests declare them upfront.
fn install_session(setup: &Setup, url: &str, models: &str) -> (PathBuf, PathBuf) {
    let diag = install_lua(
        setup,
        "diag",
        &json!({"providers": {"p": [], "q": []}}),
        &diag_init(),
    );
    std::fs::create_dir_all(diag.join("providers")).unwrap();
    std::fs::write(diag.join("providers/p.lua"), models).unwrap();
    std::fs::write(
        diag.join("providers/q.lua"),
        format!(
            "fiber.provider(\"q\", {{ quota = {{ timeout = 60000, run = function(seen)\n\
               local reply = host.http({{ url = \"{url}/quota\" }})\n\
               return {{ windows = {{ {{ name = \"w\", used_percent = 1 }} }} }}\n\
             end }} }})\n"
        ),
    )
    .unwrap();
    let solo = install_lua(
        setup,
        "solo",
        &json!({"providers": {"s": [], "t": [], "u": [], "v": []}}),
        "",
    );
    std::fs::remove_file(solo.join("init.lua")).unwrap();
    std::fs::create_dir_all(solo.join("providers")).unwrap();
    (diag, solo)
}

fn session_config(setup: &Setup) -> Config {
    Config::load(Sources {
        home: setup.home(),
        workspace: setup.workspace(),
        project: ProjectKey::new("p").unwrap(),
        overrides: Vec::new(),
    })
    .unwrap()
}

fn diagnosed_session(
    setup: &Setup,
    clock: Arc<FakeClock>,
    sink: &Arc<Watching>,
) -> Arc<SessionExtensions> {
    let session = load(setup, clock);
    let (timing, _) = recording();
    Arc::new(
        Arc::try_unwrap(session)
            .unwrap_or_else(|_| panic!("the session is still shared"))
            .with_diagnostics(timing, sink.clone() as Arc<dyn DebugLog>),
    )
}

fn provider_named(session: &Arc<SessionExtensions>, provider: &str) -> Arc<LuaProvider> {
    session
        .lua_providers()
        .iter()
        .find(|(_, lua)| lua.name() == provider)
        .map(|(_, lua)| Arc::clone(lua))
        .unwrap_or_else(|| panic!("provider {provider} loaded"))
}

#[test]
fn session_discovery_and_refresh_carry_the_sink() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start_responding(|request| match request.path.as_str() {
        "/models" => fakes::Response::status(200, "{}"),
        _ => fakes::Response::status(500, "unexpected"),
    })
    .unwrap();
    server.hold_from(2, Duration::from_secs(30));
    let url = server.url();
    install_session(&setup, &url, &models_file(&url));
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let session = diagnosed_session(&setup, clock, &sink);
    // Uncached discovery is observed on its first call.
    let models = provider_named(&session, "p");
    fakes::within(
        "models.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let models = Arc::clone(&models);
            move || models.list_models(&ModelsArg::default())
        },
    )
    .expect("discovery succeeds");
    // A stale-list refresh runs detached: its request is received while
    // held, and its completion is collected through its JoinHandle.
    let refreshed = models
        .refresh(None, ModelsArg::default())
        .expect("a stale list refreshes");
    assert!(
        server.await_requests(2, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the refresh reached its request"
    );
    server.release();
    fakes::within(
        "refreshed.join",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || refreshed.join(),
    )
    .expect("the refresh thread joins")
    .expect("the refresh succeeds");
    // The cache serves from here: no HTTP, no record.
    fakes::within(
        "models.models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let models = Arc::clone(&models);
            move || models.models(&ModelsArg::default())
        },
    )
    .expect("the cached list serves");
    assert_eq!(server.requests().len(), 2);
    let purposes: Vec<String> = sink
        .records()
        .iter()
        .map(|record| {
            serde_json::to_value(record.purpose)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(purposes, ["model_list", "model_list"]);
}

#[test]
fn late_fresh_and_reused_vms_carry_the_sink() {
    let setup = Setup::new();
    let server = fakes::ProviderServer::start_responding(|request| match request.path.as_str() {
        "/models" => fakes::Response::status(200, "{}"),
        "/quota" => fakes::Response::status(200, "{\"windows\":[]}"),
        "/token" => fakes::Response::status(200, "{\"token\":\"t\",\"expires_at\":1893456000}"),
        "/cost" => fakes::Response::status(200, "0.5"),
        _ => fakes::Response::status(500, "unexpected"),
    })
    .unwrap();
    let url = server.url();
    let (diag, solo) = install_session(&setup, &url, &models_file(&url));
    for (name, body) in [
        (
            "s",
            format!(
                "host.http({{ url = \"{url}/models\" }})\nreturn {{{{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{url}\" }}}}"
            ),
        ),
        (
            "t",
            format!(
                "host.http({{ url = \"{url}/quota\" }})\nreturn {{ windows = {{ {{ name = \"w\", used_percent = 1 }} }} }}"
            ),
        ),
        (
            "u",
            format!(
                "host.http({{ method = \"POST\", url = \"{url}/token\" }})\nreturn {{ token = \"t\", expires_at = 1893456000 }}"
            ),
        ),
        (
            "v",
            format!("host.http({{ url = \"{url}/cost\" }})\nreturn 0.5"),
        ),
    ] {
        let function = match name {
            "s" => "models",
            "t" => "quota",
            "u" => "credential",
            "v" => "cost",
            _ => unreachable!(),
        };
        std::fs::write(
            solo.join("providers").join(format!("{name}.lua")),
            format!("fiber.provider(\"{name}\", {{ {function} = {{ timeout = 60000, run = function(seen)\n{body}\nend }} }})\n"),
        )
        .unwrap();
    }
    let _ = diag;
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let session = diagnosed_session(&setup, clock.clone(), &sink);
    // Hold the hook+provider extension's handles; the provider-only
    // extension's handles die with the retain below.
    let held_p = provider_named(&session, "p");
    let mut session = Arc::try_unwrap(session).unwrap_or_else(|_| panic!("shared"));
    session.retain_lua_providers(&[]);
    let session = Arc::new(session);
    assert!(session.lua_providers().is_empty());
    let config = session_config(&setup);
    let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
    // Deferred: the expired provider-only VM starts fresh, and its first
    // credential, models, quota and cost calls all observe through the
    // retained sink. Nothing else could serve them: the weak handles died
    // with the retain, so each fresh start below is a new VM.
    for (provider, purpose) in [
        ("s", "model_list"),
        ("t", "quota"),
        ("u", "token_refresh"),
        ("v", "cost"),
    ] {
        let started = fakes::within(
            "start_provider",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            {
                let session = Arc::clone(&session);
                let config = config.clone();
                let clock = clock.clone();
                let locks = Arc::clone(&locks);
                let provider = provider.to_owned();
                move || session.start_provider(&config, clock, locks, "fiber.test/solo", &provider)
            },
        )
        .unwrap_or_else(|_| panic!("late start of {provider}"));
        assert_eq!(started.name(), provider);
        match purpose {
            "model_list" => {
                fakes::within(
                    "list_models",
                    Deadline::after(fakes::MUST_SUCCEED_WITHIN),
                    {
                        let started = Arc::clone(&started);
                        move || started.list_models(&ModelsArg::default())
                    },
                )
                .expect("fresh models() succeeds");
            }
            "quota" => {
                let pair = CredentialPair {
                    credential: "c".to_owned(),
                    label: "l".to_owned(),
                };
                let quota = fakes::within("quota", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
                    let started = Arc::clone(&started);
                    move || started.quota(&pair, None)
                });
                assert!(matches!(quota, extensions::QuotaEntry::Reported(_)));
            }
            "token_refresh" => {
                let pair = CredentialPair {
                    credential: "c".to_owned(),
                    label: "l".to_owned(),
                };
                fakes::within(
                    "credential_token",
                    Deadline::after(fakes::MUST_SUCCEED_WITHIN),
                    {
                        let started = Arc::clone(&started);
                        move || started.credential_token(&pair)
                    },
                )
                .expect("fresh credential() succeeds");
            }
            "cost" => {
                let cost = fakes::within("cost", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
                    let started = Arc::clone(&started);
                    let url = url.clone();
                    move || started.cost(&GenerationId("g1".into()), &url, None)
                })
                .expect("fresh cost() succeeds");
                assert_eq!(cost, Some(0.5));
            }
            _ => unreachable!(),
        }
    }
    // Reused: the hook-held extension lends its running VM, and two
    // providers on it keep their distinct attribution.
    let reused = fakes::within(
        "start_provider",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            let config = config.clone();
            let clock = clock.clone();
            let locks = Arc::clone(&locks);
            move || session.start_provider(&config, clock, locks, "fiber.test/diag", "p")
        },
    )
    .expect("the held VM restarts its provider");
    fakes::within(
        "reused.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let reused = Arc::clone(&reused);
            move || reused.list_models(&ModelsArg::default())
        },
    )
    .expect("reused models() succeeds");
    let pair = CredentialPair {
        credential: "c".to_owned(),
        label: "l".to_owned(),
    };
    let quota = fakes::within(
        "start_provider",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            let config = config.clone();
            let clock = clock.clone();
            let locks = Arc::clone(&locks);
            let pair = pair.clone();
            move || {
                session
                    .start_provider(&config, clock, locks, "fiber.test/diag", "q")
                    .map(|started| started.quota(&pair, None))
            }
        },
    )
    .expect("the held VM restarts its second provider");

    assert!(matches!(quota, extensions::QuotaEntry::Reported(_)));
    // The pre-retain handles never mutated: the session, its hooks and
    // its providers all still work.
    fakes::within(
        "held_p.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let held_p = Arc::clone(&held_p);
            move || held_p.list_models(&ModelsArg::default())
        },
    )
    .expect("the held provider still lists");
    let content = vec![ContentPart::Text { text: "hi".into() }];
    let answer = fakes::within(
        "session.before_message",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let session = Arc::clone(&session);
            move || {
                session.before_message(&BeforeMessage {
                    content: &content,
                    sender: None,
                })
            }
        },
    );
    match answer.outcome {
        BeforeMessageOutcome::Passed { context, .. } => assert_eq!(context.len(), 1),
        BeforeMessageOutcome::Refused(refusal) => {
            panic!("the hooks survived the retain: {refusal:?}")
        }
        BeforeMessageOutcome::Failed(failure) => {
            panic!("the hooks survived the retain: {failure:?}")
        }
    }
    let purposes: Vec<(String, String)> = sink
        .records()
        .iter()
        .map(|record| {
            (
                record.provider.clone(),
                serde_json::to_value(record.purpose)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
            )
        })
        .collect();
    assert_eq!(
        purposes,
        [
            ("s".to_owned(), "model_list".to_owned()),
            ("t".to_owned(), "quota".to_owned()),
            ("u".to_owned(), "token_refresh".to_owned()),
            ("v".to_owned(), "cost".to_owned()),
            ("p".to_owned(), "model_list".to_owned()),
            ("q".to_owned(), "quota".to_owned()),
            // The held handle's first models() discovers: list_models
            // never writes the cache, so nothing cached it, and the late
            // discovery observes like any other.
            ("p".to_owned(), "model_list".to_owned()),
        ]
    );
}

#[test]
fn failed_provider_loads_record_nothing_and_silent_loads_stay_silent() {
    let setup = Setup::new();
    let server =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let url = server.url();
    install_session(&setup, &url, &models_file(&url));
    // An extension file its manifest never declares.
    let undecl = install_lua(&setup, "undecl", &json!({}), "");
    std::fs::remove_file(undecl.join("init.lua")).unwrap();
    std::fs::create_dir_all(undecl.join("providers")).unwrap();
    std::fs::write(
        undecl.join("providers/u.lua"),
        "fiber.provider(\"u\", {})\n",
    )
    .unwrap();
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let session = diagnosed_session(&setup, clock.clone(), &sink);
    let config = session_config(&setup);
    let locks: Arc<dyn contract::files::PathLock> = Arc::new(NoLock);
    // A missing provider and an undeclared one fail the same way with or
    // without a sink, inventing no request record.
    for (extension, provider) in [("fiber.test/diag", "missing"), ("fiber.test/undecl", "u")] {
        let answer = fakes::within(
            "start_provider",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            {
                let session = Arc::clone(&session);
                let config = config.clone();
                let clock = clock.clone();
                let locks = Arc::clone(&locks);
                let extension = extension.to_owned();
                let provider = provider.to_owned();
                move || session.start_provider(&config, clock, locks, &extension, &provider)
            },
        );
        let err = match answer {
            Ok(_) => panic!("{extension}/{provider} loads, but must not"),
            Err(err) => err,
        };
        assert!(
            matches!(
                err,
                extensions::Error::Lua { .. } | extensions::Error::UndeclaredProvider { .. }
            ),
            "a failed load keeps its error: {err:?}"
        );
    }
    assert!(sink.records().is_empty());
    assert!(server.requests().is_empty());
    // Without an installed sink the same work succeeds silently: the
    // request is received and no diagnostic exists to record it.
    let plain = load(&setup, FakeClock::new());
    let models = provider_named(&plain, "p");
    fakes::within(
        "models.list_models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let models = Arc::clone(&models);
            move || models.list_models(&ModelsArg::default())
        },
    )
    .expect("silent discovery succeeds");
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn failures_retain_the_cached_list_without_observation() {
    let setup = Setup::new();
    let server =
        fakes::ProviderServer::start_responding(|_| fakes::Response::status(200, "{}")).unwrap();
    let url = server.url();
    let (diag, _) = install_session(&setup, &url, &models_file(&url));
    let (sink1, _) = Watching::new();
    let first = diagnosed_session(&setup, FakeClock::new(), &sink1);
    let before = fakes::within(
        "models.models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let models = provider_named(&first, "p");
            move || models.models(&ModelsArg::default())
        },
    )
    .expect("discovery writes the cache");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(sink1.records().len(), 1);
    drop(first);
    // The provider file breaks after the cache landed: the next session
    // serves the retained list with no HTTP and no record.
    std::fs::write(
        diag.join("providers/p.lua"),
        "fiber.provider(\"p\", { models = { timeout = 60000, run = function()\n\
           error(\"the list is gone\")\n\
         end } }})\n",
    )
    .unwrap();
    let (sink2, _) = Watching::new();
    let second = diagnosed_session(&setup, FakeClock::new(), &sink2);
    let after = fakes::within(
        "models.models",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        {
            let models = provider_named(&second, "p");
            move || models.models(&ModelsArg::default())
        },
    )
    .expect("the retained list serves");
    assert_eq!(
        after.iter().map(|model| &model.id).collect::<Vec<_>>(),
        before.iter().map(|model| &model.id).collect::<Vec<_>>()
    );
    assert_eq!(server.requests().len(), 1);
    assert!(sink2.records().is_empty());
}
