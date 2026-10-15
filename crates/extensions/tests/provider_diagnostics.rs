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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
use contract::hook::{
    AfterToolCall, AfterToolOutcome, BeforeMessage, BeforeMessageOutcome, BeforeModelCall,
    BeforeToolCall, CheckOutcome, ContextOutcome, Hooks, SessionStart, StartReason, TurnEnd,
    TurnStart,
};
use contract::shapes::{ContentPart, DeclaredEffects, Tokens, Usage};
use contract::tool::Cancel;
use extensions::SessionExtensions;
use fakes::Deadline;
use fakes::clock::FakeClock;
use serde_json::{Map, json};

/// How long a test waits for one dispatch or signal before failing.
const WAIT: Duration = Duration::from_secs(5);

/// The session's per-path lock: this test never takes it, so it runs every
/// call straight through.
struct NoLock;

impl contract::files::PathLock for NoLock {
    fn hold(&self, _path: &Path, run: &mut dyn FnMut()) {
        run();
    }

    fn hold_all(&self, _paths: &[PathBuf], run: &mut dyn FnMut()) {
        run();
    }
}

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

/// Runs `f` on its own thread and waits under [`WAIT`], so a dispatch that
/// never returns fails the test instead of hanging it.
#[track_caller]
fn within<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _sent = tx.send(f());
    });
    Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the dispatch")
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
        Deadline::after(WAIT)
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
    ]
    .join("\n")
}

fn dispatch_all(session: &Arc<SessionExtensions>) {
    let id = session_id();
    let used = usage();
    let content = vec![ContentPart::Text { text: "hi".into() }];
    within({
        let session = Arc::clone(session);
        move || {
            session.session_start(&SessionStart {
                reason: StartReason::Start,
                session_id: &id,
                workspace: "/w",
                cancel: &Never,
            })
        }
    });
    within({
        let session = Arc::clone(session);
        move || {
            session.turn_start(&TurnStart {
                input: &[],
                cancel: &Never,
            })
        }
    });
    within({
        let session = Arc::clone(session);
        move || {
            session.turn_end(&TurnEnd {
                reply: "done",
                cancel: &Never,
            })
        }
    });
    within({
        let session = Arc::clone(session);
        move || session.before_message(&BeforeMessage { content: &content })
    });
    within({
        let session = Arc::clone(session);
        move || {
            session.before_model_call(&BeforeModelCall {
                model: "p/m",
                usage: &used,
                cancel: &Never,
            })
        }
    });
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
        ]
    );
    // Two successive dispatches emit twice.
    let id = session_id();
    within({
        let session = Arc::clone(&session);
        move || {
            session.session_start(&SessionStart {
                reason: StartReason::Start,
                session_id: &id,
                workspace: "/w",
                cancel: &Never,
            })
        }
    });
    assert_eq!(ops(&calls).len(), 6);
    assert_eq!(ops(&calls)[5], (Op::Hook(HookChain::SessionStart), None));
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
    let answer = within({
        let session = Arc::clone(&session);
        move || session.before_message(&BeforeMessage { content: &content })
    });
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
    let refused = within({
        let session = Arc::clone(&session);
        move || session.before_message(&BeforeMessage { content: &content })
    });
    assert!(matches!(refused.outcome, BeforeMessageOutcome::Refused(_)));
    let used = usage();
    let failed = within({
        let session = Arc::clone(&session);
        move || {
            session.before_model_call(&BeforeModelCall {
                model: "p/m",
                usage: &used,
                cancel: &Never,
            })
        }
    });
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
    let refused = within({
        let session = Arc::clone(&session);
        move || {
            session.before_model_call(&BeforeModelCall {
                model: "p/m",
                usage: &used,
                cancel: &Never,
            })
        }
    });
    assert!(matches!(refused.outcome, CheckOutcome::Refused(_)));
    let content = vec![ContentPart::Text { text: "hi".into() }];
    let passed = within({
        let session = Arc::clone(&session);
        move || session.before_message(&BeforeMessage { content: &content })
    });
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
        server.await_requests(1, WAIT),
        "the first hook suspended on HTTP"
    );
    cancel.fire();
    server.release();
    let answer = Deadline::after(WAIT)
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
    };
    let arguments = Map::new();
    let rewritten = within({
        let session = Arc::clone(&session);
        move || {
            session.before_tool(&BeforeToolCall {
                tool: "read",
                arguments: &arguments,
                declared: &declared,
            })
        }
    });
    assert!(matches!(
        rewritten.outcome,
        contract::hook::BeforeToolOutcome::Rewritten { .. }
    ));
    let changed = within({
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
            })
        }
    });
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
            let answer = session.before_message(&BeforeMessage { content: &content });
            let _sent = tx.send(answer);
        })
    };
    let id = Deadline::after(WAIT)
        .recv(&id_rx)
        .expect("waited for the dispatch thread");
    assert!(server.await_requests(1, WAIT), "the hook suspended on HTTP");
    // The dispatch waits on the fake clock past its own check before fake
    // time moves.
    // The dispatch waits on the fake clock past its own check before fake
    // time moves: once its callback suspends, the caller waits until the
    // callback's deadline plus the scheduler's grace for a suspended call
    // (`docs/extensions.md`, "How an extension runs"), a stable park the
    // transient deadline wait ends in before the HTTP receipt returns.
    assert!(
        clock.await_thread_parked(id, Some(mark + Duration::from_secs(31)), WAIT),
        "the hook caller parked past its check on the suspended callback"
    );
    clock.advance(Duration::from_millis(2500));
    assert!(
        ends.lock().unwrap().is_empty(),
        "no completion while the dispatch is suspended"
    );
    server.release();
    let answer = Deadline::after(WAIT)
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
            let answer = session.before_message(&BeforeMessage { content: &content });
            let _sent = tx.send(answer);
        })
    };
    let id = Deadline::after(WAIT)
        .recv(&id_rx)
        .expect("waited for the dispatch thread");
    assert!(server.await_requests(1, WAIT), "the hook suspended on HTTP");
    // The caller waits on the suspended callback past its own check: the
    // hook's deadline plus the scheduler's grace for a suspended call.
    assert!(
        clock.await_thread_parked(id, Some(mark + Duration::from_millis(2000)), WAIT),
        "the hook caller parked past its check on the suspended callback"
    );
    // Past the hook's deadline but inside its grace: the caller takes the
    // timeout, the chain fails, and the scope still closes exactly once.
    clock.advance(Duration::from_millis(1500));
    let answer = Deadline::after(WAIT)
        .recv(&rx)
        .expect("waited for the timed-out dispatch");
    assert!(matches!(answer.outcome, BeforeMessageOutcome::Failed(_)));
    assert_eq!(ops(&calls), [(Op::Hook(HookChain::BeforeMessage), None)]);
    // The late reply is discarded: releasing the hold ends the worker
    // without hanging the suite.
    server.release();
    worker.join().expect("the dispatch thread joins");
}
