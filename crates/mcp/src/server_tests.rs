//! The server against the fixture and a fake clock: every wait carries
//! a named deadline, and the clock advances only after `await_parked`
//! proves the caller is waiting on it.

use std::collections::BTreeMap;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::TempDir;
use fakes::clock::FakeClock;
use serde_json::{Value, json};

use super::{CallError, Server, StartError};

/// How long a test waits for a thread or a child, in real time.
const WITHIN: Duration = Duration::from_secs(5);

/// One real-time poll of a child's exit.
const POLL: Duration = Duration::from_millis(50);

struct Setup {
    dir: TempDir,
    fake: std::sync::Arc<FakeClock>,
}

impl Setup {
    fn tools(tools: &Value) -> Self {
        let dir = TempDir::new("fiber-mcp-server");
        write(&dir, "tools.json", &tools.to_string());
        Self {
            dir,
            fake: FakeClock::new(),
        }
    }

    fn clock(&self) -> std::sync::Arc<dyn Clock> {
        self.fake.clone()
    }

    fn result(&self, tool: &str, body: &str) {
        write(&self.dir, &format!("call-{tool}.json"), body);
    }

    fn start(&self, timeout: Duration) -> super::OpenServer {
        let script = fakes::mcp_fixture().display().to_string();
        let workspace = self.dir.path().to_path_buf();
        Server::start(
            &script,
            &[workspace.display().to_string()],
            &BTreeMap::new(),
            &workspace,
            &self.clock(),
            timeout,
            "0.0.0",
        )
        .expect("the fixture server starts")
    }

    fn pid(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("pid.txt"))
            .expect("pid.txt")
            .trim()
            .parse()
            .expect("a pid")
    }
}

fn write(dir: &TempDir, name: &str, content: &str) {
    let path = dir.path().join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture parent");
    }
    std::fs::write(path, content).expect("fixture file");
}

fn echo_tools() -> Value {
    json!([{
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
        "annotations": {"readOnlyHint": true},
    }])
}

#[test]
fn initialize_and_list_succeed() {
    let setup = Setup::tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let opened = setup.start(Duration::from_secs(5));
    assert_eq!(opened.tools.len(), 1);
    let tool = &opened.tools[0];
    assert_eq!(tool.name, "echo");
    assert_eq!(tool.description, "Echoes.");
    assert_eq!(tool.hints.read_only, Some(true));
    assert_eq!(tool.hints.destructive, None);
    opened.server.stop();
}

#[test]
fn a_call_round_trips() {
    let setup = Setup::tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    let opened = setup.start(Duration::from_secs(5));
    let answer = opened
        .server
        .call(
            "echo",
            &json!({"text": "hi"}),
            Duration::from_secs(30),
            &fakes::CancelToken::new(),
        )
        .expect("the call answers");
    assert_eq!(answer, json!({"content": [{"type": "text", "text": "hi"}]}));
    let log = std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
    assert!(log.contains(r#""method":"tools/call""#));
    assert!(log.contains(r#""name":"echo""#));
    opened.server.stop();
}

#[test]
fn two_concurrent_calls_resolve_by_id_out_of_order() {
    let tools = json!([{"name": "slow"}, {"name": "fast"}]);
    let setup = Setup::tools(&tools);
    setup.result("slow", r#"{"content":[{"type":"text","text":"slow"}]}"#);
    setup.result("fast", r#"{"content":[{"type":"text","text":"fast"}]}"#);
    write(&setup.dir, "delay-slow", "1");
    let opened = setup.start(Duration::from_secs(5));
    let server = std::sync::Arc::new(opened.server);
    let (done, results) = mpsc::channel();
    for tool in ["slow", "fast"] {
        let server = std::sync::Arc::clone(&server);
        let done = done.clone();
        thread::spawn(move || {
            let answer = server.call(
                tool,
                &json!({}),
                Duration::from_secs(30),
                &fakes::CancelToken::new(),
            );
            done.send((tool.to_owned(), answer)).expect("collected");
        });
    }
    drop(done);
    let mut seen = Vec::new();
    for _ in 0..2 {
        let (tool, answer): (String, Result<Value, CallError>) = results
            .recv_timeout(WITHIN)
            .expect("both calls answer within 5s");
        seen.push((tool, answer.expect("no call fails")));
    }
    seen.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        seen,
        [
            (
                "fast".to_owned(),
                json!({"content": [{"type": "text", "text": "fast"}]}),
            ),
            (
                "slow".to_owned(),
                json!({"content": [{"type": "text", "text": "slow"}]}),
            ),
        ],
    );
}

#[test]
fn a_hang_tool_times_out_only_after_the_clock_advances() {
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        let answer = opened
            .server
            .call("hang", &json!({}), timeout, &fakes::CancelToken::new());
        done.send(answer).expect("collected");
    });
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the caller waits on the call deadline within {WITHIN:?}",
    );
    setup.fake.advance(Duration::from_secs(59));
    assert!(
        result.recv_timeout(Duration::from_millis(100)).is_err(),
        "the call is still waiting a second before its deadline",
    );
    setup.fake.advance(Duration::from_secs(1));
    assert_eq!(
        result.recv_timeout(WITHIN).expect("the call ends"),
        Err(CallError::Timeout),
    );
}

#[test]
fn cancel_ends_the_wait_and_sends_cancelled() {
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
    let timeout = Duration::from_secs(60);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let cancel = fakes::CancelToken::new();
    let (done, result) = mpsc::channel();
    // Scoped: the server stays alive until the log below was read, so the
    // fixture cannot die under the assertion (`Server::drop` kills it).
    thread::scope(|scope| {
        scope.spawn(|| {
            let answer = opened.server.call("hang", &json!({}), timeout, &cancel);
            done.send(answer).expect("collected");
        });
        assert!(
            setup.fake.await_parked(deadline, WITHIN),
            "the caller waits on the call deadline within {WITHIN:?}",
        );
        cancel.cancel();
        assert_eq!(
            result.recv_timeout(WITHIN).expect("the call ends"),
            Err(CallError::Cancelled),
        );
        // The waiter sends `notifications/cancelled` before it answers,
        // but the fixture appends it when it reads it: poll the log.
        let (_held, tick) = mpsc::channel::<()>();
        for _ in 0..100 {
            let log =
                std::fs::read_to_string(setup.dir.path().join("requests.log")).expect("requests");
            if log.contains("notifications/cancelled") {
                return;
            }
            match tick.recv_timeout(POLL) {
                Ok(()) | Err(_) => {}
            }
        }
        panic!("waited 5s for notifications/cancelled in requests.log");
    });
}

#[test]
fn a_command_that_does_not_exist_fails_the_start() {
    let setup = Setup::tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    let error = Server::start(
        "/no/such/command",
        &[],
        &BTreeMap::new(),
        &workspace,
        &setup.clock(),
        Duration::from_secs(5),
        "0.0.0",
    )
    .err()
    .expect("an unknown command fails");
    assert!(matches!(error, StartError::StartFailed(_)));
}

#[test]
fn a_server_that_exits_at_once_fails_the_start() {
    let setup = Setup::tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    let error = Server::start(
        "/bin/true",
        &[],
        &BTreeMap::new(),
        &workspace,
        &setup.clock(),
        Duration::from_secs(5),
        "0.0.0",
    )
    .err()
    .expect("an instant exit fails");
    assert!(matches!(error, StartError::StartFailed(_)));
}

#[test]
fn a_server_that_misses_its_startup_deadline_is_left_out() {
    let setup = Setup::tools(&json!([]));
    let workspace = setup.dir.path().to_path_buf();
    let timeout = Duration::from_secs(5);
    let deadline = setup.fake.now().checked_add(timeout).expect("deadline");
    let clock = setup.clock();
    let (done, result) = mpsc::channel();
    thread::spawn(move || {
        // `sleep` answers nothing: only the deadline ends the start.
        let error = Server::start(
            "/bin/sleep",
            &["30".to_owned()],
            &BTreeMap::new(),
            &workspace,
            &clock,
            timeout,
            "0.0.0",
        )
        .err()
        .expect("a silent server misses its deadline");
        done.send(error).expect("collected");
    });
    assert!(
        setup.fake.await_parked(deadline, WITHIN),
        "the start waits on the startup deadline within {WITHIN:?}",
    );
    setup.fake.advance(timeout);
    assert_eq!(
        result.recv_timeout(WITHIN).expect("the start ends"),
        StartError::Deadline,
    );
}

#[test]
fn a_server_killed_mid_call_is_gone() {
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
    fakes::kill_pid(setup.pid(), "KILL").expect("the server dies");
    // The reader marks the server gone when EOF arrives, which races the
    // kill: retry short calls until one sees it, bounding the retries.
    let mut answer = Err(CallError::Timeout);
    for _ in 0..50 {
        let (done, result) = mpsc::channel();
        let server = &opened.server;
        thread::scope(|scope| {
            scope.spawn(|| {
                let call = server.call(
                    "hang",
                    &json!({}),
                    Duration::from_secs(1),
                    &fakes::CancelToken::new(),
                );
                done.send(call).expect("collected");
            });
            setup.fake.advance(Duration::from_secs(1));
            answer = result.recv_timeout(WITHIN).expect("the call ends");
        });
        if answer == Err(CallError::Gone) {
            break;
        }
    }
    assert_eq!(answer, Err(CallError::Gone));
}

#[test]
fn garbage_on_stdout_is_ignored() {
    let setup = Setup::tools(&echo_tools());
    setup.result("echo", r#"{"content":[{"type":"text","text":"hi"}]}"#);
    write(&setup.dir, "noise", "not json at all\n[1, 2, 3]\n");
    let opened = setup.start(Duration::from_secs(5));
    let answer = opened
        .server
        .call(
            "echo",
            &json!({"text": "hi"}),
            Duration::from_secs(30),
            &fakes::CancelToken::new(),
        )
        .expect("calls work past the garbage");
    assert_eq!(answer, json!({"content": [{"type": "text", "text": "hi"}]}));
    opened.server.stop();
}

#[test]
fn stop_leaves_no_running_child() {
    let setup = Setup::tools(&json!([{"name": "hang"}]));
    setup.result("hang", "hang");
    let opened = setup.start(Duration::from_secs(5));
    let pid = setup.pid();
    assert!(
        fakes::kill_pid(pid, "0").expect("probe"),
        "the server runs before the stop",
    );
    opened.server.stop();
    let (_held, probe) = mpsc::channel::<()>();
    for _ in 0..100 {
        if !fakes::kill_pid(pid, "0").expect("probe") {
            return;
        }
        match probe.recv_timeout(POLL) {
            Ok(()) | Err(_) => {}
        }
    }
    panic!("waited 5s for pid {pid} to exit after the stop");
}
