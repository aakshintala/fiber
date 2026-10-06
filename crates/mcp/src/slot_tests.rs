//! The per-server slot through the public [`start`]: the first call to a
//! cached server starts it, later calls reuse it, a failed start dies once,
//! a removed tool fails without a call, and the cache follows the live list.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::ServerFailure;
use contract::tool::Tool;
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::server::ListedTool;
use crate::start::{DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, ServerSpec, Started, start};

/// How long a test waits for a thread or a child, in real time.
const WITHIN: Duration = Duration::from_secs(10);

/// One real-time poll of a child's exit.
const POLL: Duration = Duration::from_millis(50);

/// Poll iterations that span one `WITHIN` of `POLL` sleeps.
const POLLS: u128 = WITHIN.as_millis() / POLL.as_millis();

struct Setup {
    dir: TempDir,
    fake: Arc<FakeClock>,
}

impl Setup {
    fn new() -> Self {
        Self {
            dir: TempDir::new("fiber-mcp-slot"),
            fake: FakeClock::new(),
        }
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.fake.clone()
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }

    fn cache(&self) -> std::path::PathBuf {
        self.dir.path().join("cache")
    }

    fn tools(&self, tools: &Value) {
        std::fs::write(self.dir.path().join("tools.json"), tools.to_string()).expect("tools");
    }

    fn result(&self, name: &str, body: &str) {
        std::fs::write(self.dir.path().join(format!("call-{name}.json")), body).expect("result");
    }

    fn spec(&self, name: &str) -> ServerSpec {
        ServerSpec {
            name: name.to_owned(),
            command: fakes::mcp_fixture().display().to_string(),
            args: vec![self.dir.path().display().to_string()],
            env: BTreeMap::new(),
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
            call_timeout: DEFAULT_CALL_TIMEOUT,
            enabled: None,
            disabled: Vec::new(),
            hints: BTreeMap::new(),
            required: false,
        }
    }

    /// Writes the cache for `spec` holding `tools`, as a first start would.
    fn write_cache(&self, spec: &ServerSpec, tools: &[Value]) {
        crate::cache::write(
            &self.cache(),
            &spec.name,
            &crate::cache::key(&spec.command, &spec.args, &spec.env),
            tools,
        );
    }

    /// Starts once to populate the cache, stops, and clears the spawn
    /// signals, so the next start declares from the cache.
    fn populate(&self, spec: ServerSpec) {
        let started = self.start(vec![spec]);
        assert!(started.failed.is_empty());
        self.stop(&started);
        std::fs::remove_file(self.dir.path().join("pid.txt")).expect("pid.txt");
        let requests = self.dir.path().join("requests.log");
        if requests.exists() {
            std::fs::remove_file(requests).expect("requests.log");
        }
    }

    fn start(&self, specs: Vec<ServerSpec>) -> Started {
        let workspace = self.workspace();
        let cache = self.cache();
        let clock = self.clock();
        // Threaded with a wall-clock limit: a silent server would sit
        // parked on the fake clock forever, so a bare direct start would
        // hang the test instead of failing it.
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let started = start(specs, &workspace, &cache, &clock, "0.0.0");
            done.send(started).expect("collected");
        });
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the start ends within {WITHIN:?}"))
    }

    fn tool(&self, started: &Started, name: &str) -> Arc<dyn Tool> {
        started
            .tools
            .iter()
            .find(|(_, tool)| tool.definition().name == name)
            .unwrap_or_else(|| panic!("tool {name} is declared"))
            .1
            .clone()
    }

    fn run(&self, tool: &Arc<dyn Tool>) -> contract::tool::Output {
        // Threaded with a wall-clock limit: a silent server would sit
        // parked on the fake clock forever, so a bare direct call would
        // hang the test instead of failing it.
        let tool = Arc::clone(tool);
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let output = tool.run(
                &Default::default(),
                &fakes::CancelToken::new(),
                &fakes::Recorder::default(),
            );
            done.send(output).expect("collected");
        });
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the call ends within {WITHIN:?}"))
    }

    fn stop(&self, started: &Started) {
        // Threaded with a wall-clock limit: a lingering child would keep
        // the stop parked on the fake clock forever, so a bare direct
        // stop would hang the test instead of failing it.
        std::thread::scope(|scope| {
            let (done, stopped) = std::sync::mpsc::channel();
            scope.spawn(move || {
                started.servers.stop();
                done.send(()).expect("collected");
            });
            stopped
                .recv_timeout(WITHIN)
                .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
        });
    }

    fn listed(name: &str) -> Vec<Value> {
        vec![json!({"name": name})]
    }
}

fn initializes(dir: &std::path::Path) -> usize {
    let log = std::fs::read_to_string(dir.join("requests.log")).expect("requests.log");
    log.matches(r#""method":"initialize""#).count()
}

#[test]
fn the_first_call_starts_the_server_and_the_second_reuses_it() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    setup.populate(setup.spec("fx"));
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "declaring from the cache spawns nothing",
    );
    let tool = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&tool);
    assert!(first.error.is_none());
    assert!(
        setup.dir.path().join("pid.txt").exists(),
        "the first call starts the server",
    );
    let second = setup.run(&tool);
    assert!(second.error.is_none());
    assert_eq!(
        initializes(setup.dir.path()),
        1,
        "both calls share the one started server",
    );
    setup.stop(&started);
}

#[test]
fn two_concurrent_first_calls_spawn_once() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    setup.populate(setup.spec("fx"));
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let (done, results) = std::sync::mpsc::channel();
    for _ in 0..2 {
        let done = done.clone();
        let tool = Arc::clone(&tool);
        std::thread::spawn(move || {
            let output = tool.run(
                &Default::default(),
                &fakes::CancelToken::new(),
                &fakes::Recorder::default(),
            );
            done.send(output).expect("collected");
        });
    }
    drop(done);
    let first = results
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the first call ends within {WITHIN:?}"));
    let second = results
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the second call ends within {WITHIN:?}"));
    let outputs = vec![first, second];
    assert_eq!(outputs.len(), 2);
    for output in &outputs {
        assert!(output.error.is_none());
    }
    assert_eq!(
        initializes(setup.dir.path()),
        1,
        "concurrent first calls share one start",
    );
    setup.stop(&started);
}

#[test]
fn a_cached_server_whose_command_fails_dies_on_the_first_call() {
    let setup = Setup::new();
    let mut spec = setup.spec("bad");
    spec.command = "/no/such/command".to_owned();
    spec.args = Vec::new();
    setup.write_cache(&spec, &Setup::listed("echo"));
    let started = setup.start(vec![spec]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__bad__echo");
    let first = setup.run(&tool);
    let error = first.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpServerUnavailable);
    assert!(
        error
            .message
            .starts_with("The MCP server `bad` failed to start: "),
        "message: {}",
        error.message,
    );
    let record = first.server_failed.expect("the failed start is recorded");
    assert_eq!(record.server, "bad");
    assert_eq!(record.reason, ServerFailure::StartFailed);
    assert!(!record.will_restart);
    assert_eq!(record.error.code, ErrorCode::McpServerUnavailable);
    // The second call repeats the failure without another spawn attempt
    // and carries no record: exactly one `mcp_server_failed` per start.
    let second = setup.run(&tool);
    let again = second.error.expect("failed");
    assert_eq!(again.code, ErrorCode::McpServerUnavailable);
    assert!(second.server_failed.is_none());
    setup.stop(&started);
}

#[test]
fn a_lazy_start_that_misses_its_deadline_fails_with_deadline() {
    let setup = Setup::new();
    let mut spec = setup.spec("slow");
    spec.command = "/bin/sleep".to_owned();
    spec.args = vec!["30".to_owned()];
    setup.write_cache(&spec, &Setup::listed("echo"));
    let started = setup.start(vec![spec]);
    let tool = setup.tool(&started, "mcp__slow__echo");
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    let (done, result) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let output = tool.run(
                &Default::default(),
                &fakes::CancelToken::new(),
                &fakes::Recorder::default(),
            );
            done.send(output).expect("collected");
        });
        assert!(
            setup.fake.await_parked(deadline, WITHIN),
            "the lazy start waits on the startup deadline",
        );
        setup.fake.advance(DEFAULT_STARTUP_TIMEOUT);
        let output = result.recv_timeout(WITHIN).expect("the call ends");
        let error = output.error.expect("failed");
        assert_eq!(error.code, ErrorCode::McpServerUnavailable);
        assert_eq!(
            error.message,
            "The MCP server `slow` did not answer before its startup deadline of 5000 ms. \
             Raise `startup_timeout_ms` under `mcp.servers.slow` if it needs longer.",
        );
        let record = output.server_failed.expect("the failed start is recorded");
        assert_eq!(record.reason, ServerFailure::Deadline);
        assert!(!record.will_restart);
    });
    setup.stop(&started);
}

#[test]
fn a_call_to_a_removed_tool_fails_without_calling_and_updates_the_cache() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    // The cache names a tool the live server no longer lists.
    setup.write_cache(&setup.spec("fx"), &Setup::listed("gone"));
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__gone");
    let output = setup.run(&tool);
    let error = output.error.expect("failed");
    assert_eq!(error.code, ErrorCode::McpToolRemoved);
    assert_eq!(
        error.message,
        "The MCP server `fx` no longer has the tool `gone`; it stays declared until the next session.",
    );
    assert!(output.server_failed.is_none());
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    assert!(
        !log.contains(r#""method":"tools/call""#),
        "a removed tool is never called: {log}",
    );
    // The cache now holds the live list, so the next session declares it.
    let live = crate::cache::read(
        &setup.cache(),
        "fx",
        &crate::cache::key(
            &fakes::mcp_fixture().display().to_string(),
            &[setup.dir.path().display().to_string()],
            &BTreeMap::new(),
        ),
    )
    .expect("the cache holds the live list");
    assert_eq!(
        live.iter()
            .map(|entry| ListedTool::read(entry).name)
            .collect::<Vec<_>>(),
        ["echo"],
    );
    setup.stop(&started);
}

#[test]
fn a_live_list_equal_to_the_cache_leaves_the_file_untouched() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo", "description": "Echoes."}]));
    setup.result("echo", r#"{"content":[]}"#);
    setup.populate(setup.spec("fx"));
    let before =
        std::fs::read(setup.cache().join("fx.json")).expect("the first start writes the cache");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    let after = std::fs::read(setup.cache().join("fx.json")).expect("cache");
    assert_eq!(before, after, "an equal live list rewrites nothing");
    setup.stop(&started);
}

#[test]
fn stop_stops_a_lazily_started_server_and_ignores_a_never_started_one() {
    // A slot that never started: the stop spawns nothing and returns.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    setup.populate(setup.spec("fx"));
    let quiet = setup.start(vec![setup.spec("fx")]);
    let idle = setup.tool(&quiet, "mcp__fx__echo");
    setup.stop(&quiet);
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "stopping a never-started slot spawns nothing",
    );
    // A call after the stop fails without spawning: the stop marked the
    // slot dead.
    let refused = setup.run(&idle);
    assert_eq!(
        refused.error.as_ref().map(|error| &error.code),
        Some(&ErrorCode::McpServerUnavailable)
    );
    assert!(refused.server_failed.is_none());
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "nothing spawns after the stop",
    );
    // A lazily started server: the stop ends its child.
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    let pid: u32 = std::fs::read_to_string(setup.dir.path().join("pid.txt"))
        .expect("pid.txt")
        .trim()
        .parse()
        .expect("a pid");
    setup.stop(&started);
    let (_held, probe) = std::sync::mpsc::channel::<()>();
    for _ in 0..POLLS {
        if !fakes::kill_pid(pid, "0").expect("probe") {
            return;
        }
        match probe.recv_timeout(POLL) {
            Ok(()) | Err(_) => {}
        }
    }
    panic!("waited {WITHIN:?} for pid {pid} to exit after the stop");
}

#[test]
fn an_annotation_only_change_rewrites_the_cache() {
    // `idempotentHint` is not one of the hints Fiber reads, so the parsed
    // tools are equal either way: only the raw entries tell the live list
    // changed.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    setup.populate(setup.spec("fx"));
    let before = std::fs::read_to_string(setup.cache().join("fx.json"))
        .expect("the first start writes the cache");
    assert!(!before.contains("idempotentHint"), "{before:?}");
    setup.tools(&json!([{"name": "echo", "annotations": {"idempotentHint": true}}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    let after =
        std::fs::read_to_string(setup.cache().join("fx.json")).expect("cache");
    assert_ne!(before, after, "an annotation-only change rewrites the cache");
    assert!(
        after.contains("idempotentHint"),
        "the cache holds the raw entry: {after:?}",
    );
    setup.stop(&started);
}
