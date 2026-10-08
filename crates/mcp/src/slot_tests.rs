//! The per-server slot through the public [`start`]: the first call to a
//! cached server starts it, later calls reuse it, a failed start dies once,
//! a removed tool fails without a call, and the cache follows the live list.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::{McpServerFailed, ServerFailure};
use contract::tool::{ServerRecord, Tool};
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::server::ListedTool;
use crate::slot::{Run, Served, State};
use crate::start::{
    DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, ServerSpec, Servers, Started, start,
};

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

    /// Writes the cache for `spec` holding `tools` and no prompts, as a
    /// first start would.
    fn write_cache(&self, spec: &ServerSpec, tools: &[Value]) {
        crate::cache::write(
            &self.cache(),
            &spec.name,
            &crate::cache::key(&spec.command, &spec.args, &spec.env),
            &crate::cache::Cached {
                tools: tools.to_vec(),
                prompts: Vec::new(),
            },
        );
    }

    /// Starts once to populate the cache, stops, and clears the spawn
    /// signals, so the next start declares from the cache.
    fn populate(&self, spec: ServerSpec) {
        let started = self.start(vec![spec]);
        assert!(started.failed.is_empty());
        self.stop(started.servers);
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

    fn serve_slot(&self, slot: &Arc<crate::slot::Slot>) -> Served {
        // Threaded with a wall-clock limit: startup waits on the fake clock.
        let slot = Arc::clone(slot);
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            done.send(slot.serve()).expect("collected");
        });
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the serve ends within {WITHIN:?}"))
    }

    fn stop(&self, servers: Servers) {
        // Detached with a wall-clock limit: a lingering child would keep
        // the stop parked on the fake clock forever, so a bare direct
        // stop, or a join on its thread, would hang the test instead of
        // failing it.
        let (done, stopped) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            servers.stop();
            done.send(()).expect("collected");
        });
        stopped
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
    }

    fn pid(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("pid.txt"))
            .expect("pid.txt")
            .trim()
            .parse()
            .expect("a pid")
    }

    /// Marks a server with this name as never having spawned, so a test can
    /// tell whether a later call spawns it.
    fn forget_spawn(&self) {
        std::fs::remove_file(self.dir.path().join("pid.txt")).expect("pid.txt");
    }

    fn spawned(&self) -> bool {
        self.dir.path().join("pid.txt").exists()
    }

    /// Kills the running server of `started`'s only slot and waits, at most
    /// `WITHIN`, until its reader has seen the exit.
    fn kill(&self, started: &Started) {
        fakes::kill_pid(self.pid(), "KILL").expect("the server dies");
        let slot = &started.servers.slots[0];
        let (_held, probe) = std::sync::mpsc::channel::<()>();
        for _ in 0..POLLS {
            if let State::Running { server, .. } = &*super::lock(&slot.state)
                && server.is_gone()
            {
                return;
            }
            match probe.recv_timeout(POLL) {
                Ok(()) | Err(_) => {}
            }
        }
        panic!("waited {WITHIN:?} for the killed server to be gone");
    }

    fn cache_inode(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(self.cache().join("fx.json"))
            .expect("the cache file")
            .ino()
    }

    fn cached_names(&self) -> Vec<String> {
        crate::cache::read(
            &self.cache(),
            "fx",
            &crate::cache::key(
                &fakes::mcp_fixture().display().to_string(),
                &[self.dir.path().display().to_string()],
                &BTreeMap::new(),
            ),
        )
        .expect("the cache holds lists")
        .tools
        .iter()
        .map(|entry| ListedTool::read(entry).name)
        .collect()
    }

    fn cached_prompts(&self) -> Vec<Value> {
        crate::cache::read(
            &self.cache(),
            "fx",
            &crate::cache::key(
                &fakes::mcp_fixture().display().to_string(),
                &[self.dir.path().display().to_string()],
                &BTreeMap::new(),
            ),
        )
        .expect("the cache holds lists")
        .prompts
    }

    fn listed(name: &str) -> Vec<Value> {
        vec![json!({"name": name})]
    }
}

/// The call's one `mcp_server_failed` record, if it carries exactly that.
fn only_failed(servers: &[ServerRecord]) -> Option<McpServerFailed> {
    match servers {
        [ServerRecord::Failed(failed)] => Some(failed.clone()),
        _ => None,
    }
}

/// A call's server lines, one word each: `ready`, or the failure's reason
/// and whether Fiber will restart it.
fn lines(servers: &[ServerRecord]) -> Vec<String> {
    servers
        .iter()
        .map(|record| match record {
            ServerRecord::Failed(failed) => {
                assert_eq!(failed.server, "fx");
                assert_eq!(failed.error.code, ErrorCode::McpServerUnavailable);
                let reason = serde_json::to_value(failed.reason).expect("reason");
                format!(
                    "failed {} {}",
                    reason.as_str().expect("a string"),
                    if failed.will_restart {
                        "restart"
                    } else {
                        "final"
                    },
                )
            }
            ServerRecord::Ready(ready) => {
                assert_eq!(ready.server, "fx");
                "ready".to_owned()
            }
        })
        .collect()
}

fn code(output: &contract::tool::Output) -> Option<ErrorCode> {
    output.error.as_ref().map(|error| error.code.clone())
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
    setup.stop(started.servers);
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
    setup.stop(started.servers);
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
    let record = only_failed(&first.servers).expect("the failed start is recorded");
    assert_eq!(record.server, "bad");
    assert_eq!(record.reason, ServerFailure::StartFailed);
    assert!(record.will_restart, "a failed first start is its one death");
    assert_eq!(record.error.code, ErrorCode::McpServerUnavailable);
    // The second call tries the one restart, which fails too: the second
    // death, recorded once, with no restart left.
    let second = setup.run(&tool);
    let again = second.error.expect("failed");
    assert_eq!(again.code, ErrorCode::McpServerUnavailable);
    let last = only_failed(&second.servers).expect("the failed restart is recorded");
    assert_eq!(last.reason, ServerFailure::StartFailed);
    assert!(!last.will_restart);
    // Every later call repeats the failure and carries no record.
    let third = setup.run(&tool);
    assert_eq!(
        third.error.map(|error| error.code),
        Some(ErrorCode::McpServerUnavailable)
    );
    assert!(third.servers.is_empty());
    setup.stop(started.servers);
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
    // Detached, not scoped: a scope joins its thread even after a
    // `recv_timeout` panic, so a hung call would hang the test.
    let (done, result) = std::sync::mpsc::channel();
    let call = Arc::clone(&tool);
    std::thread::spawn(move || {
        let output = call.run(
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
    let record = only_failed(&output.servers).expect("the failed start is recorded");
    assert_eq!(record.reason, ServerFailure::Deadline);
    assert!(record.will_restart, "a failed first start is its one death");
    setup.stop(started.servers);
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
    assert!(output.servers.is_empty());
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
        live.tools
            .iter()
            .map(|entry| ListedTool::read(entry).name)
            .collect::<Vec<_>>(),
        ["echo"],
    );
    setup.stop(started.servers);
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
    setup.stop(started.servers);
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
    setup.stop(quiet.servers);
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
    assert!(refused.servers.is_empty());
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
    setup.stop(started.servers);
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
    let after = std::fs::read_to_string(setup.cache().join("fx.json")).expect("cache");
    assert_ne!(
        before, after,
        "an annotation-only change rewrites the cache"
    );
    assert!(
        after.contains("idempotentHint"),
        "the cache holds the raw entry: {after:?}",
    );
    setup.stop(started.servers);
}

const HI: &str = r#"{"content":[{"type":"text","text":"hi"}]}"#;

#[test]
fn a_failed_first_start_restarts_on_the_next_call() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    setup.populate(setup.spec("fx"));
    std::fs::write(setup.dir.path().join("fail-start"), "").expect("fail-start");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&tool);
    assert_eq!(code(&first), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(lines(&first.servers), ["failed start_failed restart"]);
    std::fs::remove_file(setup.dir.path().join("fail-start")).expect("fail-start");
    let second = setup.run(&tool);
    assert!(second.error.is_none(), "{:?}", second.error);
    assert_eq!(lines(&second.servers), ["ready"]);
    assert_eq!(initializes(setup.dir.path()), 1, "the restart answered");
    // The restarted server keeps serving, with no more records.
    let third = setup.run(&tool);
    assert!(third.error.is_none());
    assert!(third.servers.is_empty());
    assert_eq!(initializes(setup.dir.path()), 1);
    setup.stop(started.servers);
}

#[test]
fn a_failed_restart_leaves_the_server_dead_without_another_spawn() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    setup.populate(setup.spec("fx"));
    std::fs::write(setup.dir.path().join("fail-start"), "").expect("fail-start");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&tool);
    assert_eq!(lines(&first.servers), ["failed start_failed restart"]);
    setup.forget_spawn();
    let second = setup.run(&tool);
    assert_eq!(code(&second), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(lines(&second.servers), ["failed start_failed final"]);
    assert!(setup.spawned(), "the second call tried the restart");
    setup.forget_spawn();
    std::fs::remove_file(setup.dir.path().join("fail-start")).expect("fail-start");
    let third = setup.run(&tool);
    assert_eq!(code(&third), Some(ErrorCode::McpServerUnavailable));
    assert!(third.servers.is_empty());
    assert!(!setup.spawned(), "a dead server never spawns again");
    setup.stop(started.servers);
}

#[test]
fn a_server_killed_while_idle_restarts_once_then_stays_dead() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__fx__echo");
    setup.kill(&started);
    let first = setup.run(&tool);
    assert!(first.error.is_none(), "{:?}", first.error);
    assert_eq!(lines(&first.servers), ["failed died restart", "ready"]);
    let ServerRecord::Failed(died) = &first.servers[0] else {
        panic!("a failed record first");
    };
    assert_eq!(
        died.error.message,
        "The MCP server `fx` exited; Fiber restarts it on the next call.",
    );
    assert_eq!(initializes(setup.dir.path()), 2, "one start, one restart");
    setup.kill(&started);
    let second = setup.run(&tool);
    assert_eq!(code(&second), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(lines(&second.servers), ["failed died final"]);
    let message = "The MCP server `fx` exited again; its tools fail until the next session.";
    assert_eq!(
        second.error.as_ref().map(|e| e.message.as_str()),
        Some(message)
    );
    setup.forget_spawn();
    let third = setup.run(&tool);
    assert_eq!(code(&third), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(
        third.error.as_ref().map(|e| e.message.as_str()),
        Some(message)
    );
    assert!(third.servers.is_empty());
    assert!(!setup.spawned(), "a dead server never spawns again");
    assert_eq!(initializes(setup.dir.path()), 2);
    setup.stop(started.servers);
}

#[test]
fn a_death_mid_call_fails_the_call_and_the_next_call_restarts() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "die"}, {"name": "echo"}]));
    setup.result("die", "exit");
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let die = setup.tool(&started, "mcp__fx__die");
    let echo = setup.tool(&started, "mcp__fx__echo");
    let first = setup.run(&die);
    assert_eq!(code(&first), Some(ErrorCode::McpServerUnavailable));
    assert_eq!(
        first.error.as_ref().map(|e| e.message.as_str()),
        Some("The MCP server `fx` exited; Fiber restarts it on the next call."),
    );
    assert_eq!(lines(&first.servers), ["failed died restart"]);
    let second = setup.run(&echo);
    assert!(second.error.is_none(), "{:?}", second.error);
    assert_eq!(lines(&second.servers), ["ready"]);
    // The restart was the last: a second death mid-call is final.
    let third = setup.run(&die);
    assert_eq!(lines(&third.servers), ["failed died final"]);
    let fourth = setup.run(&echo);
    assert_eq!(code(&fourth), Some(ErrorCode::McpServerUnavailable));
    assert!(fourth.servers.is_empty());
    assert_eq!(initializes(setup.dir.path()), 2);
    setup.stop(started.servers);
}

#[test]
fn concurrent_calls_that_see_one_death_record_it_once() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let started = setup.start(vec![setup.spec("fx")]);
    let tool = setup.tool(&started, "mcp__fx__hang");
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_CALL_TIMEOUT)
        .expect("deadline");
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
    assert!(
        setup.fake.await_parked_count(deadline, 2, WITHIN),
        "both calls wait on the server",
    );
    fakes::kill_pid(setup.pid(), "KILL").expect("the server dies");
    let mut outputs = Vec::new();
    for _ in 0..2 {
        outputs.push(
            results
                .recv_timeout(WITHIN)
                .unwrap_or_else(|_| panic!("each call ends within {WITHIN:?}")),
        );
    }
    for output in &outputs {
        assert_eq!(code(output), Some(ErrorCode::McpServerUnavailable));
    }
    let recorded: Vec<String> = outputs
        .iter()
        .flat_map(|output| lines(&output.servers))
        .collect();
    assert_eq!(recorded, ["failed died restart"], "one death, one record");
    setup.stop(started.servers);
}

#[test]
fn nothing_spawns_after_a_stop_from_a_dead_or_running_slot() {
    // Stopped after a death, before the restart.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "die"}, {"name": "echo"}]));
    setup.result("die", "exit");
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let die = setup.tool(&started, "mcp__fx__die");
    let echo = setup.tool(&started, "mcp__fx__echo");
    assert_eq!(lines(&setup.run(&die).servers), ["failed died restart"]);
    setup.stop(started.servers);
    setup.forget_spawn();
    let after = setup.run(&echo);
    assert_eq!(code(&after), Some(ErrorCode::McpServerUnavailable));
    assert!(after.servers.is_empty());
    assert!(!setup.spawned(), "a stopped slot never restarts");
    // Stopped while running.
    let started = setup.start(vec![setup.spec("fx")]);
    let echo = setup.tool(&started, "mcp__fx__echo");
    assert!(setup.run(&echo).error.is_none());
    setup.stop(started.servers);
    setup.forget_spawn();
    let after = setup.run(&echo);
    assert_eq!(code(&after), Some(ErrorCode::McpServerUnavailable));
    assert!(after.servers.is_empty());
    assert!(!setup.spawned(), "a stopped slot never restarts");
}

#[test]
fn a_restart_listing_the_same_tools_leaves_the_cache_untouched() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let before = setup.cache_inode();
    setup.kill(&started);
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert_eq!(lines(&output.servers), ["failed died restart", "ready"]);
    assert_eq!(
        setup.cache_inode(),
        before,
        "an equal list rewrites nothing"
    );
    setup.stop(started.servers);
}

#[test]
fn a_restart_listing_other_tools_updates_the_cache_and_keeps_the_declarations() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}, {"name": "gone"}]));
    setup.result("echo", HI);
    setup.result("gone", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    setup.kill(&started);
    setup.tools(&json!([{"name": "echo"}, {"name": "added"}]));
    let gone = setup.tool(&started, "mcp__fx__gone");
    let output = setup.run(&gone);
    assert_eq!(code(&output), Some(ErrorCode::McpToolRemoved));
    assert_eq!(
        lines(&output.servers),
        ["failed died restart", "ready"],
        "a removed tool still carries the restart's records",
    );
    assert_eq!(setup.cached_names(), ["echo", "added"]);
    let names: Vec<String> = started
        .tools
        .iter()
        .map(|(_, tool)| tool.definition().name)
        .collect();
    assert_eq!(names, ["mcp__fx__echo", "mcp__fx__gone"]);
    let echo = setup.tool(&started, "mcp__fx__echo");
    let again = setup.run(&echo);
    assert!(again.error.is_none());
    assert!(again.servers.is_empty());
    setup.stop(started.servers);
}

#[test]
fn run_rejects_its_server_after_a_concurrent_restart_replaces_it() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = Arc::clone(&started.servers.slots[0]);
    let old_pid = setup.pid();
    let (served, got_served) = std::sync::mpsc::channel();
    let (resume, wait_to_resume) = std::sync::mpsc::channel();
    *super::lock(&slot.run_after_serve) = Some(Box::new(move || {
        served
            .send(())
            .expect("the call reached the live-server check");
        wait_to_resume
            .recv_timeout(WITHIN)
            .expect("the concurrent restart finishes within the wall-clock limit");
    }));
    let (done, result) = std::sync::mpsc::channel();
    let calling = Arc::clone(&slot);
    std::thread::spawn(move || {
        done.send(calling.run("echo")).expect("collected");
    });
    got_served
        .recv_timeout(WITHIN)
        .expect("run served the original server within the wall-clock limit");

    setup.kill(&started);
    setup.tools(&json!([{"name": "replacement"}]));
    let restarted = setup.serve_slot(&slot);
    let new_pid = setup.pid();
    resume.send(()).expect("release the waiting call");
    let outcome = result
        .recv_timeout(WITHIN)
        .expect("the call ends within the wall-clock limit");

    assert_ne!(
        old_pid, new_pid,
        "the concurrent restart replaced the child"
    );
    let Served::Up(_, records) = restarted else {
        panic!("the concurrent restart brings the server back");
    };
    assert_eq!(lines(&records), ["failed died restart", "ready"]);
    let Run::Failed(failed) = outcome else {
        panic!("a call using the replaced server fails as unavailable");
    };
    assert_eq!(failed.error.code, ErrorCode::McpServerUnavailable);
    assert!(
        failed.records.is_empty(),
        "the restart's records belong to its call"
    );
    setup.stop(started.servers);
}

#[test]
fn a_late_death_of_a_replaced_server_records_nothing() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    let started = setup.start(vec![setup.spec("fx")]);
    let slot = Arc::clone(&started.servers.slots[0]);
    let first = {
        let state = super::lock(&slot.state);
        let State::Running { server, .. } = &*state else {
            panic!("the session started the server");
        };
        Arc::clone(server)
    };
    setup.kill(&started);
    let tool = setup.tool(&started, "mcp__fx__echo");
    assert_eq!(
        lines(&setup.run(&tool).servers),
        ["failed died restart", "ready"]
    );
    // A call still on the first server sees it gone only now: the slot
    // already runs its replacement, so nothing is recorded or changed.
    assert!(slot.died(&first).is_none());
    let after = setup.run(&tool);
    assert!(after.error.is_none(), "{:?}", after.error);
    assert!(after.servers.is_empty());
    setup.stop(started.servers);
}

#[test]
fn a_changed_prompt_list_rewrites_the_cache() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", HI);
    setup.populate(setup.spec("fx"));
    assert_eq!(setup.cached_prompts(), Vec::<Value>::new());
    // Only the prompt list changes: the tools are untouched.
    std::fs::write(
        setup.dir.path().join("prompts.json"),
        json!([{"name": "greet", "description": "Greets."}]).to_string(),
    )
    .expect("prompts.json");
    let started = setup.start(vec![setup.spec("fx")]);
    assert!(started.failed.is_empty());
    let tool = setup.tool(&started, "mcp__fx__echo");
    let output = setup.run(&tool);
    assert!(output.error.is_none());
    assert_eq!(
        setup.cached_prompts(),
        json!([{"name": "greet", "description": "Greets."}])
            .as_array()
            .cloned()
            .unwrap_or_default()
    );
    assert_eq!(setup.cached_names(), ["echo"]);
    setup.stop(started.servers);
}
