//! The server's prompt list through the fixture and a fake clock: every
//! wait carries a named deadline, and the clock advances only after the
//! log proves the handshake is paging on it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use crate::server::{Server, StartError};

/// How long a test waits for a thread or a child, in real time.
const WITHIN: Duration = Duration::from_secs(10);

/// One real-time poll of a file or a child's exit.
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
            dir: TempDir::new("fiber-mcp-prompts"),
            fake: FakeClock::new(),
        }
    }

    fn clock(&self) -> Arc<dyn Clock> {
        self.fake.clone()
    }

    fn tools(&self, tools: &Value) {
        write(&self.dir, "tools.json", &tools.to_string());
    }

    fn prompts(&self, prompts: &str) {
        write(&self.dir, "prompts.json", prompts);
    }

    fn cursor_forever(&self) {
        write(&self.dir, "cursor-forever", "");
    }

    fn start_result(&self, timeout: Duration) -> Result<crate::server::OpenServer, StartError> {
        // Threaded with a wall-clock limit: an endlessly paging server
        // would sit looping on the fake clock forever, so a bare direct
        // start would hang the test instead of failing it.
        let script = fakes::mcp_fixture().display().to_string();
        let workspace = self.dir.path().to_path_buf();
        let arg = workspace.display().to_string();
        let clock = self.clock();
        let (done, result) = mpsc::channel();
        thread::spawn(move || {
            let outcome = Server::start(
                &script,
                &[arg],
                &BTreeMap::new(),
                &workspace,
                &clock,
                timeout,
                "0.0.0",
            );
            done.send(outcome).expect("collected");
        });
        result
            .recv_timeout(WITHIN)
            .unwrap_or_else(|_| panic!("the start ends within {WITHIN:?}"))
    }

    fn start(&self, timeout: Duration) -> crate::server::OpenServer {
        self.start_result(timeout)
            .expect("the fixture server starts")
    }

    fn requests(&self) -> String {
        std::fs::read_to_string(self.dir.path().join("requests.log")).unwrap_or_default()
    }

    /// Polls `requests.log` until it holds `count` lines naming `method`,
    /// at most `WITHIN`: a passing run sees them, and a silent server fails
    /// the test instead of hanging it.
    fn await_requests(&self, method: &str, count: usize) {
        let (_held, probe) = mpsc::channel::<()>();
        for _ in 0..POLLS {
            if self.requests().matches(method).count() >= count {
                return;
            }
            match probe.recv_timeout(POLL) {
                Ok(()) | Err(_) => {}
            }
        }
        panic!("waited {WITHIN:?} for {count} `{method}` lines");
    }

    fn pid(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("pid.txt"))
            .expect("pid.txt")
            .trim()
            .parse()
            .expect("a pid")
    }

    /// Polls until `kill -0` fails for `pid`, at most `WITHIN`: the
    /// handshake's failure path reaps the child it started.
    fn await_reaped(&self, pid: u32) {
        let (_held, probe) = mpsc::channel::<()>();
        for _ in 0..POLLS {
            if !fakes::kill_pid(pid, "0").expect("probe") {
                return;
            }
            match probe.recv_timeout(POLL) {
                Ok(()) | Err(_) => {}
            }
        }
        panic!("waited {WITHIN:?} for pid {pid} to be reaped");
    }
}

fn write(dir: &TempDir, name: &str, content: &str) {
    std::fs::write(dir.path().join(name), content).expect("fixture file");
}

fn greet_prompts() -> Value {
    json!([{
        "name": "greet",
        "description": "Greets someone.",
        "arguments": [{"name": "who", "required": true}, {"name": "tone"}],
    }])
}

#[test]
fn a_server_with_prompts_lists_them_at_start() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts(&greet_prompts().to_string());
    let open = setup.start(Duration::from_secs(5));
    assert_eq!(
        open.prompts,
        greet_prompts().as_array().cloned().unwrap_or_default()
    );
    assert_eq!(
        open.tools,
        json!([{"name": "echo"}])
            .as_array()
            .cloned()
            .unwrap_or_default()
    );
    let log = setup.requests();
    let initialized = log
        .find("notifications/initialized")
        .expect("the handshake notifies initialized");
    let listed = log
        .find(r#""method":"prompts/list""#)
        .expect("the handshake lists prompts");
    assert!(
        initialized < listed,
        "prompts/list runs after notifications/initialized",
    );
    open.server.stop();
}

#[test]
fn a_server_without_the_prompts_capability_is_never_asked_for_prompts() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    let open = setup.start(Duration::from_secs(5));
    assert!(open.prompts.is_empty());
    assert!(
        !setup.requests().contains(r#""method":"prompts/list""#),
        "no prompts/list without the capability",
    );
    open.server.stop();
}

#[test]
fn a_failing_prompt_list_leaves_the_server_started_with_no_prompts() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts("error");
    let open = setup.start(Duration::from_secs(5));
    assert!(open.prompts.is_empty());
    assert_eq!(open.tools.len(), 1, "the tools still list");
    open.server.stop();
}

#[test]
fn endless_pages_end_at_the_startup_deadline() {
    // `tools/list` pages first in one handshake, so the endless cursor
    // starts only once its pages are done: the first `prompts/list` line
    // proves tools paging finished.
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.prompts(&greet_prompts().to_string());
    let timeout = Duration::from_secs(5);
    let (done, result) = mpsc::channel();
    let script = fakes::mcp_fixture().display().to_string();
    let workspace = setup.dir.path().to_path_buf();
    let arg = workspace.display().to_string();
    let clock = setup.clock();
    thread::spawn(move || {
        let outcome = Server::start(
            &script,
            &[arg],
            &BTreeMap::new(),
            &workspace,
            &clock,
            timeout,
            "0.0.0",
        );
        done.send(outcome).expect("collected");
    });
    setup.await_requests(r#""method":"prompts/list""#, 1);
    setup.cursor_forever();
    setup.await_requests(r#""method":"prompts/list""#, 2);
    let pid = setup.pid();
    setup.fake.advance(timeout + Duration::from_millis(1));
    let outcome = result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the paging start ends within {WITHIN:?}"));
    assert!(
        matches!(outcome, Err(StartError::Deadline)),
        "endless prompt pages end at the startup deadline",
    );
    setup.await_reaped(pid);
}

#[test]
fn endless_tool_pages_end_at_the_startup_deadline() {
    let setup = Setup::new();
    setup.tools(&json!([{"name": "echo"}]));
    setup.cursor_forever();
    let timeout = Duration::from_secs(5);
    let (done, result) = mpsc::channel();
    let script = fakes::mcp_fixture().display().to_string();
    let workspace = setup.dir.path().to_path_buf();
    let arg = workspace.display().to_string();
    let clock = setup.clock();
    thread::spawn(move || {
        let outcome = Server::start(
            &script,
            &[arg],
            &BTreeMap::new(),
            &workspace,
            &clock,
            timeout,
            "0.0.0",
        );
        done.send(outcome).expect("collected");
    });
    setup.await_requests(r#""method":"tools/list""#, 2);
    let pid = setup.pid();
    setup.fake.advance(timeout + Duration::from_millis(1));
    let outcome = result
        .recv_timeout(WITHIN)
        .unwrap_or_else(|_| panic!("the paging start ends within {WITHIN:?}"));
    assert!(
        matches!(outcome, Err(StartError::Deadline)),
        "endless tool pages end at the startup deadline",
    );
    setup.await_reaped(pid);
}
