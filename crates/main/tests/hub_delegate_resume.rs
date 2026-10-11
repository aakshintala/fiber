//! A client subscribed `summary` to a delegate through the hub follows it
//! into its later run (`docs/invocation.md`, "Driver commands",
//! `subscribe` row): the delegate finishes, the parent resumes it, and the
//! client receives the later run's statuses without sending anything.
//!
//! The parent's resume is stood in for by the test: resuming a delegate is
//! untracked work, so the later run starts with the internal resume
//! command, exactly as a parent does for the first run. The hub path under
//! test is the same whoever starts the later run: the hub never resumes a
//! delegate, it only sees the socket come back.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::io::{BufRead, BufReader};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::{Deadline, ProviderServer, Watchdog};
use serde_json::{Value, json};

/// A running `fiber session` process: its drained stdout lines, killed on
/// drop through its guards.
struct Running {
    child: Option<Child>,
    _watchdog: Watchdog,
    lines: mpsc::Receiver<Value>,
    stderr: mpsc::Receiver<String>,
    deadline: support::Deadline,
}

impl Running {
    /// Starts `fiber session` with `args` and `stdin` in `dir`, draining
    /// stdout on a thread. The caller holds the returned stdin open.
    #[track_caller]
    fn spawn(
        setup: &support::Setup,
        args: &[&str],
        stdin: Stdio,
        dir: &std::path::Path,
    ) -> (Self, Option<std::process::ChildStdin>) {
        let mut command = setup.fiber(args);
        command.current_dir(dir);
        command.stdin(stdin);
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let watchdog = Watchdog::group(child.id());
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (err_tx, stderr) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            match std::io::Read::read_to_string(&mut BufReader::new(stderr_pipe), &mut text) {
                Ok(_) | Err(_) => {}
            }
            match err_tx.send(text) {
                Ok(()) | Err(mpsc::SendError(_)) => {}
            }
        });
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match tx.send(serde_json::from_str(&line.unwrap()).unwrap()) {
                    Ok(()) => {}
                    Err(mpsc::SendError(_)) => break,
                }
            }
        });
        (
            Self {
                child: Some(child),
                _watchdog: watchdog,
                lines,
                stderr,
                deadline: setup.deadline,
            },
            stdin,
        )
    }

    /// Stdout lines until one completes `done`.
    #[track_caller]
    fn wait_line(&mut self, what: &str, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let mut got = Vec::new();
        loop {
            let left = self.deadline.left().min(Duration::from_secs(5));
            if left.is_zero() {
                panic!(
                    "waited until the deadline for {what}; got {got:?}; stderr: {}",
                    self.stderr.try_recv().unwrap_or_default()
                )
            }
            match Deadline::after(left).recv(&self.lines) {
                Ok(line) => {
                    let stop = done(&line);
                    got.push(line);
                    if stop {
                        return got;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!(
                        "waited {left:?} with no line for {what}; got {got:?}; stderr: {}",
                        self.stderr.try_recv().unwrap_or_default()
                    )
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!(
                        "stdout ended before {what}; got {got:?}; stderr: {}",
                        self.stderr.try_recv().unwrap_or_default()
                    )
                }
            }
        }
    }

    /// Waits for the process to exit under the deadline.
    #[track_caller]
    fn wait_exit(mut self) {
        let mut child = self.child.take().unwrap();
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait()).unwrap());
        match self.deadline.recv(&finished) {
            Ok(status) => assert_eq!(status.unwrap().code(), Some(0)),
            Err(_) => panic!("waited until the deadline for the session to exit"),
        }
    }
}

fn wall_ms() -> u64 {
    support::SystemClock
        .wall()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// Connects to the session's socket once it runs: the caller waits for its
/// load line on stdout first, as its bind precedes it.
#[track_caller]
fn connect_session(setup: &support::Setup, id: &str, later: &mut Running) -> support::Socket {
    later.wait_line("the resumed run's load", |line| {
        line["kind"] == "extensions_loaded"
    });
    match later.child.as_mut().unwrap().try_wait() {
        Ok(Some(status)) => panic!("the resumed session exited early: {status:?}"),
        Ok(None) => {}
        Err(e) => panic!("try_wait failed: {e}"),
    }
    support::Socket::connect(setup.deadline, &setup.session_socket(id))
}

#[test]
fn a_summary_subscription_through_the_hub_follows_a_delegate_into_its_later_run() {
    let setup = support::Setup::new();
    let _sessions =
        support::SessionGuard::arm(setup.deadline, &setup.workspace().to_string_lossy());
    let server = ProviderServer::start([support::hello(), support::hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "session": {"idle_exit_ms": 3600000}}),
    );
    let hub_slot = Arc::new(Mutex::new(None));
    let (hub, _) = support::connect_hub(&setup, &hub_slot);

    let parent = doors::mint("s_");
    let delegate = doors::mint("s_");
    let job = format!("j_{}", "0".repeat(16));
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let workdir = setup.workspace();
    // The delegate's first run, exactly as a parent starts it, with stdin
    // held open as the lifeline.
    let (mut first, first_stdin) = Running::spawn(
        &setup,
        &[
            "session",
            "--id",
            &delegate,
            "--workspace",
            &workspace,
            "--model",
            "fake/m",
            "--prompt",
            "hi",
            "--parent",
            &parent,
            "--delegate-id",
            &job,
        ],
        Stdio::piped(),
        workdir.as_path(),
    );
    let _held_first = first_stdin;
    assert!(
        server.await_requests(1, Duration::from_secs(10)),
        "the first run called the model"
    );
    hub.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{delegate}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"summary\"}}}}"
    ));
    let ack = support::recv_reply(&hub, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted");
    assert_eq!(ack["payload"]["command_id"], "c_sub");
    loop {
        let line = support::recv_reply(&hub, "the first run's status");
        if line.get("kind").and_then(Value::as_str) == Some("session_status")
            && line.get("session_id").and_then(Value::as_str) == Some(delegate.as_str())
        {
            assert_eq!(line["payload"]["parent"], parent, "{line}");
            break;
        }
    }
    server.release_one();
    first.wait_line("the first run's exit", |line| {
        line["kind"] == "fiber_exited"
    });
    drop(_held_first);
    first.wait_exit();
    let t = wall_ms();
    // The later run, with the internal resume command. On the delegate's
    // own socket it is subscribed `full` and prompted; its model call
    // stays held.
    let (later, later_stdin) = Running::spawn(
        &setup,
        &[
            "session",
            "--id",
            &delegate,
            "--workspace",
            &workspace,
            "--resume",
        ],
        Stdio::piped(),
        workdir.as_path(),
    );
    let _held_later = later_stdin;
    let mut later = later;
    let direct = connect_session(&setup, &delegate, &mut later);
    direct.send(r#"{"id":"c_full","command":"subscribe","args":{"level":"full"}}"#);
    loop {
        let line = support::recv_reply(&direct, "the direct subscribe acknowledgement");
        if line["kind"] == "command_accepted"
            && line["payload"].get("command_id") == Some(&json!("c_full"))
        {
            break;
        }
    }
    direct.send(
        r#"{"id":"c_p1","command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}}"#,
    );
    // Without writing anything more to the hub socket, the client receives
    // the later run's streaming status: `since` tells it apart, since the
    // first run's statuses all began before `t`.
    let lines = support::until(&hub, "the later run's status", |line| {
        line.get("kind").and_then(Value::as_str) == Some("session_status")
            && line.get("session_id").and_then(Value::as_str) == Some(delegate.as_str())
            && line["payload"]["state"] == "streaming"
            && line["payload"]["since"]
                .as_u64()
                .is_some_and(|since| since >= t)
    });
    let status = lines.last().unwrap();
    assert_eq!(status["session_id"], delegate, "{status}");
    server.release();
    direct.send(r#"{"id":"c_close","command":"close","args":{"now":true}}"#);
    loop {
        let line = support::recv_reply(&direct, "the close accept");
        if line["kind"] == "command_accepted"
            && line["payload"].get("command_id") == Some(&json!("c_close"))
        {
            break;
        }
    }
    later.wait_exit();
    if let Some(proc) = hub_slot.lock().unwrap().take() {
        proc.kill_and_wait();
    }
}
