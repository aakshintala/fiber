//! Session diagnostics attachment and provider HTTP observation: the
//! one-time sink on each VM, the hook-chain timer scopes and one observed
//! line per eligible Lua-provider request (`docs/state.md`, "Diagnostic
//! logs"). Same-crate tests for what no public API exposes: which VM
//! holds which sink, the purpose mapping, the milestone arithmetic and
//! the admission and worker ordering the public API cannot freeze.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]
#![allow(clippy::indexing_slicing, reason = "test code")]

use fakes::MUST_SUCCEED_WITHIN;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime};

use config::{Config, ProjectKey, Sources};
use contract::clock::Clock;
use contract::diag::HttpRequest as ObservedRequest;
use contract::diag::{DebugLog, Identity, ProviderPurpose, Purpose, Timing};
use fakes::clock::FakeClock;

use super::SessionExtensions;
use crate::{LuaExtension, ModelsArg, Origin, Request, plan};
use fakes::FakeLock;

/// A sink that keeps every request it is given.
#[derive(Default)]
struct Sink {
    requests: Mutex<Vec<ObservedRequest>>,
}

impl Sink {
    fn named() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

impl DebugLog for Sink {
    fn debugging(&self) -> bool {
        true
    }

    fn http_request(&self, request: &ObservedRequest) {
        self.requests.lock().unwrap().push(request.clone());
    }
}

/// A timing that records each invocation and returns a token guard.
fn recording() -> (Timing, Arc<Mutex<Vec<contract::diag::Op>>>) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&calls);
    let timing: Timing = Arc::new(move |op, _tool, _started| {
        seen.lock().unwrap().push(op);
        Some(Box::new(()) as Box<dyn Send>)
    });
    (timing, calls)
}

struct Home {
    root: fakes::TempDir,
}

impl Home {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-diagnostics-attach");
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("workspace")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    /// Installs `fiber.test/<short>`: `init.lua` when `init` is `Some`, a
    /// provider-only extension otherwise. The manifest declares every
    /// provider file, with no URLs.
    fn install(&self, short: &str, init: Option<&str>, providers: &[(&str, &str)]) {
        let src = self.root.path().join("src").join(short);
        fs::create_dir_all(&src).unwrap();
        let mut manifest = serde_json::json!({"name": format!("fiber.test/{short}"), "version": "v1.2.3", "fiber": "0.1.0", "api": 1});
        if !providers.is_empty() {
            fs::create_dir_all(src.join("providers")).unwrap();
            let declared: serde_json::Map<String, serde_json::Value> = providers
                .iter()
                .map(|(name, lua)| {
                    fs::write(src.join("providers").join(format!("{name}.lua")), lua).unwrap();
                    ((*name).to_owned(), serde_json::json!([]))
                })
                .collect();
            manifest["providers"] = serde_json::Value::Object(declared);
        }
        fs::write(src.join("extension.json"), manifest.to_string()).unwrap();
        if let Some(init) = init {
            fs::write(src.join("init.lua"), init).unwrap();
        }
        plan(
            &self.home(),
            &Request::Path(src),
            "0.1.0",
            &Origin::github(),
            &*FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    fn load(&self, clock: Arc<FakeClock>) -> SessionExtensions {
        let config = Config::load(Sources {
            home: self.home(),
            workspace: self.root.path().join("workspace"),
            project: ProjectKey::new("p").unwrap(),
            overrides: Vec::new(),
        })
        .unwrap();
        let home = self.home();
        let locks: Arc<dyn contract::files::PathLock> = Arc::new(FakeLock::new());
        // On its own thread: the fake clock never ends a wait the runtime
        // does not end itself.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _sent = tx.send(SessionExtensions::load(&home, &config, clock, locks, None));
        });
        fakes::Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&rx)
            .expect("waited for the extensions to load")
    }
}

const HOOKS_INIT: &str = r#"
fiber.hook("before_message", { timeout = 30000, on_failure = "non-blocking",
  run = function(msg) return {} end })
"#;

const PROVIDER_ONLY: &str =
    "fiber.provider(\"p\", { models = { timeout = 30000, run = function() return {} end } })";

#[test]
fn hub_sink_first_installation_wins() {
    let clock = FakeClock::new();
    let hub = crate::lua::Hub::new(clock);
    let first = Sink::named();
    let second = Sink::named();
    hub.set_diagnostics(first.clone());
    hub.set_diagnostics(second.clone());
    let held = hub.diagnostics().expect("a sink was installed");
    assert!(
        Arc::ptr_eq(&held, &(first as Arc<dyn DebugLog>)),
        "the second installation must not redirect the VM"
    );
}

#[test]
fn lua_builder_installs_the_sink_without_starting_the_vm() {
    let dir = fakes::TempDir::new("fiber-diagnostics-builder");
    let extension = LuaExtension::new("ext", dir.path(), dir.path(), FakeClock::new());
    assert!(!extension.is_running());
    let sink = Sink::named();
    let extension = extension.with_diagnostics(sink.clone());
    assert!(!extension.is_running(), "attachment runs no callback");
    let held = extension
        .hub_diagnostics()
        .expect("the builder installed the sink");
    assert!(
        Arc::ptr_eq(&held, &(sink as Arc<dyn DebugLog>)),
        "the installed sink is the one given"
    );
}

#[test]
fn session_attach_reaches_hook_and_provider_only_vms() {
    let home = Home::new();
    home.install("hooks", Some(HOOKS_INIT), &[]);
    home.install("provider", None, &[("p", PROVIDER_ONLY)]);
    let clock = FakeClock::new();
    let session = home.load(clock);
    assert_eq!(session.attached_vms().len(), 2);
    for vm in session.attached_vms() {
        assert!(
            vm.hub_diagnostics().is_none(),
            "no sink before with_diagnostics"
        );
    }
    let provider_vm = session
        .attached_vms()
        .into_iter()
        .find(|vm| vm.name() == "fiber.test/provider")
        .expect("the provider-only VM is attached");
    assert!(
        !provider_vm.is_running(),
        "loading starts no provider callback"
    );
    let sink = Sink::named();
    let (timing, _) = recording();
    let session = session.with_diagnostics(timing, sink.clone());
    let vms = session.attached_vms();
    assert_eq!(vms.len(), 2, "hook and provider-only VMs");
    for vm in &vms {
        let held = vm.hub_diagnostics().expect("every live VM holds the sink");
        assert!(
            Arc::ptr_eq(&held, &(sink.clone() as Arc<dyn DebugLog>)),
            "the installed sink reaches {}",
            vm.name()
        );
    }
    assert!(
        !provider_vm.is_running(),
        "attachment runs no provider callback"
    );
    // A second installation never redirects a VM that already has one.
    let later = Sink::named();
    let (timing, _) = recording();
    let session = session.with_diagnostics(timing, later);
    for vm in session.attached_vms() {
        let held = vm.hub_diagnostics().expect("the sink stays installed");
        assert!(
            Arc::ptr_eq(&held, &(sink.clone() as Arc<dyn DebugLog>)),
            "the first sink still observes {}",
            vm.name()
        );
    }
}

#[test]
fn session_without_extensions_attaches_cleanly() {
    let home = Home::new();
    let clock = FakeClock::new();
    let session = home.load(clock);
    assert!(session.attached_vms().is_empty());
    let sink = Sink::named();
    let (timing, calls) = recording();
    let session = session.with_diagnostics(timing, sink);
    assert!(session.attached_vms().is_empty());
    assert!(calls.lock().unwrap().is_empty());
}

const TURN_START_INIT: &str = r#"
fiber.hook("turn_start", { timeout = 30000, on_failure = "non-blocking",
  run = function(ctx) return { context = "t" } end })
"#;

#[test]
fn dispatch_on_a_stopped_vm_keeps_its_scope() {
    use contract::hook::{ContextOutcome, Hooks, TurnStart};
    use contract::tool::Cancel;

    struct Never;
    impl Cancel for Never {
        fn is_cancelled(&self) -> bool {
            false
        }

        fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
    }

    let home = Home::new();
    home.install("hooks", Some(TURN_START_INIT), &[]);
    let clock = FakeClock::new();
    let session = home.load(clock);
    let sink = Sink::named();
    let (timing, calls) = recording();
    let session = session.with_diagnostics(timing, sink);
    assert!(session.attached_vms().len() == 1);
    for vm in session.attached_vms() {
        vm.dispose();
    }
    // A stopped VM answers at once, without waiting: the dispatch still
    // holds its one scope around the failure.
    let answer = session.turn_start(&TurnStart {
        input: &[],
        cancel: &Never,
    });
    match &answer.outcome {
        ContextOutcome::Added(context) => assert!(context.is_empty()),
        ContextOutcome::Failed(failure) => panic!("a stopped chain adds no context: {failure:?}"),
    }
    assert_eq!(answer.notices.len(), 1);
    assert_eq!(calls.lock().unwrap().len(), 1);
    assert_eq!(
        calls.lock().unwrap()[0],
        contract::diag::Op::Hook(contract::diag::HookChain::TurnStart)
    );
}

// Observed Lua HTTP requests: one bounded execution, provider purposes and
// the admission and worker ordering.

use std::sync::mpsc;

use contract::ErrorCode;
use fakes::{Deadline, ProviderServer, Response};

use crate::host::{
    HttpObservation, HttpRequest, add_bytes, byte_len, duration_ms, ms_since, perform,
    perform_observed,
};
use crate::lua::{
    CredentialFor, Pause, fail_next_spawn_for, pause_observed_execution, pause_settle,
    provider_purpose, unpause_observed_execution, unpause_settle,
};

/// A clock returning `stamps` in order, then repeating the last: controlled
/// milestones with no real time and no asynchronous advance.
struct Script {
    stamps: Mutex<VecDeque<Instant>>,
    wall: SystemTime,
}

impl Script {
    fn at(offsets_ms: &[u64]) -> Arc<Self> {
        let origin = FakeClock::new().origin();
        Arc::new(Self {
            stamps: Mutex::new(
                offsets_ms
                    .iter()
                    .map(|ms| origin + Duration::from_millis(*ms))
                    .collect(),
            ),
            wall: SystemTime::UNIX_EPOCH,
        })
    }
}

impl Clock for Script {
    fn now(&self) -> Instant {
        let mut stamps = self.stamps.lock().unwrap();
        if stamps.len() > 1 {
            stamps.pop_front().unwrap()
        } else {
            stamps[0]
        }
    }

    fn wall(&self) -> SystemTime {
        self.wall
    }

    fn sleep(&self, _d: Duration) {}

    fn wait_until(&self, _until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        wait(Some(Duration::ZERO));
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// A sink that counts its clock reads, delegating to a fake clock.
struct Counting {
    clock: Arc<FakeClock>,
    reads: AtomicU64,
}

impl Counting {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            clock: FakeClock::new(),
            reads: AtomicU64::new(0),
        })
    }

    fn reads(&self) -> u64 {
        self.reads.load(Ordering::SeqCst)
    }
}

impl Clock for Counting {
    fn now(&self) -> Instant {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.clock.now()
    }

    fn wall(&self) -> SystemTime {
        self.clock.wall()
    }

    fn sleep(&self, d: Duration) {
        self.clock.sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        self.clock.wait_until(until, wait);
    }

    fn subscribe(&self, waker: std::sync::Weak<dyn contract::clock::Wake>) {
        self.clock.subscribe(waker);
    }
}

/// A sink that signals each record it is given.
struct Watching {
    requests: Mutex<Vec<ObservedRequest>>,
    arrived: mpsc::Sender<ObservedRequest>,
}

impl Watching {
    fn new() -> (Arc<Self>, mpsc::Receiver<ObservedRequest>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(Self {
                requests: Mutex::new(Vec::new()),
                arrived: tx,
            }),
            rx,
        )
    }

    fn records(&self) -> Vec<ObservedRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl DebugLog for Watching {
    fn debugging(&self) -> bool {
        true
    }

    fn http_request(&self, request: &ObservedRequest) {
        self.requests.lock().unwrap().push(request.clone());
        let _ignored = self.arrived.send(request.clone());
    }
}

/// A sink that records no debug lines: `http_request` must never run.
struct Dark;

impl DebugLog for Dark {
    fn debugging(&self) -> bool {
        false
    }

    fn http_request(&self, _request: &ObservedRequest) {
        panic!("an info sink records nothing");
    }
}

fn get(url: &str, timeout: Option<Duration>) -> HttpRequest {
    HttpRequest {
        method: "GET".to_owned(),
        url: url.to_owned(),
        headers: Vec::new(),
        body: None,
        timeout,
    }
}

fn observed(
    provider: &str,
    purpose: ProviderPurpose,
    debug: Arc<dyn DebugLog>,
    clock: Arc<dyn Clock>,
) -> HttpObservation {
    HttpObservation {
        attribution: crate::host::HttpAttribution::Provider {
            provider: provider.to_owned(),
            purpose,
        },
        debug,
        clock,
    }
}

#[test]
fn observed_and_plain_agree_on_every_outcome() {
    let server = ProviderServer::start_responding(|request| {
        if request.path == "/missing" {
            Response::status(503, "back off")
        } else if request.path == "/binary" {
            Response::status(200, vec![0xff, 0xfe, 0x00, b'x'])
        } else if request.method == "POST" {
            Response::status(429, "slow down")
        } else {
            Response::status(200, "{}")
        }
    })
    .unwrap();
    let url = server.url();
    let post = |path: &str| HttpRequest {
        method: "POST".to_owned(),
        url: format!("{url}{path}"),
        headers: vec![("content-type".to_owned(), "application/json".to_owned())],
        body: Some(br#"{"a":1}"#.to_vec()),
        timeout: None,
    };
    for (request, status, request_bytes, response_bytes) in [
        (get(&format!("{url}/get"), None), 200, 0, 2),
        (post("/post"), 429, 7, 9),
        (get(&format!("{url}/binary"), None), 200, 0, 4),
        (
            HttpRequest {
                method: "POST".to_owned(),
                url: format!("{url}/missing"),
                headers: Vec::new(),
                body: None,
                timeout: None,
            },
            503,
            0,
            8,
        ),
    ] {
        let clock = FakeClock::new();
        let (sink, _) = Watching::new();
        let plain = perform(&request);
        let seen = perform_observed(
            &request,
            observed("p", ProviderPurpose::ModelList, sink.clone(), clock),
        );
        // A status other than 2xx is a reply, not an error: the exact
        // outcome matches on both paths.
        assert_eq!(plain, seen);
        let (status_got, body) = seen.expect("the scripted reply succeeds");
        assert_eq!(status_got, status);
        assert_eq!(body.len() as u64, response_bytes);
        let records = sink.records();
        // docs/state.md, "Diagnostic logs": one line when the request ends, with the provider's purpose.
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.identity(), Some(&Identity::Provider("p".to_owned())));
        assert_eq!(record.purpose(), Purpose::ModelList);
        assert_eq!(record.status, Some(status));
        assert_eq!(record.attempt, 1);
        assert_eq!(record.request_bytes, request_bytes);
        assert_eq!(record.response_bytes, response_bytes);
        assert_eq!(record.headers_ms, Some(0));
        assert_eq!(record.total_ms, 0);
    }
    // A refused connection fails the same on both paths, with no status,
    // no headers and zero response bytes.
    let refused = get("http://127.0.0.1:1/", None);
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let plain = perform(&refused).unwrap_err();
    let seen = perform_observed(
        &refused,
        observed("p", ProviderPurpose::Quota, sink.clone(), clock),
    )
    .unwrap_err();
    assert_eq!(plain, seen);
    assert_eq!(plain.0, ErrorCode::ConnectionFailed);
    let records = sink.records();
    // docs/state.md, "Diagnostic logs": one that fails after the send writes one without status or headers_ms and with zero response bytes.
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, None);
    assert_eq!(records[0].headers_ms, None);
    assert_eq!(records[0].response_bytes, 0);
}

#[test]
fn pre_send_failures_emit_nothing() {
    let server = ProviderServer::start_responding(|_| Response::status(200, "{}")).unwrap();
    let url = server.url();
    for request in [
        HttpRequest {
            method: "NOT A METHOD".to_owned(),
            url: format!("{url}/x"),
            headers: Vec::new(),
            body: None,
            timeout: None,
        },
        HttpRequest {
            method: "GET".to_owned(),
            url: "://missing-scheme".to_owned(),
            headers: Vec::new(),
            body: None,
            timeout: None,
        },
        HttpRequest {
            method: "GET".to_owned(),
            url: "http://[::1".to_owned(),
            headers: Vec::new(),
            body: None,
            timeout: None,
        },
        HttpRequest {
            method: "GET".to_owned(),
            url: format!("{url}/x"),
            headers: vec![("bad header".to_owned(), "v".to_owned())],
            body: None,
            timeout: None,
        },
    ] {
        let clock = FakeClock::new();
        let (sink, _) = Watching::new();
        let plain = perform(&request).unwrap_err();
        let seen = perform_observed(
            &request,
            observed("p", ProviderPurpose::Cost, sink.clone(), clock),
        )
        .unwrap_err();
        // The error's code and message match, not only its failure.
        assert_eq!(plain, seen);
        assert_eq!(plain.0, ErrorCode::ConnectionFailed);
        assert!(plain.1.starts_with("host.http: "), "{}", plain.1);
        assert!(sink.records().is_empty());
    }
    assert!(
        server.requests().is_empty(),
        "a request never built is never sent"
    );
}

#[test]
fn milestones_come_from_the_send() {
    let server = ProviderServer::start_responding(|_| Response::status(200, "{}")).unwrap();
    let url = server.url();
    for (stamps, headers_ms, total_ms) in [
        (vec![0, 5, 20], 5, 20),
        (vec![7, 7, 7], 0, 0),
        (vec![20, 5, 0], 0, 0),
    ] {
        let (sink, _) = Watching::new();
        let seen = perform_observed(
            &get(&format!("{url}/x"), None),
            observed(
                "p",
                ProviderPurpose::TokenRefresh,
                sink.clone(),
                Script::at(&stamps),
            ),
        )
        .expect("the scripted reply succeeds");
        assert_eq!(seen.0, 200);
        let records = sink.records();
        // docs/state.md, "Diagnostic logs": one line when the request ends, with the provider's purpose.
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].headers_ms, Some(headers_ms));
        assert_eq!(records[0].total_ms, total_ms);
    }
}

#[test]
fn partial_read_keeps_status_headers_and_count() {
    let prefix = vec![9u8; 5];
    let server = ProviderServer::start([Response::stall(200, prefix.clone(), 100)]).unwrap();
    let again = ProviderServer::start([Response::stall(200, prefix.clone(), 100)]).unwrap();
    let plain = perform(&get(&server.url(), Some(Duration::from_millis(300)))).unwrap_err();
    let (sink, _) = Watching::new();
    let seen = perform_observed(
        &get(&again.url(), Some(Duration::from_millis(300))),
        observed(
            "p",
            ProviderPurpose::ModelList,
            sink.clone(),
            FakeClock::new(),
        ),
    )
    .unwrap_err();
    // The same classified error on both paths: the passed backstop is a
    // timeout, never elapsed wall milliseconds.
    assert_eq!(plain, seen);
    assert_eq!(plain.0, ErrorCode::Timeout);
    // The partial reply keeps its status, its headers and the bytes read
    // before the failure, not the configured length.
    let records = sink.records();
    // docs/state.md, "Diagnostic logs": a body read that fails keeps the status, the headers and the bytes read before the failure.
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].status, Some(200));
    assert!(records[0].headers_ms.is_some());
    assert_eq!(records[0].response_bytes, prefix.len() as u64);
}

#[test]
fn cap_boundaries_hold_on_both_paths() {
    let cap = 10 * 1024 * 1024;
    for (size, status) in [
        (cap - 1, 200),
        (cap, 200),
        (cap + 1, 200),
        (cap - 1, 503),
        (cap, 503),
        (cap + 1, 503),
    ] {
        let body = vec![b'x'; size];
        let server =
            ProviderServer::start_responding(move |_| Response::status(status, body.clone()))
                .unwrap();
        let request = get(&server.url(), None);
        let plain = fakes::within("perform", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
            let request = HttpRequest {
                method: request.method.clone(),
                url: request.url.clone(),
                headers: Vec::new(),
                body: None,
                timeout: None,
            };
            move || perform(&request)
        });
        let (sink, _) = Watching::new();
        let held = sink.clone();
        let seen = fakes::within(
            "perform_observed",
            Deadline::after(fakes::MUST_SUCCEED_WITHIN),
            {
                let request = HttpRequest {
                    method: "GET".to_owned(),
                    url: server.url(),
                    headers: Vec::new(),
                    body: None,
                    timeout: None,
                };
                move || {
                    perform_observed(
                        &request,
                        observed("p", ProviderPurpose::Quota, held.clone(), FakeClock::new()),
                    )
                }
            },
        );
        assert_eq!(plain, seen);
        let records = sink.records();
        // docs/state.md, "Diagnostic logs": a body read that fails keeps the status, the headers and the bytes read before the failure.
        assert_eq!(records.len(), 1);
        // Past the bound the read fails and the bytes read so far are
        // kept: exactly the bound, not the configured length.
        let want_bytes = (size as u64).min(cap as u64);
        assert_eq!(records[0].status, Some(status));
        assert_eq!(records[0].response_bytes, want_bytes);
        match (size == cap - 1, &seen) {
            (true, Ok((got, bytes))) => {
                assert_eq!(*got, status);
                assert_eq!(bytes.len(), size);
            }
            (false, Err((code, _))) => assert_eq!(*code, ErrorCode::ConnectionFailed),
            (true, Err(_)) | (false, Ok(_)) => {
                panic!("size {size} status {status}: the bound decides the outcome")
            }
        }
    }
}

#[test]
fn conversions_truncate_saturate_and_floor() {
    assert_eq!(duration_ms(Duration::ZERO), 0);
    assert_eq!(duration_ms(Duration::from_nanos(999_999)), 0);
    assert_eq!(duration_ms(Duration::from_millis(1)), 1);
    assert_eq!(duration_ms(Duration::from_nanos(1_999_999)), 1);
    assert_eq!(
        duration_ms(Duration::from_millis(u64::MAX - 1)),
        u64::MAX - 1
    );
    assert_eq!(duration_ms(Duration::from_millis(u64::MAX)), u64::MAX);
    let origin = FakeClock::new().origin();
    assert_eq!(ms_since(origin, origin), 0);
    assert_eq!(ms_since(origin, origin + Duration::from_millis(20)), 20);
    assert_eq!(ms_since(origin + Duration::from_millis(20), origin), 0);
    assert_eq!(byte_len(0), 0);
    assert_eq!(byte_len(1 << 20), 1 << 20);
    assert_eq!(add_bytes(0, 0), 0);
    assert_eq!(add_bytes(5, 10), 15);
    assert_eq!(add_bytes(u64::MAX - 1, 1), u64::MAX);
    assert_eq!(add_bytes(u64::MAX, 1), u64::MAX);
    assert_eq!(add_bytes(u64::MAX, usize::MAX), u64::MAX);
}

#[test]
fn purpose_mapping_covers_every_target() {
    use crate::lua::{Target, http_observation};
    use crate::oauth::LoginSlot;

    let pair = crate::CredentialPair {
        credential: "c".to_owned(),
        label: "l".to_owned(),
    };
    let slot: LoginSlot = Arc::new(Mutex::new(None));
    for (target, purpose) in [
        (Target::Command("c".to_owned()), None),
        (
            Target::Hook {
                point: "before_message".to_owned(),
                index: 0,
            },
            None,
        ),
        (Target::Timer { id: 0 }, None),
        (Target::Tool("t".to_owned()), None),
        (Target::Search("s".to_owned()), None),
        (Target::Effects("e".to_owned()), None),
        (
            Target::Provider {
                name: "p".to_owned(),
                function: "models",
                credential: None,
            },
            Some(ProviderPurpose::ModelList),
        ),
        (
            Target::Provider {
                name: "p".to_owned(),
                function: "quota",
                credential: None,
            },
            Some(ProviderPurpose::Quota),
        ),
        (
            Target::Provider {
                name: "p".to_owned(),
                function: "credential",
                credential: None,
            },
            Some(ProviderPurpose::TokenRefresh),
        ),
        (
            Target::Provider {
                name: "ext".to_owned(),
                function: "credential",
                credential: Some(CredentialFor::Stored(pair.clone())),
            },
            Some(ProviderPurpose::TokenRefresh),
        ),
        (
            Target::Provider {
                name: "ext".to_owned(),
                function: "credential",
                credential: Some(CredentialFor::Login(Arc::clone(&slot))),
            },
            Some(ProviderPurpose::TokenRefresh),
        ),
        (
            Target::Provider {
                name: "p".to_owned(),
                function: "cost",
                credential: None,
            },
            Some(ProviderPurpose::Cost),
        ),
        (
            Target::Provider {
                name: "p".to_owned(),
                function: "sign",
                credential: None,
            },
            None,
        ),
        (
            Target::Provider {
                name: "p".to_owned(),
                function: "bogus",
                credential: None,
            },
            None,
        ),
    ] {
        assert_eq!(
            provider_purpose(&target).map(|(_, purpose)| purpose),
            purpose,
            "the mapping decides by function alone"
        );
        if let Some(purpose) = purpose {
            let (name, _) = provider_purpose(&target).unwrap();
            assert!(
                ["p", "ext"].contains(&name),
                "the provider's registered name is kept"
            );
            let _ = purpose;
        }
    }
    // The snapshot filters before any stamp, read or write: no sink, an
    // info sink, or no debugging all give none without touching the clock.
    // Outside a provider function the request is the extension's own, so
    // hooks, commands, timers, tools, watchers, searches, effects and
    // delegates all observe with the extension's name.
    let target = Target::Provider {
        name: "p".to_owned(),
        function: "models",
        credential: None,
    };
    let bare = crate::lua::Hub::new(FakeClock::new());
    assert!(http_observation(&bare, &target, "mem").is_none());
    let counting = Counting::new();
    let dark_hub = crate::lua::Hub::new(counting.clone());
    dark_hub.set_diagnostics(Arc::new(Dark));
    assert!(http_observation(&dark_hub, &target, "mem").is_none());
    assert_eq!(counting.reads(), 0);
    let (sink, _) = Watching::new();
    let hub = crate::lua::Hub::new(FakeClock::new());
    hub.set_diagnostics(sink.clone());
    let context = http_observation(&hub, &target, "mem").expect("an eligible target observes");
    assert_eq!(
        context.attribution,
        crate::host::HttpAttribution::Provider {
            provider: "p".to_owned(),
            purpose: ProviderPurpose::ModelList,
        }
    );
    assert!(sink.records().is_empty());
    // Every non-provider target observes as the extension, with no
    // provider and no host, path or model on the line.
    for target in [
        Target::Command("c".to_owned()),
        Target::Hook {
            point: "before_message".to_owned(),
            index: 0,
        },
        Target::Watcher { index: 0 },
        Target::Timer { id: 0 },
        Target::Tool("t".to_owned()),
        Target::Search("s".to_owned()),
        Target::Effects("e".to_owned()),
        Target::Delegate {
            extension: "mem".to_owned(),
            job_id: contract::JobId("1".to_owned()),
            handle: 1,
            timeout: std::time::Duration::from_secs(1),
        },
    ] {
        let context =
            http_observation(&hub, &target, "mem").expect("outside a provider function observes");
        assert_eq!(
            context.attribution,
            crate::host::HttpAttribution::Extension {
                extension: "mem".to_owned(),
            }
        );
    }
    // A provider function without a purpose stays silent, whatever HTTP
    // it sends.
    for function in ["sign", "bogus"] {
        let target = Target::Provider {
            name: "p".to_owned(),
            function,
            credential: None,
        };
        assert!(http_observation(&hub, &target, "mem").is_none());
    }
    assert!(sink.records().is_empty());
}

// Admission and worker ordering: what runs before admission, what the
// worker proves after it, and what late workers leave behind.

use crate::LuaProvider;
use crate::lua::Target;

/// A directory holding `init.lua` and one provider file per entry.
fn ext_dir(init: Option<&str>, providers: &[(&str, &str)]) -> fakes::TempDir {
    let dir = fakes::TempDir::new("fiber-observed-admission");
    if let Some(init) = init {
        fs::write(dir.path().join("init.lua"), init).unwrap();
    }
    fs::create_dir_all(dir.path().join("providers")).unwrap();
    for (name, lua) in providers {
        fs::write(
            dir.path().join("providers").join(format!("{name}.lua")),
            lua,
        )
        .unwrap();
    }
    dir
}

fn models_lua(name: &str, url: &str) -> String {
    let mut out = String::from("fiber.provider(\"");
    out.push_str(name);
    out.push_str("\", { models = { timeout = 60000, run = function()\n");
    out.push_str("local reply = host.http({ url = \"");
    out.push_str(url);
    out.push_str("/models\" })\n");
    out.push_str("return { { id = \"m1\", protocol = \"openai-responses\", base_url = \"");
    out.push_str(url);
    out.push_str("\" } }\n");
    out.push_str("end } })\n");
    out
}

/// A clock that gates one park inside the fake clock's wait path, from
/// this file only. `wait_until` delegates to the inner fake clock, but
/// when `until` matches the armed target it signals arrival after the
/// inner clock recorded the park (so `await_parked` already fires) and
/// blocks before invoking the outer wait closure. The caller still holds
/// the hub guard until that closure's condvar wait releases it
/// (`support::clock::park` over `Hub::wait`), so the block holds the
/// guard in the exact park-recorded-but-not-released window. Fires once
/// per arming; every other wait passes straight through, so existing
/// deadlines and the fake clock are unchanged.
struct GatedClock {
    inner: Arc<FakeClock>,
    gate: Mutex<Option<GatedGate>>,
}

struct GatedGate {
    target: Option<Instant>,
    arrived: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
}

impl GatedClock {
    fn new(inner: Arc<FakeClock>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            gate: Mutex::new(None),
        })
    }

    fn arm(&self, target: Option<Instant>) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        *self.gate.lock().unwrap() = Some(GatedGate {
            target,
            arrived: arrived_tx,
            release: release_rx,
        });
        (arrived_rx, release_tx)
    }
}

impl Clock for GatedClock {
    fn now(&self) -> Instant {
        self.inner.now()
    }

    fn wall(&self) -> SystemTime {
        self.inner.wall()
    }

    fn sleep(&self, d: Duration) {
        self.inner.sleep(d);
    }

    fn wait_until(&self, until: Option<Instant>, wait: &mut dyn FnMut(Option<Duration>)) {
        let armed = self
            .gate
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|gate| gate.target == until);
        if !armed {
            self.inner.wait_until(until, wait);
            return;
        }
        let (arrived, release) = {
            let mut gate = self.gate.lock().unwrap();
            let gate = gate.take().expect("the gated window was armed");
            (gate.arrived, gate.release)
        };
        self.inner.wait_until(until, &mut |bound| {
            // The inner clock recorded the park and notified `await_parked`;
            // the outer condvar wait has not run, so the hub guard is held.
            let _ignored = arrived.send(());
            let _ignored = release.recv();
            wait(bound);
        });
    }

    fn subscribe(&self, waker: std::sync::Weak<dyn contract::clock::Wake>) {
        self.inner.subscribe(waker);
    }
}

/// The message of a panic caught on a helper thread: `&str` or `String`,
/// else a fixed text. The helper must run on its own thread because its
/// wake takes the hub lock the gated caller holds, so the outcome travels
/// back over a channel and the test re-panics with it at the call site.
fn panic_text(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "the advance helper failed with an unknown panic payload".to_owned()
    }
}

/// Waits for the gated park window: on a miss, drains the helper outcome
/// and re-panics with its text, so a wrong instant fails with the
/// helper's park report instead of the receive timeout.
fn gate_reached(arrived: &mpsc::Receiver<()>, outcome: &mpsc::Receiver<Result<(), String>>) {
    if Deadline::after(MUST_SUCCEED_WITHIN).recv(arrived).is_err() {
        match Deadline::after(MUST_SUCCEED_WITHIN).recv(outcome) {
            Ok(Err(text)) => panic!("the advance helper failed: {text}"),
            _ => panic!("the caller reached the gated park window"),
        }
    }
}

/// Waits, at most `within` of real time, until the hub lock is free.
/// `FakeClock::wait_until` records the caller's park and wakes
/// `await_parked` before its wait closure runs, and the caller still holds
/// the hub guard until the condvar wait inside that closure releases it
/// (`support::clock::park` over `Hub::wait`). A single `hub_unlocked()`
/// read can land in that window, so the read repeats until one of them
/// proves the release: success is the proof, and expiry fails naming the
/// guard, as the assert this replaces did. The `GatedClock` above forces
/// that window with a blocked gate, so the test proves this wait holds
/// through it instead of merely passing after it.
#[track_caller]
fn await_unlocked(ext: &LuaExtension, within: Duration) {
    let deadline = Deadline::after(within);
    while !ext.hub_unlocked() {
        assert!(
            !deadline.left().is_zero(),
            "the guard released before emission"
        );
        std::thread::yield_now();
    }
}

#[test]
fn worker_emits_only_after_the_guard_releases() {
    unpause_observed_execution();
    let server =
        ProviderServer::start_responding(|_| Response::status(200, "{\"data\":[]}".to_owned()))
            .unwrap();
    let hook_server = ProviderServer::start_responding(|_| Response::status(200, "{}")).unwrap();
    // A hook plus a provider on one VM: the hook freezes the scheduler
    // at a no-lock pause while the provider's worker waits past the
    // rendezvous.
    let dir = ext_dir(
        Some(&pausing_hook(&hook_server.url())),
        &[("rz", &models_lua("rz", &server.url()))],
    );
    let fake = FakeClock::new();
    let clock = GatedClock::new(Arc::clone(&fake));
    let clock_dyn: Arc<dyn Clock> = clock.clone();
    let (sink, _) = Watching::new();
    let ext = Arc::new(
        LuaExtension::new("ext", dir.path(), dir.path(), Arc::clone(&clock_dyn))
            .with_diagnostics(sink.clone() as Arc<dyn DebugLog>),
    );
    let (arrived, release) = pause_observed_execution("rz");
    let (paused, resume) = pause_at_admission();
    let mark = fake.now();
    let provider = LuaProvider::new(Arc::clone(&ext), "rz");
    let (tx, rx) = mpsc::channel();
    let models_caller = std::thread::spawn(move || {
        let _sent = tx.send(provider.list_models(&ModelsArg::default()));
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived)
        .expect("the worker reached its pause");
    // Freeze the scheduler at its outside-lock pause before permitting
    // emission: the hook suspends, admits nothing yet and holds no lock.
    let (hook_tx, hook_rx) = mpsc::channel();
    {
        let ext = Arc::clone(&ext);
        std::thread::spawn(move || {
            let answer = ext.hook(PAUSE_POINT, 0, serde_json::Value::Null);
            let _sent = hook_tx.send(answer);
        });
    }
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&paused)
        .expect("the scheduler reached its pause");
    // Both callers settled past every lock acquisition before the guard is
    // read: the hook caller waits at its deadline while its callback runs
    // on the thread, and the models caller waits past its check, its
    // deadline plus the grace for a suspended call, while its callback
    // stays suspended. A transient hold no longer fails the read below.
    assert!(
        fake.await_parked(
            mark + Duration::from_secs(60),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the hook caller parked at its deadline"
    );
    assert!(
        fake.await_parked(
            mark + Duration::from_secs(61),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the caller parked past its check"
    );
    // Gate the models caller's next park past its check: the window where
    // the fake clock recorded the park but the caller still holds the hub
    // guard. Both callers are parked above with the guard free; arming now
    // cannot block their first park, and a zero advance wakes them into a
    // re-park without moving time, so the gate catches the re-entry
    // deterministically. The forced interleaving: hold `await_unlocked` in
    // that window and prove it cannot complete until the release — a
    // blocked gate plus channels, no sleeps or reruns.
    let (gated_arrived, gated_release) = clock.arm(Some(mark + Duration::from_secs(61)));
    // The zero advance wakes the parked callers into their re-park, and
    // the gated caller holds the hub guard while it blocks on the
    // release. The advance's wake takes the hub lock, so it runs on its
    // own thread: running it here would block forever with only the gate
    // able to release it.
    let (advanced_tx, advanced_rx) = mpsc::channel();
    {
        let fake = Arc::clone(&fake);
        let models_caller = models_caller.thread().id();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fake.advance_when_parked(
                    Some(mark + Duration::from_secs(61)),
                    Some(models_caller),
                    Duration::ZERO,
                    &Deadline::after(MUST_SUCCEED_WITHIN),
                );
            }))
            .map_err(panic_text);
            let _ignored = advanced_tx.send(outcome);
        });
    }
    gate_reached(&gated_arrived, &advanced_rx);
    {
        let (done_tx, done_rx) = mpsc::channel();
        {
            let ext = Arc::clone(&ext);
            std::thread::spawn(move || {
                await_unlocked(&ext, MUST_SUCCEED_WITHIN);
                let _ignored = done_tx.send(());
            });
        }
        assert!(
            Deadline::after(Duration::from_millis(200))
                .recv(&done_rx)
                .is_err(),
            "await_unlocked completed while the guard was held in the park window"
        );
        assert!(
            !ext.hub_unlocked(),
            "the guard is held while the park window is gated"
        );
        gated_release
            .send(())
            .expect("the gated window takes its release");
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&done_rx)
            .expect("await_unlocked completes after the release");
    }
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&advanced_rx)
        .expect("the zero advance returns after the release")
        .expect("the zero advance succeeded");
    assert!(
        fake.await_parked(
            mark + Duration::from_secs(61),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the caller parked past its check"
    );
    // With the scheduler frozen past the rendezvous and the worker gated
    // past it, the admission guard is free and no probe has run: a worker
    // holding the lock across its pause, or a write before it, fails here.
    // The parked signals above fire when each caller records its park,
    // before its condvar wait releases the hub guard, so the guard is read
    // only once a read proves the release.
    assert!(sink.records().is_empty());
    // Stamps after this advance prove they ran after the pause: a stamp
    // from before it would end 5000 ms later.
    fake.advance_when_parked(
        Some(mark + Duration::from_secs(61)),
        Some(models_caller.thread().id()),
        Duration::from_millis(5000),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    // Both releases go out before either answer is awaited: the frozen
    // scheduler delivers the worker's reply only once the hook resumes it.
    release.send(()).expect("the worker takes its release");
    resume.send(()).expect("the hook takes its release");
    let (models, _) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the models")
        .expect("the observed models() succeeds");
    assert_eq!(models.len(), 1);
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&hook_rx)
        .expect("waited for the hook")
        .expect("the released hook completes");
    unpause_settle();
    // The worker's line and the released hook's line both land: one
    // provider line for the models call, one extension line for the
    // hook's own HTTP, matched as a set since the two writes race.
    let records = sink.records();
    // docs/state.md, "Diagnostic logs": where the call is made decides the purpose: a provider function keeps its own, anything else records extension.
    assert_eq!(
        records.len(),
        2,
        "the worker and the released hook each record"
    );
    let provider_record = records
        .iter()
        .find(|record| record.identity() == Some(&Identity::Provider("rz".to_owned())))
        .expect("the models call records its provider line");
    assert_eq!(provider_record.purpose(), Purpose::ModelList);
    assert_eq!(provider_record.status, Some(200));
    assert_eq!(provider_record.attempt, 1);
    assert_eq!(provider_record.headers_ms, Some(0));
    assert_eq!(provider_record.total_ms, 0);
    let hook_record = records
        .iter()
        .find(|record| record.identity() == Some(&Identity::Extension("ext".to_owned())))
        .expect("the released hook records its extension line");
    assert_eq!(hook_record.purpose(), Purpose::Extension);
    assert_eq!(hook_record.status, Some(200));
}

#[test]
fn gated_park_holds_the_hub_guard() {
    let server = ProviderServer::start([Response::status(200, "{\"data\":[]}")]).unwrap();
    server.hold_from(1, Duration::from_secs(30));
    let dir = ext_dir(None, &[("rg", &models_lua("rg", &server.url()))]);
    let fake = FakeClock::new();
    let clock = GatedClock::new(Arc::clone(&fake));
    let clock_dyn: Arc<dyn Clock> = clock.clone();
    let (sink, _) = Watching::new();
    let ext = Arc::new(
        LuaExtension::new("ext", dir.path(), dir.path(), Arc::clone(&clock_dyn))
            .with_diagnostics(sink.clone() as Arc<dyn DebugLog>),
    );
    // No pause globals: the held HTTP reply suspends the worker, so this
    // repro shares nothing with the tests that arm them.
    let mark = fake.now();
    let provider = LuaProvider::new(Arc::clone(&ext), "rg");
    let (tx, rx) = mpsc::channel();
    let models_caller = std::thread::spawn(move || {
        let _sent = tx.send(provider.list_models(&ModelsArg::default()));
    });
    assert!(
        server.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the worker suspended on HTTP"
    );
    assert!(
        fake.await_parked(
            mark + Duration::from_secs(61),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the caller parked past its check"
    );
    let (gated_arrived, gated_release) = clock.arm(Some(mark + Duration::from_secs(61)));
    // The hub notify runs on its own thread: the gated caller holds the
    // hub guard while it blocks on the release, and the notify's wake
    // takes the hub lock.
    let (notified_tx, notified_rx) = mpsc::channel();
    {
        let fake = Arc::clone(&fake);
        let models_caller = models_caller.thread().id();
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                fake.advance_when_parked(
                    Some(mark + Duration::from_secs(61)),
                    Some(models_caller),
                    Duration::ZERO,
                    &Deadline::after(MUST_SUCCEED_WITHIN),
                );
            }))
            .map_err(panic_text);
            let _ignored = notified_tx.send(outcome);
        });
    }
    gate_reached(&gated_arrived, &notified_rx);
    assert!(
        !ext.hub_unlocked(),
        "the gated caller holds the hub lock while parked"
    );
    gated_release
        .send(())
        .expect("the gated window takes its release");
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&notified_rx)
        .expect("the hub notify returns after the release")
        .expect("the zero advance succeeded");
    server.release();
    let (models, _) = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&rx)
        .expect("waited for the models")
        .expect("the models() succeeds after the release");
    assert_eq!(models.len(), 1);
}

#[test]
fn spawn_failure_starts_no_request_or_record() {
    let server =
        ProviderServer::start_responding(|_| Response::status(200, "{\"data\":[]}")).unwrap();
    let dir = ext_dir(None, &[("sf", &models_lua("sf", &server.url()))]);
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let ext = LuaExtension::new("ext", dir.path(), dir.path(), clock)
        .with_diagnostics(sink.clone() as Arc<dyn DebugLog>);
    fail_next_spawn_for("sf");
    let err = fakes::within(
        "ext.provider_call",
        Deadline::after(fakes::MUST_SUCCEED_WITHIN),
        move || ext.provider_call("sf", "models", serde_json::Value::Null),
    )
    .unwrap_err();
    assert_eq!(err.code(), ErrorCode::IoFailed);
    assert!(server.requests().is_empty(), "a failed spawn sends nothing");
    assert!(sink.records().is_empty());
}

/// Registered by no other test in this file. The pause is a process-wide
/// hook, and nextest runs each test in its own process, so tests elsewhere
/// never meet it.
const PAUSE_POINT: &str = "before_handoff";

fn pausing_hook(url: &str) -> String {
    format!(
        "fiber.hook(\"{PAUSE_POINT}\", {{ timeout = 60000, on_failure = \"non-blocking\",\n\
           run = function(arg) host.http({{ url = \"{url}/held\" }}) return nil end }})\n"
    )
}

fn queued_models_lua(url: &str) -> String {
    format!(
        "fiber.provider(\"b\", {{ models = {{ timeout = 1000, run = function()\n\
           local reply = host.http({{ url = \"{url}/models\" }})\n\
           return {{ {{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{url}\" }} }}\n\
         end }} }})\n"
    )
}

/// Holds the extension's thread at its next admission of the pausing hook,
/// returning the arrival and the release. Only that hook waits: every
/// other suspension passes through, so parallel tests never block here.
fn pause_at_admission() -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    unpause_settle();
    let (arrived_tx, arrived_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release = Arc::new(Mutex::new(release_rx));
    pause_settle(Arc::new(move |target: &Target, pause: Pause| {
        if pause == Pause::Admission
            && matches!(target, Target::Hook { point, .. } if point == PAUSE_POINT)
        {
            let _ignored = arrived_tx.send(());
            let _ignored = release.lock().unwrap().recv();
        }
    }));
    (arrived_rx, release_tx)
}

#[test]
fn expired_before_admission_starts_nothing() {
    let server_a = ProviderServer::start_responding(|_| Response::status(200, "{}")).unwrap();
    let server_b =
        ProviderServer::start_responding(|_| Response::status(200, "{\"data\":[]}")).unwrap();
    let dir = ext_dir(
        Some(&pausing_hook(&server_a.url())),
        &[("b", &queued_models_lua(&server_b.url()))],
    );
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let ext = Arc::new(
        LuaExtension::new("ext", dir.path(), dir.path(), clock.clone())
            .with_diagnostics(sink.clone() as Arc<dyn DebugLog>),
    );
    // The provider file loads before anything suspends: the queued call
    // below waits only on the held extension thread.
    ext.ensure_provider("b").expect("the provider file loads");
    let (arrived, release) = pause_at_admission();
    let (hook_tx, hook_rx) = mpsc::channel();
    {
        let ext = Arc::clone(&ext);
        std::thread::spawn(move || {
            let answer = ext.hook(PAUSE_POINT, 0, serde_json::Value::Null);
            let _sent = hook_tx.send(answer);
        });
    }
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived)
        .expect("the hook reached admission");
    // Queued behind the held thread, the provider call expires before the
    // extension ever runs it.
    let mark = clock.now();
    let (expired_tx, expired_rx) = mpsc::channel();
    let call_thread = {
        let ext = Arc::clone(&ext);
        std::thread::spawn(move || {
            let answer = ext.provider_call("b", "models", serde_json::Value::Null);
            let _sent = expired_tx.send(answer);
        })
    };
    assert!(
        clock.await_parked(
            mark + Duration::from_millis(1000),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the queued call parked at its deadline"
    );
    clock.advance_when_parked(
        Some(mark + Duration::from_millis(1000)),
        Some(call_thread.thread().id()),
        Duration::from_millis(1000),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let err = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&expired_rx)
        .expect("waited for the expiry")
        .unwrap_err();
    assert!(
        matches!(err, crate::Error::Timeout { .. }),
        "the queued call takes its timeout: {err:?}"
    );
    release.send(()).expect("the hook takes its release");
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&hook_rx)
        .expect("waited for the hook")
        .expect("the released hook completes");
    unpause_settle();
    assert!(
        server_b.requests().is_empty(),
        "an unadmitted call sends nothing"
    );
    // The expired provider call records nothing; the released hook ran
    // its own HTTP, which records as the extension's.
    let records = sink.records();
    // docs/state.md, "Diagnostic logs": any other host.http call records one line with purpose extension.
    assert_eq!(records.len(), 1, "only the released hook's request records");
    let record = &records[0];
    assert_eq!(
        record.identity(),
        Some(&Identity::Extension("ext".to_owned()))
    );
    assert_eq!(record.purpose(), Purpose::Extension);
}

#[test]
fn stopped_before_admission_starts_nothing() {
    let server = ProviderServer::start_responding(|_| Response::status(200, "{}")).unwrap();
    let dir = ext_dir(Some(&pausing_hook(&server.url())), &[]);
    let clock = FakeClock::new();
    let (sink, _) = Watching::new();
    let ext = Arc::new(
        LuaExtension::new("ext", dir.path(), dir.path(), clock)
            .with_diagnostics(sink.clone() as Arc<dyn DebugLog>),
    );
    let (arrived, release) = pause_at_admission();
    let (hook_tx, hook_rx) = mpsc::channel();
    {
        let ext = Arc::clone(&ext);
        std::thread::spawn(move || {
            let answer = ext.hook(PAUSE_POINT, 0, serde_json::Value::Null);
            let _sent = hook_tx.send(answer);
        });
    }
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrived)
        .expect("the hook reached admission");
    ext.dispose();
    release.send(()).expect("the hook takes its release");
    let err = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&hook_rx)
        .expect("waited for the hook")
        .unwrap_err();
    unpause_settle();
    assert!(
        matches!(err, crate::Error::Stopped { .. }),
        "a stopped extension admits nothing: {err:?}"
    );
    assert!(server.requests().is_empty());
    assert!(sink.records().is_empty());
}

/// An expiring hook plus a provider stopped mid-flight: the hook's caller
/// takes its timeout while the VM stays up, the next callback runs, a
/// late observed worker still emits its one accurate record, and late
/// replies are discarded.
#[test]
fn late_workers_emit_once_and_drop_late_replies() {
    let hook_server = ProviderServer::start([Response::status(200, "{}")]).unwrap();
    hook_server.hold_from(1, Duration::from_secs(30));
    let models_server = ProviderServer::start([Response::status(200, "{\"data\":[]}")]).unwrap();
    models_server.hold_from(1, Duration::from_secs(30));
    let hook_url = hook_server.url();
    let models_url = models_server.url();
    let init = format!(
        "fiber.hook(\"before_message\", {{ timeout = 1000, on_failure = \"blocking\",\n\
           run = function(msg) host.http({{ url = \"{hook_url}/held\" }}) return {{}} end }})\n\
         fiber.hook(\"turn_start\", {{ timeout = 60000, on_failure = \"non-blocking\",\n\
           run = function(ctx) return nil end }})\n"
    );
    let provider = format!(
        "fiber.provider(\"p\", {{ models = {{ timeout = 60000, run = function()\n\
           local reply = host.http({{ url = \"{models_url}/models\" }})\n\
           return {{ {{ id = \"m1\", protocol = \"openai-responses\", base_url = \"{models_url}\" }} }}\n\
         end }} }})\n"
    );
    let dir = ext_dir(Some(&init), &[("p", &provider)]);
    let clock = FakeClock::new();
    let (sink, arrivals) = Watching::new();
    let ext = Arc::new(
        LuaExtension::new("ext", dir.path(), dir.path(), clock.clone())
            .with_diagnostics(sink.clone() as Arc<dyn DebugLog>),
    );
    // The hook suspends on its held request.
    let mark = clock.now();
    let (hook_tx, hook_rx) = mpsc::channel();
    let hook_thread = {
        let ext = Arc::clone(&ext);
        std::thread::spawn(move || {
            let answer = ext.hook("before_message", 0, serde_json::Value::Null);
            let _sent = hook_tx.send(answer);
        })
    };
    assert!(
        hook_server.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the hook suspended on HTTP"
    );
    assert!(
        clock.await_parked(
            mark + Duration::from_millis(2000),
            fakes::Deadline::after(MUST_SUCCEED_WITHIN)
        ),
        "the hook caller parked past its check on the suspended callback"
    );
    // Past the hook's deadline but inside its grace: its caller takes the
    // timeout and the VM stays up.
    clock.advance_when_parked(
        Some(mark + Duration::from_millis(2000)),
        Some(hook_thread.thread().id()),
        Duration::from_millis(1500),
        &Deadline::after(MUST_SUCCEED_WITHIN),
    );
    let err = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&hook_rx)
        .expect("waited for the expired hook")
        .unwrap_err();
    assert!(
        matches!(
            err,
            crate::Error::Timeout {
                timeout_ms: 1000,
                ..
            }
        ),
        "the expired callback returns its timeout: {err:?}"
    );
    // The next callback runs on the live VM.
    fakes::within("ext.hook", Deadline::after(fakes::MUST_SUCCEED_WITHIN), {
        let ext = Arc::clone(&ext);
        move || ext.hook("turn_start", 0, serde_json::Value::Null)
    })
    .expect("the next callback runs after the expiry");
    // A provider call suspends on its held request; stopping the VM ends
    // its caller while the worker is still in flight.
    let observer = LuaProvider::new(Arc::clone(&ext), "p");
    let (models_tx, models_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _sent = models_tx.send(observer.list_models(&ModelsArg::default()));
    });
    assert!(
        models_server.await_requests(1, Deadline::after(MUST_SUCCEED_WITHIN)),
        "the provider suspended on HTTP"
    );
    ext.dispose();
    let stopped = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&models_rx)
        .expect("waited for the stopped call")
        .unwrap_err();
    assert!(
        matches!(stopped, crate::Error::Stopped { .. }),
        "a stopped call ends stopped: {stopped:?}"
    );
    // The late observed worker still emits its one accurate record once
    // its held request is released, and its reply is discarded.
    models_server.release();
    let record = Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&arrivals)
        .expect("the late worker emitted its record");
    assert_eq!(record.identity(), Some(&Identity::Provider("p".to_owned())));
    assert_eq!(record.purpose(), Purpose::ModelList);
    assert_eq!(record.status, Some(200));
    assert_eq!(record.attempt, 1);
    assert_eq!(record.request_bytes, 0);
    assert_eq!(record.response_bytes, 11);
    // The hook's late reply is discarded too: releasing its hold adds no
    // record and disturbs nothing.
    hook_server.release();
    // docs/state.md, "Diagnostic logs": one line when the request ends, with the provider's purpose.
    assert_eq!(sink.records().len(), 1);
    assert_eq!(hook_server.requests().len(), 1);
    assert_eq!(models_server.requests().len(), 1);
}
