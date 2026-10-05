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

const WITHIN: Duration = Duration::from_secs(5);

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

#[test]
fn tools_of_two_servers_declare_in_one_sorted_order() {
    let first = Setup::new();
    first.tools(&json!([{"name": "zeta"}]));
    first.result("zeta", r#"{"content":[]}"#);
    let second = Setup::new();
    second.tools(&json!([{"name": "alpha"}]));
    second.result("alpha", r#"{"content":[]}"#);
    let specs = vec![first.spec("one"), second.spec("two")];
    let started = start(specs, &first.workspace(), &first.clock(), "0.0.0");
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
    let started = start(vec![spec], &setup.workspace(), &setup.clock(), "0.0.0");
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
    let started = start(vec![spec], &setup.workspace(), &setup.clock(), "0.0.0");
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
    let started = start(vec![spec], &setup.workspace(), &setup.clock(), "0.0.0");
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
    let started = start(vec![spec], &setup.workspace(), &setup.clock(), "0.0.0");
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
    let started = start(
        vec![setup.spec("fx")],
        &setup.workspace(),
        &setup.clock(),
        "0.0.0",
    );
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
    let server = "s".repeat(60);
    let mut spec = setup.spec(&server);
    spec.name = server;
    let started = start(vec![spec], &setup.workspace(), &setup.clock(), "0.0.0");
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
    let started = start(
        vec![good.spec("good"), failing],
        &good.workspace(),
        &good.clock(),
        "0.0.0",
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
    let clock = setup.clock();
    let (done, result) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            done.send(start(vec![spec], &workspace, &clock, "0.0.0"))
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
    let started = start(Vec::new(), &setup.workspace(), &setup.clock(), "0.0.0");
    assert!(started.tools.is_empty());
    assert!(started.infos.is_empty());
    assert!(started.failed.is_empty());
    started.servers.stop();
}
