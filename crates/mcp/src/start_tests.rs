//! Parallel start, filtering, overrides and failures, through the
//! public [`start`].

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use contract::ErrorCode;
use contract::clock::Clock;
use contract::events::ServerFailure;
use contract::events::ToolSource;
use contract::shapes::Effect;
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{DEFAULT_CALL_TIMEOUT, DEFAULT_STARTUP_TIMEOUT, ServerSpec, start};
use crate::effects::Hints;

/// How long a test waits for a thread or a child, in real time.
///
/// The largest round value that keeps every test's serial deadlines within
/// half of nextest's 120 s kill: the worst mcp test,
/// `server::tests::cancel_ends_the_wait_and_sends_cancelled`, can exhaust five
/// (5 x 10 s = 50 s <= 60 s). A passing run never waits on it; it only
/// bounds a hang.
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
            dir: TempDir::new("fiber-mcp-start"),
            fake: FakeClock::new(),
        }
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.fake.clone()
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

    fn workspace(&self) -> std::path::PathBuf {
        self.dir.path().to_path_buf()
    }
}

fn names(started: &super::Started) -> Vec<String> {
    started
        .tools
        .iter()
        .map(|(_, tool)| tool.definition().name)
        .collect()
}

fn start_within(
    specs: Vec<ServerSpec>,
    workspace: std::path::PathBuf,
    clock: Arc<dyn Clock>,
) -> super::Started {
    // Threaded with a wall-clock limit: a silent or missing server would
    // sit parked on the fake clock forever, so a bare direct start would
    // hang the test instead of failing it.
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let cache = workspace.join("cache");
        let started = start(specs, &workspace, &cache, &clock, "0.0.0");
        done.send(started).expect("collected");
    });
    result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the start ends within {WITHIN:?}"))
}

#[test]
fn tools_of_two_servers_declare_in_one_sorted_order() {
    let first = Setup::new();
    first.tools(&json!([{"name": "zeta"}]));
    first.result("zeta", r#"{"content":[]}"#);
    let second = Setup::new();
    second.tools(&json!([{"name": "alpha"}]));
    second.result("alpha", r#"{"content":[]}"#);
    let specs = vec![first.spec("one"), second.spec("two")];
    let started = start_within(specs, first.workspace(), first.clock());
    assert!(started.failed.is_empty());
    assert_eq!(names(&started), ["mcp__one__zeta", "mcp__two__alpha"]);
    for info in &started.infos {
        assert!(matches!(info.source, ToolSource::Mcp { .. }));
    }
    started.servers.stop();
}

#[test]
fn enabled_declares_only_those_tools() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "keep"}, {"name": "drop"}]));
    setup.result("keep", r#"{"content":[]}"#);
    setup.result("drop", r#"{"content":[]}"#);
    let mut spec = setup.spec("fx");
    spec.enabled = Some(vec!["keep".to_owned()]);
    let started = start_within(vec![spec], setup.workspace(), setup.clock());
    assert_eq!(names(&started), ["mcp__fx__keep"]);
    started.servers.stop();
}

#[test]
fn disabled_removes_those_tools() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "keep"}, {"name": "drop"}]));
    setup.result("keep", r#"{"content":[]}"#);
    setup.result("drop", r#"{"content":[]}"#);
    let mut spec = setup.spec("fx");
    spec.disabled = vec!["drop".to_owned()];
    let started = start_within(vec![spec], setup.workspace(), setup.clock());
    assert_eq!(names(&started), ["mcp__fx__keep"]);
    started.servers.stop();
}

#[test]
fn enabled_and_disabled_is_enabled_minus_disabled() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "a"}, {"name": "b"}]));
    setup.result("a", r#"{"content":[]}"#);
    setup.result("b", r#"{"content":[]}"#);
    let mut spec = setup.spec("fx");
    spec.enabled = Some(vec!["a".to_owned(), "b".to_owned()]);
    spec.disabled = vec!["b".to_owned()];
    let started = start_within(vec![spec], setup.workspace(), setup.clock());
    assert_eq!(names(&started), ["mcp__fx__a"]);
    started.servers.stop();
}

#[test]
fn the_persons_hints_replace_the_servers() {
    let setup = Setup::new();
    setup.tools(&json!([{
        "name": "wipe",
        "annotations": {"readOnlyHint": true},
    }]));
    setup.result("wipe", r#"{"content":[]}"#);
    let mut spec = setup.spec("fx");
    spec.hints.insert(
        "wipe".to_owned(),
        Hints {
            read_only: None,
            destructive: Some(false),
            open_world: None,
        },
    );
    let started = start_within(vec![spec], setup.workspace(), setup.clock());
    assert_eq!(names(&started), ["mcp__fx__wipe"]);
    let (_, tool) = started.tools.first().expect("one tool");
    let effects = tool.effects(&Default::default()).expect("classifiable");
    assert_eq!(
        effects.declared.effects,
        vec![Effect::Writes, Effect::Network]
    );
    assert!(effects.declared.reversible);
    started.servers.stop();
}

#[test]
fn the_schema_passes_through_with_sorted_keys() {
    let setup = Setup::new();
    setup.tools(&json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {
            "type": "object",
            "properties": {"zebra": {"type": "string"}, "apple": {"type": "string"}},
        },
    }]));
    setup.result("echo", r#"{"content":[]}"#);
    let started = start_within(vec![setup.spec("fx")], setup.workspace(), setup.clock());
    let (_, tool) = started.tools.first().expect("one tool");
    let definition = tool.definition();
    assert_eq!(definition.description, "Echoes.");
    assert_eq!(
        definition.input_schema,
        json!({
            "type": "object",
            "properties": {"apple": {"type": "string"}, "zebra": {"type": "string"}},
        }),
    );
    started.servers.stop();
}

#[test]
fn a_long_qualified_name_is_cut() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "tool"}]));
    setup.result("tool", r#"{"content":[]}"#);
    let server = "s".repeat(crate::name::MAX_NAME_LEN);
    let mut spec = setup.spec(&server);
    spec.name = server;
    let started = start_within(vec![spec], setup.workspace(), setup.clock());
    let name = names(&started).pop().expect("one tool");
    assert_eq!(name.chars().count(), crate::name::MAX_NAME_LEN);
    assert!(name.starts_with("mcp__ssss"));
    started.servers.stop();
}

#[test]
fn a_failing_command_leaves_the_other_servers_tools_declared() {
    let good = Setup::new();
    good.tools(&json!([{"name": "echo"}]));
    good.result("echo", r#"{"content":[]}"#);
    let bad = Setup::new();
    let mut failing = bad.spec("bad");
    failing.command = "/no/such/command".to_owned();
    failing.args = Vec::new();
    let started = start_within(
        vec![good.spec("good"), failing],
        good.workspace(),
        good.clock(),
    );
    assert_eq!(names(&started), ["mcp__good__echo"]);
    assert_eq!(started.failed.len(), 1);
    let failure = &started.failed[0];
    assert_eq!(failure.server, "bad");
    assert_eq!(failure.reason, ServerFailure::StartFailed);
    assert!(!failure.will_restart);
    assert_eq!(failure.error.code, ErrorCode::McpServerUnavailable);
    started.servers.stop();
}

#[test]
fn a_server_that_misses_its_deadline_is_left_out() {
    let setup = Setup::new();
    let mut spec = setup.spec("slow");
    spec.command = "/bin/sleep".to_owned();
    spec.args = vec!["30".to_owned()];
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    let workspace = setup.workspace();
    let cache = workspace.join("cache");
    let clock = setup.clock();
    let (done, result) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            done.send(start(vec![spec], &workspace, &cache, &clock, "0.0.0"))
                .expect("collected");
        });
        assert!(
            setup.fake.await_parked(deadline, WITHIN),
            "the start waits on the startup deadline",
        );
        setup.fake.advance(DEFAULT_STARTUP_TIMEOUT);
        let started = result.recv_timeout(WITHIN).expect("the start ends");
        assert!(started.tools.is_empty());
        assert_eq!(started.failed.len(), 1);
        assert_eq!(started.failed[0].reason, ServerFailure::Deadline);
        assert_eq!(
            started.failed[0].error.code,
            ErrorCode::McpServerUnavailable
        );
        started.servers.stop();
    });
}

#[test]
fn no_specs_starts_nothing() {
    let setup = Setup::new();
    let started = start_within(Vec::new(), setup.workspace(), setup.clock());
    assert!(started.tools.is_empty());
    assert!(started.infos.is_empty());
    assert!(started.failed.is_empty());
    started.servers.stop();
}

#[test]
fn stopping_all_servers_leaves_no_running_child() {
    // Without `Servers::stop` the fixture would stay alive in the test
    // process: poll its pid until the reap makes `kill -0` fail.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let started = start_within(vec![setup.spec("fx")], setup.workspace(), setup.clock());
    assert!(started.failed.is_empty());
    let pid: u32 = std::fs::read_to_string(setup.dir.path().join("pid.txt"))
        .expect("pid.txt")
        .trim()
        .parse()
        .expect("a pid");
    assert!(fakes::kill_pid(pid, "0").expect("probe"));
    started.servers.stop();
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
fn servers_stop_at_once() {
    // Each server ignores SIGTERM and outlives the end of its input, so
    // each stop waits out the grace: both are parked on the clock together.
    let setups = [Setup::new(), Setup::new()];
    let script = "trap '' TERM\n\"$1\" \"$2\"\nwhile :; do sleep 0.05; done\n";
    let specs = setups
        .iter()
        .enumerate()
        .map(|(index, setup)| {
            setup.tools(&json!([{"name": format!("tool{index}")}]));
            ServerSpec {
                command: "/bin/bash".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    script.to_owned(),
                    "lingering".to_owned(),
                    fakes::mcp_fixture().display().to_string(),
                    setup.dir.path().display().to_string(),
                ],
                ..setup.spec(&format!("s{index}"))
            }
        })
        .collect();
    let clock = setups[0].clock();
    let workspace = setups[0].workspace();
    let cache = workspace.join("cache");
    let (opened, started) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        opened
            .send(start(specs, &workspace, &cache, &clock, "0.0.0"))
            .expect("collected");
    });
    let started = started
        .recv_timeout(WITHIN)
        .expect("both servers start within 5s");
    assert_eq!(started.failed.len(), 0);
    let grace = setups[0].fake.now() + Duration::from_millis(800);
    let (done, stopped) = std::sync::mpsc::channel();
    let servers = started.servers;
    std::thread::spawn(move || {
        servers.stop();
        done.send(()).expect("collected");
    });
    assert!(
        setups[0].fake.await_parked_count(grace, 2, WITHIN),
        "both stops wait on the grace at once"
    );
    setups[0].fake.advance(Duration::from_millis(800));
    stopped.recv_timeout(WITHIN).expect("the stop returned");
}

fn stop_within(servers: super::Servers) {
    // Detached with a wall-clock limit: a lingering child would keep the
    // stop parked on the fake clock forever, so a bare direct stop, or a
    // join on its thread, would hang the test instead of failing it.
    let (done, stopped) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        servers.stop();
        done.send(()).expect("collected");
    });
    stopped
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the stop ends within {WITHIN:?}"));
}

#[test]
fn a_cached_server_declares_without_spawning() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo", "description": "Echoes."}]));
    setup.result("echo", r#"{"content":[]}"#);
    let first = start_within(vec![setup.spec("fx")], setup.workspace(), setup.clock());
    assert_eq!(names(&first), ["mcp__fx__echo"]);
    assert!(first.failed.is_empty());
    assert!(first.required_failed.is_none());
    assert!(
        setup.workspace().join("cache").join("fx.json").exists(),
        "the first start writes the cache",
    );
    stop_within(first.servers);
    std::fs::remove_file(setup.dir.path().join("pid.txt")).expect("pid.txt");
    // The second start shares the workspace, so it shares the cache: it
    // declares the same tools and infos and spawns nothing.
    let second = start_within(vec![setup.spec("fx")], setup.workspace(), setup.clock());
    assert_eq!(names(&second), ["mcp__fx__echo"]);
    assert_eq!(second.infos, first.infos);
    assert!(second.failed.is_empty());
    assert!(second.required_failed.is_none());
    assert!(
        !setup.dir.path().join("pid.txt").exists(),
        "a cached server spawns nothing before its first call",
    );
    stop_within(second.servers);
}

#[test]
fn a_changed_arg_misses_the_cache_and_spawns() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    let first = start_within(vec![setup.spec("fx")], setup.workspace(), setup.clock());
    assert_eq!(names(&first), ["mcp__fx__echo"]);
    stop_within(first.servers);
    std::fs::remove_file(setup.dir.path().join("pid.txt")).expect("pid.txt");
    let mut changed = setup.spec("fx");
    changed.args.push("changed".to_owned());
    let second = start_within(vec![changed], setup.workspace(), setup.clock());
    assert_eq!(names(&second), ["mcp__fx__echo"]);
    assert!(
        setup.dir.path().join("pid.txt").exists(),
        "a changed declaration misses the cache and spawns",
    );
    stop_within(second.servers);
}

#[test]
fn a_required_server_with_a_cache_starts_and_declares_live() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.result("echo", r#"{"content":[]}"#);
    let first = start_within(vec![setup.spec("fx")], setup.workspace(), setup.clock());
    assert_eq!(names(&first), ["mcp__fx__echo"]);
    stop_within(first.servers);
    std::fs::remove_file(setup.dir.path().join("pid.txt")).expect("pid.txt");
    // The live list changed since the cache was written: a required
    // server declares what the server lists now, not the cached list.
    setup.tools(&json!([{"name": "other"}]));
    setup.result("other", r#"{"content":[]}"#);
    let mut required = setup.spec("fx");
    required.required = true;
    let second = start_within(vec![required], setup.workspace(), setup.clock());
    assert_eq!(names(&second), ["mcp__fx__other"]);
    assert!(
        setup.dir.path().join("pid.txt").exists(),
        "a required server starts with the session even with a cache",
    );
    stop_within(second.servers);
}

#[test]
fn a_required_server_that_fails_to_start_yields_required_failed() {
    let setup = Setup::new();
    let mut failing = setup.spec("bad");
    failing.command = "/no/such/command".to_owned();
    failing.args = Vec::new();
    failing.required = true;
    let started = start_within(vec![failing], setup.workspace(), setup.clock());
    assert!(started.tools.is_empty());
    assert!(started.failed.is_empty());
    let failure = started.required_failed.as_ref().expect("required_failed");
    assert_eq!(failure.server, "bad");
    assert_eq!(failure.reason, ServerFailure::StartFailed);
    assert!(!failure.will_restart);
    assert_eq!(failure.error.code, ErrorCode::McpServerUnavailable);
    assert!(
        failure
            .error
            .message
            .starts_with("The required MCP server `bad` failed to start: "),
        "message: {}",
        failure.error.message,
    );
    assert!(
        failure
            .error
            .message
            .contains("Check its `command` and `args` under `mcp.servers` in your configuration.",),
        "message: {}",
        failure.error.message,
    );
    stop_within(started.servers);
}

#[test]
fn a_required_server_that_misses_its_deadline_yields_required_failed() {
    let setup = Setup::new();
    let mut spec = setup.spec("slow");
    spec.command = "/bin/sleep".to_owned();
    spec.args = vec!["30".to_owned()];
    spec.required = true;
    let deadline = setup
        .fake
        .now()
        .checked_add(DEFAULT_STARTUP_TIMEOUT)
        .expect("deadline");
    let workspace = setup.workspace();
    let cache = workspace.join("cache");
    let clock = setup.clock();
    // Detached, not scoped: a scope joins its thread even after a
    // `recv_timeout` panic, so a hung start would hang the test.
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        done.send(start(vec![spec], &workspace, &cache, &clock, "0.0.0"))
            .expect("collected");
    });
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the start waits on the startup deadline",
    );
    setup.fake.advance(DEFAULT_STARTUP_TIMEOUT);
    let started = result.recv_timeout(WITHIN).expect("the start ends");
    assert!(started.tools.is_empty());
    assert!(started.failed.is_empty());
    let failure = started.required_failed.as_ref().expect("required_failed");
    assert_eq!(failure.server, "slow");
    assert_eq!(failure.reason, ServerFailure::Deadline);
    assert_eq!(
        failure.error.message,
        "The required MCP server `slow` did not answer before its startup deadline of 5000 ms. \
         Raise `startup_timeout_ms` under `mcp.servers.slow` if it needs longer.",
    );
    stop_within(started.servers);
}

#[test]
fn a_session_start_failure_carries_what_to_do() {
    let setup = Setup::new();
    let mut failing = setup.spec("bad");
    failing.command = "/no/such/command".to_owned();
    failing.args = Vec::new();
    let started = start_within(vec![failing], setup.workspace(), setup.clock());
    assert_eq!(started.failed.len(), 1);
    let failure = &started.failed[0];
    assert!(
        failure
            .error
            .message
            .starts_with("The MCP server `bad` failed to start: "),
        "message: {}",
        failure.error.message,
    );
    assert!(
        failure
            .error
            .message
            .contains("Check its `command` and `args` under `mcp.servers` in your configuration.",),
        "message: {}",
        failure.error.message,
    );
    stop_within(started.servers);
}

#[test]
fn the_first_failing_required_server_in_spec_order_wins() {
    let setup = Setup::new();
    let mut first = setup.spec("first");
    first.command = "/no/such/command".to_owned();
    first.args = Vec::new();
    first.required = true;
    let mut second = setup.spec("second");
    second.command = "/no/such/command".to_owned();
    second.args = Vec::new();
    second.required = true;
    let started = start_within(vec![first, second], setup.workspace(), setup.clock());
    assert!(started.failed.is_empty());
    let failure = started.required_failed.as_ref().expect("required_failed");
    assert_eq!(failure.server, "first");
    stop_within(started.servers);
}
