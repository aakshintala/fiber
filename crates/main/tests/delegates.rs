//! End-to-end tests of Fiber delegates (`docs/delegates.md`): a parent
//! session spawns a child `fiber session` through `delegate_spawn`, and the
//! child's end wakes the parent. The built `fiber` runs in its own process
//! group with its own `FIBER_HOME`. Parent and child use two fake models,
//! `pa/m` and `pb/m`, on two provider servers, so each side's requests read
//! back separately. The binary-level fixtures (`support::Setup`,
//! `support::Socket`, the streams and `support::run_to_exit`) are shared;
//! only the delegate shapes live here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "test helpers; a failure is the test's; a live test prints its outcome"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, SessionGuard};

/// Installs provider `name` with model `m` on `protocol` at `url`.
fn install_url(setup: &support::Setup, name: &str, url: &str, protocol: &str) {
    let source = setup.root.path().join(format!("src-{name}"));
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    support::write_json(
        &source.join(format!("providers/{name}.json")),
        &json!({
            "name": name,
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": protocol,
                "base_url": format!("{url}/v1"), "context_window": 100000}]
        }),
    );
    extensions::plan(
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
}

/// Installs the parent's `pa/m` and the child's `pb/m` on `protocol` at the
/// two base URLs, and configures the parent's model and reviewer.
fn providers_url(setup: &support::Setup, parent: &str, child: &str, protocol: &str) {
    install_url(setup, "pa", parent, protocol);
    install_url(setup, "pb", child, protocol);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"}}),
    );
}

/// Installs the parent's `pa/m` and the child's `pb/m` on `openai-responses`.
fn providers(setup: &support::Setup, parent: &ProviderServer, child: &ProviderServer) {
    providers_url(setup, &parent.url(), &child.url(), "openai-responses");
}

/// A standing allow for `delegate_spawn`: `always_reviewed` skips it, so
/// the reviewer is still asked.
fn standing_allow(setup: &support::Setup) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""})
        ),
    )
    .unwrap();
}

/// The session keeps serving instead of idling out: an exit proves the run
/// ended on its own.
fn slow_idle(setup: &support::Setup) {
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000}}),
    );
}

/// The sessions directory for a session in `workspace`.
fn sessions_in(home: &Path, workspace: &Path) -> PathBuf {
    let workspace = fs::canonicalize(workspace).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    home.join("projects").join(key).join("sessions")
}

/// The sessions directory for `fiber ask`: its workspace is the launch
/// directory, the test root.
fn ask_sessions(setup: &support::Setup) -> PathBuf {
    sessions_in(&setup.home(), setup.root.path())
}

/// Arms the watchdog for `setup`: every child holds the test root on its
/// command line, so a failing test leaves no child behind. First, before
/// any fallible step.
fn arm(setup: &support::Setup) -> SessionGuard {
    SessionGuard::arm(setup.deadline, &setup.root.path().to_string_lossy())
}

/// A finished `function_call` for `name` with `arguments`.
fn function_call(call_id: &str, name: &str, arguments: &Value) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": format!("fc_{call_id}"),
        "call_id": call_id,
        "name": name,
        "arguments": arguments.to_string()
    }})
}

/// An `openai-responses` stream answering `text`: what a reviewer verdict
/// and a delegate's final message read as.
fn text_reply(text: &str) -> Response {
    support::stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// A `delegate_spawn` call for `model`: the parent's first reply.
fn spawn_call(model: &str) -> Value {
    function_call(
        "call_delegate",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello", "model": model}),
    )
}

/// Reads `path` as one JSON value per line, skipping blank lines. A path
/// that does not exist yet reads as empty.
fn read_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The `tool_call_completed` line for the log's one call of `name`.
fn completed_for(lines: &[Value], name: &str) -> Value {
    let requested = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested" && line["payload"]["name"] == name)
        .unwrap_or_else(|| panic!("no {name} call in {:?}", kinds(lines)));
    let action = requested["action_id"].as_str().unwrap();
    lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == action)
        .unwrap()
        .clone()
}

/// The payload of the log's one `kind` line.
fn one(lines: &[Value], kind: &str) -> Value {
    let mut found = lines.iter().filter(|line| line["kind"] == kind);
    let line = found
        .next()
        .unwrap_or_else(|| panic!("no {kind} in {:?}", kinds(lines)));
    assert!(found.next().is_none(), "two {kind} lines");
    line["payload"].clone()
}

/// The log's kinds, for a failure.
fn kinds(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// Kills the process group on drop, waited out under cleanup time.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        support::kill_group_detached(self.0, "KILL");
    }
}

/// A running `fiber` process: its drained stdout lines, its stderr, and its
/// group, killed on drop. The watchdog is armed before spawning, so a
/// failing test leaves no child behind.
struct Running {
    child: Child,
    group: u32,
    _guard: KillGroup,
    _watchdog: Watchdog,
    lines: mpsc::Receiver<Value>,
    stderr: mpsc::Receiver<String>,
    deadline: Deadline,
}

impl Running {
    /// Starts `fiber session --id <id>` with `extra` appended, draining
    /// stdout on a thread and keeping stderr for a failure.
    fn start(setup: &support::Setup, id: &str, extra: &[&str]) -> Self {
        let workspace = setup.workspace();
        let mut args = vec!["session", "--id", id, "--workspace"];
        args.push(workspace.to_str().unwrap());
        args.extend(extra);
        Self::spawn(setup, &args)
    }

    /// Spawns `fiber` with `args`, draining stdout on a thread and keeping
    /// stderr for a failure.
    fn spawn(setup: &support::Setup, args: &[&str]) -> Self {
        let (running, _) = Self::spawn_stdin(setup, args, Stdio::null());
        running
    }

    /// As [`Running::spawn`], with `stdin`: the caller holds the returned
    /// stdin open, which a delegate reads as its lifeline.
    fn spawn_stdin(
        setup: &support::Setup,
        args: &[&str],
        stdin: Stdio,
    ) -> (Self, Option<std::process::ChildStdin>) {
        let mut command = setup.fiber(args);
        command.stdin(stdin);
        let mut child = command.spawn().unwrap();
        let stdin = child.stdin.take();
        let group = child.id();
        let guard = KillGroup(group);
        let watchdog = Watchdog::group(group);
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                match tx.send(serde_json::from_str(&line.unwrap()).unwrap()) {
                    Ok(()) => {}
                    Err(mpsc::SendError(_)) => break,
                }
            }
        });
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
        (
            Self {
                child,
                group,
                _guard: guard,
                _watchdog: watchdog,
                lines,
                stderr,
                deadline: setup.deadline,
            },
            stdin,
        )
    }

    /// Connects a client to the session's socket, on a thread bounded by
    /// the deadline.
    fn connect(&self, setup: &support::Setup, id: &str) -> support::Socket {
        support::Socket::connect(setup.deadline, &setup.session_socket(id))
    }

    /// Stdout lines until one completes `done`: the drain thread signals
    /// every line over the channel. One deadline bounds the whole wait:
    /// each receive takes what remains of the test's deadline, at most
    /// 5 s, so endless unrelated output cannot postpone expiry, and a
    /// quiet hang fails fast.
    fn wait_line(&mut self, what: &str, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let mut got = Vec::new();
        loop {
            // Capped, never reset: the remainder shrinks as the test runs.
            let left = self.deadline.left().min(Duration::from_secs(5));
            if left.is_zero() {
                panic!("waited until the deadline for {what}; got {got:?}")
            }
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    let stop = done(&line);
                    got.push(line);
                    if stop {
                        return got;
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited {left:?} with no line for {what}; got {got:?}")
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    panic!("stdout ended before {what}; got {got:?}")
                }
            }
        }
    }

    /// Waits for the process to exit: the wait runs on a thread under the
    /// deadline, and expiry kills the group and reaps it. Stands the
    /// watchdog down first: dropping it would kill the group. Every stdout
    /// line is read: the drain ends at end of file once the process is
    /// gone, so a disconnect means the output is whole.
    fn wait(self) -> (ExitStatus, Vec<Value>, String) {
        let watchdog = self._watchdog;
        watchdog.stand_down(self.deadline.cleanup());
        let mut out = Vec::new();
        while let Ok(line) = self.lines.try_recv() {
            out.push(line);
        }
        let (done, finished) = mpsc::channel();
        let mut child = self.child;
        let group = self.group;
        let deadline = self.deadline;
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match finished.recv_timeout(deadline.left()) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                support::expired(deadline, group, &finished, "the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session wait thread ended without a result")
            }
        };
        loop {
            match self.lines.recv_timeout(deadline.left()) {
                Ok(line) => out.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited for the session's last stdout lines: {out:?}")
                }
            }
        }
        let stderr = self.stderr.recv().unwrap_or_default();
        (status, out, stderr)
    }
}

/// Subscribes `client` full, for the session's later lines. A late
/// subscription replays the log first, so lines arrive until the accept.
fn subscribe(client: &support::Socket) {
    client.send(r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#);
    loop {
        let line = support::recv_reply(client, "the subscribe acknowledgement");
        if line["kind"] == "command_accepted" {
            assert_eq!(line["payload"]["command_id"], "c_sub", "{line}");
            return;
        }
    }
}

/// Sends `close` with `now` and reads its accept, past whatever the
/// subscription is still replaying.
fn close_now(client: &support::Socket) {
    client.send(r#"{"id":"c_close_now","command":"close","args":{"now":true}}"#);
    loop {
        let line = support::recv_reply(client, "the close accept");
        if line["kind"] == "command_accepted"
            && line["payload"].get("command_id") == Some(&json!("c_close_now"))
        {
            return;
        }
    }
}

/// The parent's log lines once `fiber ask` exits.
struct Ask {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl Ask {
    fn session_id(&self) -> &str {
        self.lines[0]["session_id"].as_str().unwrap()
    }
}

/// The session requests' bodies, in order: reviewer requests carry no
/// tools, so they never match.
fn session_bodies(requests: &[Request]) -> Vec<Vec<u8>> {
    requests
        .iter()
        .filter(|request| {
            String::from_utf8_lossy(&request.body).contains(r#""name":"delegate_spawn""#)
        })
        .map(|request| request.body.clone())
        .collect()
}

/// The job id the spawn receipt names: `Started delegate <id>.`.
fn job_id(text: &str) -> String {
    let marker = "Started delegate ";
    let start = text.find(marker).unwrap() + marker.len();
    text[start..]
        .split([' ', '.', '"', '\\'])
        .next()
        .unwrap()
        .to_owned()
}

/// Answers the parent's requests by their content: `first` answers the
/// first session request, `reviewer` every reviewer request, `after` the
/// request carrying the spawn receipt (built with its job id), and `rest`
/// every later one. Reviewer requests carry no tools, so they never match
/// the session check; a later session request replays the call the receipt
/// answers, so the call's marker names it.
fn answering(
    first: Response,
    reviewer: Response,
    call: &'static str,
    after: impl Fn(&str) -> Response + Send + Sync + 'static,
    rest: Response,
) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return reviewer.clone();
        }
        if !text.contains("Started delegate") {
            return first.clone();
        }
        if text.contains(call) {
            return rest.clone();
        }
        after(&job_id(&text))
    }
}

#[test]
fn a_delegate_runs_to_its_end_and_its_finish_wakes_the_parent() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let full = "x".repeat(20 * 1024);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        support::hello(),
        support::hello(),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([text_reply(&full)]).unwrap();
    // Held until the parent's ending-notice turn has ended: a finish that
    // lands earlier joins that turn at a step boundary, so the wake turn
    // carries the finish alone only when the delegate ends after it.
    child.hold();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let mut running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let mut ended = 0;
    let mut lines = running.wait_line("the ending notice's turn end", |line| {
        ended += usize::from(line["kind"] == "turn_completed");
        ended == 2
    });
    child.release();
    let (status, rest, stderr) = running.wait();
    lines.extend(rest);
    let ask = Ask {
        code: status.code(),
        lines,
        stderr,
    };

    assert_eq!(ask.code, Some(0), "stderr: {}", ask.stderr);
    // The reviewer was asked, despite the standing allow: the call, the
    // verdict, the receipt's turn, the ending notice and the wake.
    assert_eq!(parent.requests().len(), 5);
    assert_eq!(child.requests().len(), 1);
    let resolved = one(&ask.lines, "permission_resolved");
    assert_eq!(resolved["decision"], "allow");
    assert_eq!(resolved["decided_by"], "reviewer");
    // The receipt arrives in the same turn as the call.
    let completed = ask
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    let receipt = completed["payload"]["content"][0]["text"].as_str().unwrap();
    // One job, one delegate: the log holds each exactly once, in order.
    let kinds = kinds(&ask.lines);
    let at = |kind: &str| kinds.iter().position(|got| got == kind).unwrap();
    assert!(at("job_started") < at("delegate_started"));
    assert!(at("delegate_started") < at("delegate_finished"));
    assert!(at("delegate_finished") < at("job_completed"));
    let started = one(&ask.lines, "job_started");
    let job_id = started["job_id"].as_str().unwrap();
    assert!(receipt.contains(job_id), "{receipt}");
    assert!(
        receipt.contains("events.jsonl"),
        "the receipt names the delegate's log: {receipt}"
    );
    let delegate = one(&ask.lines, "delegate_started");
    assert_eq!(delegate["job_id"], job_id);
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    // The wake turn's input holds one jobs item naming the job, and no
    // message item. The prompt's turn and the ending notice come first.
    let turns: Vec<_> = ask
        .lines
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .collect();
    assert_eq!(turns.len(), 3);
    assert_eq!(
        turns[2]["payload"]["input"],
        json!([{"type": "jobs", "job_ids": [job_id]}])
    );
    // The child's session names its parent.
    let child_dir = ask_sessions(&setup).join(child_id);
    let child_lines = read_lines(&child_dir.join("events.jsonl"));
    let child_started = one(&child_lines, "session_started");
    assert_eq!(
        child_started["parent"],
        json!({"session_id": ask.session_id(), "delegate_id": job_id})
    );
    // The 20 KiB final message is cut to 16 KiB in the wake, with the full
    // text in the artifact the cut names.
    let finished = one(&ask.lines, "delegate_finished");
    assert_eq!(finished["job_id"], job_id);
    let text = finished["text"].as_str().unwrap();
    assert!(text.starts_with(&"x".repeat(16 * 1024)), "cut at 16 KiB");
    assert!(text.contains("4096 bytes cut"), "{text}");
    let artifact = finished["artifact"].as_str().unwrap();
    assert_eq!(
        fs::read(ask_sessions(&setup).join(ask.session_id()).join(artifact)).unwrap(),
        full.as_bytes()
    );
    let done = one(&ask.lines, "job_completed");
    assert_eq!(done["job_id"], job_id);
    assert_eq!(done["status"], "completed");
    let requests = parent.requests();
    let carrying = requests
        .iter()
        .filter(|request| String::from_utf8_lossy(&request.body).contains("4096 bytes cut"))
        .count();
    assert_eq!(carrying, 1, "one later request carries the cut text");
}

#[test]
fn a_jobs_wait_returns_the_delegate_final_message_with_no_notice_after() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let full = "the scan found three caches";
    let child = ProviderServer::start([text_reply(full)]).unwrap();
    // The wait names the job the receipt minted, which no script can know:
    // the server builds the wait call from the receipt it sees.
    let parent = ProviderServer::start_responding(answering(
        support::stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        "call_wait",
        |job| {
            support::stream(&[function_call(
                "call_wait",
                "jobs",
                &json!({"action": "wait", "job_id": job, "timeout_ms": 30000}),
            )])
        },
        support::hello(),
    ))
    .unwrap();
    providers_url(&setup, &parent.url(), &child.url(), "openai-responses");
    standing_allow(&setup);
    slow_idle(&setup);

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    let started = one(&lines, "job_started");
    let job_id = started["job_id"].as_str().unwrap().to_owned();
    // The wait ran with the minted id, and returned the final message.
    let bodies = session_bodies(&parent.requests());
    assert_eq!(bodies.len(), 3);
    // The wait call goes out on the request after the receipt: its history
    // replays the spawn call first, so the jobs call is the last one.
    let waiting: Value = serde_json::from_slice(&bodies[2]).unwrap();
    let calls: Vec<_> = waiting["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
        .collect();
    let call = calls.last().unwrap();
    assert_eq!(call["name"], "jobs");
    let arguments: Value = serde_json::from_str(call["arguments"].as_str().unwrap()).unwrap();
    assert_eq!(arguments["action"], "wait");
    assert_eq!(arguments["job_id"], job_id);
    let waited = completed_for(&lines, "jobs");
    assert_eq!(waited["payload"]["status"], "completed");
    let text = waited["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains(full), "{text}");
    // Claimed once, under the wait: neither line arrived as a notice.
    let action = waited["action_id"].as_str().unwrap();
    for kind in ["delegate_finished", "job_completed"] {
        let line = lines.iter().find(|line| line["kind"] == kind).unwrap();
        assert_eq!(line["payload"]["job_id"], job_id);
        assert_eq!(line["action_id"], action, "{kind} came as a notice");
    }
}

#[test]
fn a_jobs_stop_cancels_the_delegate_which_exits_143() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let child = ProviderServer::start([stall()]).unwrap();
    // The stop names the job the receipt minted, which no script can know.
    // Held until the child stalls: the stop then meets a running delegate,
    // whatever the scheduling.
    let parent = ProviderServer::start_responding(answering(
        support::stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        "call_stop",
        |job| {
            support::stream(&[function_call(
                "call_stop",
                "jobs",
                &json!({"action": "stop", "job_id": job}),
            )])
        },
        support::hello(),
    ))
    .unwrap();
    parent.hold();
    providers_url(&setup, &parent.url(), &child.url(), "openai-responses");
    standing_allow(&setup);
    slow_idle(&setup);

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    assert!(
        parent.await_requests(1, Duration::from_secs(5)),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, Duration::from_secs(5)),
        "the verdict was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(3, Duration::from_secs(5)),
        "the stop was requested"
    );
    assert!(
        child.await_requests(1, Duration::from_secs(5)),
        "the child stalled its model call"
    );
    parent.release();
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    let started = one(&lines, "job_started");
    let job_id = started["job_id"].as_str().unwrap();
    let stopped = completed_for(&lines, "jobs");
    assert_eq!(stopped["payload"]["status"], "completed");
    // The stop call reports the cancellation in its text: the job, not the
    // call, ends cancelled.
    let report = stopped["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(
        report.contains(&format!("Job {job_id} cancelled")),
        "{report}"
    );
    // The stop claims the end: the lines carry its action.
    let action = stopped["action_id"].as_str().unwrap();
    for kind in ["delegate_finished", "job_completed"] {
        let line = lines.iter().find(|line| line["kind"] == kind).unwrap();
        assert_eq!(line["payload"]["job_id"], job_id);
        assert_eq!(line["action_id"], action, "{kind} came as a notice");
    }
    let done = one(&lines, "job_completed");
    assert_eq!(done["status"], "cancelled");
    // The child shut down on SIGTERM: its end is in the parent's log, so
    // the exit line is already written when it is read.
    let delegate = one(&lines, "delegate_started");
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let exited = read_lines(&ask_sessions(&setup).join(child_id).join("events.jsonl"))
        .last()
        .unwrap()
        .clone();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 143);
}

/// A child that stalls mid-reply: the parent's stop, close or budget end is
/// what finishes it.
fn stall() -> Response {
    let prefix = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"Working\"}\n\n";
    Response::stall(200, prefix, prefix.len() + 100000).header("content-type", "text/event-stream")
}

#[test]
fn killing_the_parent_shuts_the_stalled_child_down_with_129() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([stall()]).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let id = doors::mint("s_");
    let mut running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    // The delegate runs its turn: the log names the run, and the child
    // stalled mid-reply.
    let started = running.wait_line("the delegate start", |line| {
        line["kind"] == "delegate_started"
    });
    let delegate = started.last().unwrap();
    let child_id = delegate["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        child.await_requests(1, Duration::from_secs(5)),
        "the child stalled its model call"
    );
    // Subscribed before the kill: the child's exit is awaited on this
    // socket, so a shutdown that beats a later subscription cannot hide
    // the exit line.
    let child_socket = support::Socket::connect(setup.deadline, &setup.session_socket(&child_id));
    subscribe(&child_socket);
    // The parent is gone however it died: SIGKILL to its group, which the
    // child left when it became its own leader.
    support::kill_group(setup.deadline, running.group, "KILL").unwrap();
    let (status, _, _) = running.wait();
    assert_eq!(status.code(), None);
    // End of file on the lifeline is a hangup: the child shuts down with
    // 129 within the bound.
    let lines = support::until(&child_socket, "the child's exit", |line| {
        line["kind"] == "fiber_exited"
    });
    let exited = lines.last().unwrap();
    assert_eq!(exited["payload"]["exit_code"], 129);
}

#[test]
fn a_close_with_now_stops_the_delegate_and_exits_0() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([stall()]).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let id = doors::mint("s_");
    let mut running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    let started = running.wait_line("the delegate start", |line| {
        line["kind"] == "delegate_started"
    });
    let delegate = started.last().unwrap();
    let job_id = delegate["payload"]["job_id"].as_str().unwrap().to_owned();
    let child_id = delegate["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        child.await_requests(1, Duration::from_secs(5)),
        "the child stalled its model call"
    );
    let client = running.connect(&setup, &id);
    subscribe(&client);
    close_now(&client);
    let _tail = support::until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    // The shutdown stops the delegate as a stop does: its finish, then its
    // cancelled end, each once. The end is in the log, so the child's exit
    // line is already written when it is read.
    let kinds = kinds(&out);
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| *kind == "delegate_finished")
            .count(),
        1
    );
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "job_completed").count(),
        1
    );
    let finished = one(&out, "delegate_finished");
    assert_eq!(finished["job_id"], job_id);
    let done = one(&out, "job_completed");
    assert_eq!(done["job_id"], job_id);
    assert_eq!(done["status"], "cancelled");
    let exited = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(&child_id)
            .join("events.jsonl"),
    )
    .last()
    .unwrap()
    .clone();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 143);
}

/// An `openai-completions` chunk calling `name` with `arguments`.
fn ccall(name: &str, arguments: &Value) -> Value {
    json!({"id": "resp_1", "object": "chat.completion.chunk", "choices": [{
        "index": 0,
        "delta": {"role": "assistant", "tool_calls": [{
            "id": "call_1", "type": "function",
            "function": {"name": name, "arguments": arguments.to_string()}}]},
        "finish_reason": "tool_calls"}]})
}

/// An `openai-completions` chunk answering `text`.
fn ctext(text: &str) -> Value {
    json!({"id": "resp_1", "object": "chat.completion.chunk", "choices": [{
        "index": 0, "delta": {"role": "assistant", "content": text},
        "finish_reason": "stop"}]})
}

/// An `openai-completions` usage chunk reporting `cost`: the vendor's own
/// figure for the reply.
fn cusage(cost: f64) -> Value {
    json!({"id": "resp_1", "choices": [], "usage": {"prompt_tokens": 15,
        "completion_tokens": 9, "total_tokens": 24, "cost": cost,
        "prompt_tokens_details": {"cached_tokens": 14}}})
}

/// An `openai-completions` stream of `chunks`, then done.
fn cstream(chunks: &[Value]) -> Response {
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    Response::stream(body)
}

#[test]
fn a_turn_that_hits_the_budget_stops_its_running_delegate() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    // The first reply's own figure costs nothing, so the reviewer still
    // sends and allows; the read's reply costs past the budget, so the send
    // after its result ends the turn. The read is reads-only, so no second
    // verdict is asked.
    fs::write(setup.workspace().join("note.txt"), "the note\n").unwrap();
    let parent = ProviderServer::start([
        cstream(&[
            ccall(
                "delegate_spawn",
                &json!({"description": "scan", "prompt": "say hello", "model": "fiber:pb/m"}),
            ),
            cusage(0.0),
        ]),
        cstream(&[ctext("allow"), cusage(0.0)]),
        cstream(&[ccall("read", &json!({"path": "note.txt"})), cusage(100.0)]),
        // The delegate's end wakes one more turn, unless the close below
        // lands first; an unused reply is harmless.
        cstream(&[ctext("done"), cusage(0.0)]),
    ])
    .unwrap();
    let child = ProviderServer::start([stall()]).unwrap();
    providers_url(&setup, &parent.url(), &child.url(), "openai-completions");
    standing_allow(&setup);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "budget": {"usd": 1.0}, "session": {"idle_exit_ms": 3600000}}),
    );

    // Held until the child stalls: the budget then ends the turn on a
    // running delegate, whatever the scheduling.
    parent.hold();
    let id = doors::mint("s_");
    let mut running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    assert!(
        parent.await_requests(1, Duration::from_secs(5)),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, Duration::from_secs(5)),
        "the verdict was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(3, Duration::from_secs(5)),
        "the read was requested"
    );
    assert!(
        child.await_requests(1, Duration::from_secs(5)),
        "the child stalled its model call"
    );
    parent.release();
    // The turn spent past the budget on its second reply, and ends failed.
    // The first turn is the budget end; the delegate's end may wake one
    // more while the session serves, so later turns are not pinned.
    let first = running.wait_line("the budget end", |line| line["kind"] == "turn_completed");
    let completed = first.last().unwrap();
    assert_eq!(completed["payload"]["outcome"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "budget_exceeded");
    // The delegate was running when the turn ended: with the session still
    // serving and no close sent, its finish and cancelled end follow.
    let second = running.wait_line("the delegate end", |line| line["kind"] == "job_completed");
    // The delegate started before the budget end, so it is in the first
    // batch; its finish arrives with the end.
    let delegate = one(&first, "delegate_started");
    let job_id = delegate["job_id"].as_str().unwrap();
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let finished = one(&second, "delegate_finished");
    assert_eq!(finished["job_id"], job_id);
    let done = one(&second, "job_completed");
    assert_eq!(done["job_id"], job_id);
    assert_eq!(done["status"], "cancelled");
    // The budget end stops the delegate as a stop does: its end is in the
    // log, so the child's exit line is already written when it is read.
    let exited = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(child_id)
            .join("events.jsonl"),
    )
    .last()
    .unwrap()
    .clone();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 143);
    let client = running.connect(&setup, &id);
    subscribe(&client);
    close_now(&client);
    let _tail = support::until_close(&client);
    drop(client);
    let (status, _, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
}

/// Declares the MCP server `db` in the repository file of `workspace`: a
/// server nobody approved.
fn declare_db(workspace: &Path, extra: &Value) {
    let mut entry = json!({"command": "/bin/echo"});
    for (key, value) in extra.as_object().unwrap() {
        entry[key] = value.clone();
    }
    support::write_json(
        &workspace.join(".fiber/config.json"),
        &json!({"mcp": {"servers": {"db": entry}}}),
    );
}

#[test]
fn a_delegate_skips_repository_code_nobody_approved() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        support::hello(),
        support::hello(),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([support::hello(), support::hello()]).unwrap();
    child.hold();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);
    // The workspace declares a server nobody approved: `fiber ask` runs
    // with the test root as its workspace.
    declare_db(setup.root.path(), &json!({}));

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    assert!(
        parent.await_requests(4, Duration::from_secs(5)),
        "the parent's ending-notice turn was requested"
    );
    child.release();
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    let done = one(&lines, "job_completed");
    assert_eq!(done["status"], "completed");
    // The delegate raised no offer and never loaded the server: its
    // preamble declares no tool of `db`.
    let delegate = one(&lines, "delegate_started");
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let child_lines = read_lines(&ask_sessions(&setup).join(child_id).join("events.jsonl"));
    assert!(
        child_lines
            .iter()
            .all(|line| line["kind"] != "repository_code_offered"),
        "an offer was raised"
    );
    let preamble = one(&child_lines, "preamble_built");
    let tools: Vec<_> = preamble["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(
        tools.iter().all(|name| !name.starts_with("mcp__db__")),
        "the unapproved server loaded: {tools:?}"
    );
    // The skip itself is ephemeral, as every notice is: it goes to the
    // delegate's stdout, which its parent drains. The same delegate mode
    // run directly shows it.
    let direct = doors::mint("s_");
    let job = format!("j_{}", "0".repeat(16));
    let parent_id = lines[0]["session_id"].as_str().unwrap();
    // stdin held open: it is the delegate's lifeline.
    let (direct_running, direct_stdin) = Running::spawn_stdin(
        &setup,
        &[
            "session",
            "--id",
            &direct,
            "--workspace",
            setup.root.path().to_str().unwrap(),
            "--model",
            "pb/m",
            "--prompt",
            "hi",
            "--parent",
            parent_id,
            "--delegate-id",
            &job,
        ],
        Stdio::piped(),
    );
    // Its run ended on its own: one turn and out, with no idle wait. The
    // exit line is decided before stdin drops.
    let mut direct_running = direct_running;
    let direct_got = direct_running.wait_line("the direct run's exit", |line| {
        line["kind"] == "fiber_exited"
    });
    drop(direct_stdin);
    let (direct_status, direct_lines, direct_stderr) = direct_running.wait();
    // Its run ended on its own: one turn and out, with no idle wait.
    assert_eq!(direct_status.code(), Some(0), "stderr: {direct_stderr}");
    let direct_all: Vec<&Value> = direct_got.iter().chain(&direct_lines).collect();
    let skipped = direct_all
        .iter()
        .find(|line| {
            line["kind"] == "notice" && line["payload"]["code"] == "repository_code_skipped"
        })
        .unwrap();
    assert!(
        skipped["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("`db`"),
        "{skipped}"
    );
    assert!(
        direct_all
            .iter()
            .all(|line| line["kind"] != "repository_code_offered"),
        "an offer was raised"
    );
}

#[test]
fn a_delegate_fails_on_a_required_server_nobody_approved() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        support::hello(),
        support::hello(),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([support::hello()]).unwrap();
    // Held while the repository file is written: the parent started
    // without it, and the child starts with it required.
    parent.hold();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    assert!(
        parent.await_requests(1, Duration::from_secs(5)),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, Duration::from_secs(5)),
        "the verdict was requested"
    );
    declare_db(setup.root.path(), &json!({"required": true}));
    parent.release();
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    // The child failed before any session line: the job ends failed with
    // the server's error, and its finish carries no text.
    let done = one(&lines, "job_completed");
    assert_eq!(done["status"], "failed");
    assert_eq!(done["error"]["code"], "mcp_server_unapproved");
    let finished = one(&lines, "delegate_finished");
    assert_eq!(finished["text"], "");
}

#[test]
fn a_delegate_spawn_with_an_unknown_model_fails_and_starts_nothing() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call("fiber:fake/none")]),
        text_reply("allow"),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([support::hello()]).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let output = support::run_to_exit(
        setup.deadline,
        "fiber ask",
        setup.fiber(&["ask", "scan the tree"]),
    );
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8(output.stderr).unwrap()
    );

    let failed = completed_for(&lines, "delegate_spawn");
    let error = failed["payload"]["error"].as_object().unwrap();
    assert_eq!(error["code"], "invalid_arguments");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("fiber:pa/m"), "{message}");
    assert!(message.contains("fiber:pb/m"), "{message}");
    // No child started: no delegate line, and the parent's is the only
    // session directory.
    assert!(
        lines.iter().all(|line| line["kind"] != "delegate_started"),
        "a delegate started"
    );
    let entries: Vec<_> = std::fs::read_dir(ask_sessions(&setup))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(entries.len(), 1);
    assert!(child.requests().is_empty());
}

// A client opens a running delegate through the hub (see #634): the tests
// below start a real parent and a real delegate against two fake providers
// and reach the delegate on its own hub connection. Every wait ends at the
// test's deadline and names what it waited for; no test sleeps or reads
// the clock.

/// The steering text sent to the delegate: unique to it, so its absence
/// elsewhere proves the parent forwarded nothing.
const MARK: &str = "steer-marker-7f3a9c01";

/// The delegate's durable kinds, in order, pinned from a real run: its
/// first reply completes, the steered step follows, then the turn ends.
const DELEGATE_DURABLE: &[&str] = &[
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "step_started",
    "steering_applied",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The parent's durable kinds, in order, pinned from a real run.
const PARENT_DURABLE: &[&str] = &[
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "usage_recorded",
    "permission_resolved",
    "tool_call_started",
    "job_started",
    "delegate_started",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
];

/// Starts the parent's provider on server A and the delegate's on server B,
/// and configures the parent's model and reviewer. Server B stays held, so
/// the delegate's turn is still open while the test steers it.
fn pair(setup: &support::Setup) -> (ProviderServer, ProviderServer) {
    let server_a = ProviderServer::start_with_fallback(
        [
            support::stream(&[spawn_call("fiber:pb/m")]),
            text_reply("allow"),
            support::hello(),
        ],
        support::hello(),
    )
    .unwrap();
    let server_b = ProviderServer::start([text_reply("first"), text_reply("after-steer")]).unwrap();
    server_b.hold();
    providers(setup, &server_a, &server_b);
    standing_allow(setup);
    slow_idle(setup);
    (server_a, server_b)
}

/// A parent and its delegate, opened through the hub: the setup both tests
/// below share. `lines` holds every line read on the main connection, in
/// order; each test attributes lines by their envelope `session_id`.
struct Opened {
    main: support::Socket,
    parent: String,
    delegate: String,
    job: String,
    server_a: ProviderServer,
    server_b: ProviderServer,
    lines: Vec<Value>,
}

/// Whether `line` is a session line for `id`.
fn is_session(line: &Value, id: &str) -> bool {
    line.get("session_id").and_then(Value::as_str) == Some(id)
}

/// Whether `line` answers command `id`.
fn is_answer(line: &Value, id: &str) -> bool {
    line.get("payload")
        .and_then(|payload| payload.get("command_id"))
        .and_then(Value::as_str)
        == Some(id)
}

/// Every line for `id`, in order.
fn for_session(lines: &[Value], id: &str) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| is_session(line, id))
        .cloned()
        .collect()
}

/// The kinds of the lines carrying `seq`: a line is durable exactly then.
fn durable_kinds(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.get("seq").is_some())
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// The acknowledgement of command `id`.
fn answered(lines: &[Value], id: &str) -> Value {
    lines
        .iter()
        .find(|line| is_answer(line, id))
        .unwrap_or_else(|| panic!("no answer for {id} in {:?}", kinds(lines)))
        .clone()
}

/// A client on the hub, through the already-running hub's own socket.
fn direct_client(setup: &support::Setup) -> support::Socket {
    let client = support::Socket::connect(setup.deadline, &setup.hub_socket());
    let hello = support::recv(&client, "the hub_hello");
    assert_eq!(hello["kind"], "hub_hello", "{hello}");
    client
}

/// Starts a parent through the hub, waits for it to spawn its delegate and
/// for its first turn to end, then subscribes to the delegate through the
/// hub. The delegate's id comes only from the parent's `delegate_started`.
/// Server B stays held, so the delegate's turn is still open on return.
fn open_delegate(
    setup: &support::Setup,
    hub: &Arc<Mutex<Option<support::HubProc>>>,
    server_a: ProviderServer,
    server_b: ProviderServer,
) -> Opened {
    // A feed client may have started the hub already; either way the main
    // client is a second connection with its own command ids.
    let main = if hub.lock().unwrap().is_some() {
        direct_client(setup)
    } else {
        support::connect_hub(setup, hub).0
    };
    let mut lines = Vec::new();
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let parent = support::start_session(&main, &workspace, "scan the tree");
    main.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{parent}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let mut batch = support::until(&main, "the parent subscribe acknowledgement", |line| {
        is_answer(line, "c_sub")
    });
    assert_eq!(
        batch.last().unwrap()["kind"],
        "command_accepted",
        "{batch:?}"
    );
    lines.append(&mut batch);
    let mut batch = support::until(&main, "the parent's delegate_started", |line| {
        line["kind"] == "delegate_started" && is_session(line, &parent)
    });
    let started = batch.last().unwrap();
    let delegate = started["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = started["payload"]["job_id"].as_str().unwrap().to_owned();
    lines.append(&mut batch);
    // The parent's turn ends on its own, so the delegate's finish always
    // wakes a turn of its own.
    let mut batch = support::until(&main, "the parent's first turn_completed", |line| {
        line["kind"] == "turn_completed" && is_session(line, &parent)
    });
    lines.append(&mut batch);
    assert!(
        server_b.await_requests(1, setup.deadline.left()),
        "the delegate's turn called the model"
    );
    // A `full` subscribe folds the whole log, so a late subscriber misses
    // nothing.
    main.send(&format!(
        "{{\"id\":\"c_dsub\",\"session_id\":\"{delegate}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let mut batch = support::until(&main, "the delegate's step_started", |line| {
        line["kind"] == "step_started" && is_session(line, &delegate)
    });
    let folded = for_session(&batch, &delegate);
    let folded_started = folded
        .iter()
        .find(|line| line["kind"] == "session_started")
        .unwrap_or_else(|| panic!("no session_started in {:?}", kinds(&folded)));
    assert_eq!(
        folded_started["payload"]["parent"],
        json!({"session_id": parent, "delegate_id": job}),
        "{folded_started}"
    );
    // The delegate's subscribe is acknowledged once the session answers;
    // later batches carry it, and the test asserts it with the steer.
    lines.append(&mut batch);
    Opened {
        main,
        parent,
        delegate,
        job,
        server_a,
        server_b,
        lines,
    }
}

/// A feed subscriber, reading to the hub's stop on its own thread: the
/// thread forwards every line after the acknowledgement as it arrives and
/// returns the whole collection once the hub closes the connection.
/// Connected before any session starts, so the collection covers the whole
/// run.
struct FeedWatch {
    each: mpsc::Receiver<Value>,
    handle: Option<thread::JoinHandle<Vec<Value>>>,
}

fn watch_feed(setup: &support::Setup, hub: &Arc<Mutex<Option<support::HubProc>>>) -> FeedWatch {
    let (client, _) = support::connect_hub(setup, hub);
    client.send(r#"{"id":"c_feed","command":"feed"}"#);
    let ack = support::recv_reply(&client, "the feed acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    assert_eq!(ack["payload"]["command_id"], "c_feed");
    let (each_tx, each) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut lines = Vec::new();
        while let Some(line) = client.next("the feed's next line", &lines) {
            each_tx.send(line.clone()).unwrap_or(());
            lines.push(line);
        }
        lines
    });
    FeedWatch {
        each,
        handle: Some(handle),
    }
}

impl FeedWatch {
    /// The whole collection, once the hub's stop closed the stream. The
    /// per-line channel is drained only as far as the test's waits need;
    /// the thread's return carries every line.
    fn collect(mut self) -> Vec<Value> {
        self.handle
            .take()
            .unwrap()
            .join()
            .expect("the feed reader thread")
    }
}

/// Drains the feed's forwarded lines until the parent's `session_status`
/// arrives. The feed tracks a session on its rescan, every 500 ms, so a
/// session that lives and dies between rescans never reaches it: this wait
/// keeps the parent alive across one, and proves the positive fact the
/// final collection asserts.
fn wait_feed_status(watch: &FeedWatch, setup: &support::Setup, parent: &str) {
    loop {
        let line = watch
            .each
            .recv_timeout(setup.deadline.left())
            .expect("the feed to report the parent before the deadline");
        if line["kind"] == "session_status" && is_session(&line, parent) {
            return;
        }
    }
}

/// Stops the hub in `slot` with SIGTERM, as a person stopping it would.
fn stop_hub(hub: &Arc<Mutex<Option<support::HubProc>>>) {
    let hub = hub.lock().unwrap().take().expect("the hub started");
    hub.kill("TERM");
    hub.wait();
}

#[test]
fn a_client_opens_a_running_delegate_through_the_hub_and_steers_it() {
    let setup = support::Setup::new();
    let guard = arm(&setup);
    let (server_a, server_b) = pair(&setup);
    let hub = Arc::new(Mutex::new(None));
    let feed = watch_feed(&setup, &hub);
    let Opened {
        main,
        parent,
        delegate,
        job,
        server_a,
        server_b,
        mut lines,
    } = open_delegate(&setup, &hub, server_a, server_b);
    // The steer goes to the delegate on its own connection, then a `tools`
    // probe on the same connection: the session's reader hands each line
    // to the inbox before it reads the next, so the probe's answer proves
    // the steer is queued. Nothing is released while the reply is held.
    main.send(&format!(
        "{{\"id\":\"c_steer\",\"session_id\":\"{delegate}\",\"command\":\"steer\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{MARK}\"}}]}}}}"
    ));
    main.send(&format!(
        "{{\"id\":\"c_queued\",\"session_id\":\"{delegate}\",\"command\":\"tools\"}}"
    ));
    let mut batch = support::until(
        &main,
        "the tools answer proving the steer is queued",
        |line| is_answer(line, "c_queued"),
    );
    assert_eq!(
        batch.last().unwrap()["kind"],
        "command_accepted",
        "{batch:?}"
    );
    lines.append(&mut batch);
    server_b.release_one();
    assert!(
        server_b.await_requests(2, setup.deadline.left()),
        "the delegate's second model call after the steer"
    );
    let bodies: Vec<String> = server_b
        .requests()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(
        !bodies[0].contains(MARK),
        "the first call predates the steer"
    );
    assert!(bodies[1].contains(MARK), "the steer joined the next call");
    server_b.release();
    let mut batch = support::until(&main, "the delegate's fiber_exited", |line| {
        line["kind"] == "fiber_exited" && is_session(line, &delegate)
    });
    lines.append(&mut batch);
    let delegate_lines = for_session(&lines, &delegate);
    assert_eq!(answered(&lines, "c_steer")["kind"], "command_accepted");
    assert_eq!(answered(&lines, "c_dsub")["kind"], "command_accepted");
    // After the first reply completes, the steered step follows: the queue
    // holding the steer, the next step, the applied steer, the emptied
    // queue, and the second reply's start. Other lines (the probe's own
    // answer, statuses) travel the same stream and are not pinned here;
    // the durable list below is the complete ordered one.
    let first_done = delegate_lines
        .iter()
        .position(|line| line["kind"] == "assistant_message_completed")
        .expect("the delegate's first completed message");
    let steered: Vec<&Value> = delegate_lines[first_done + 1..]
        .iter()
        .filter(|line| {
            matches!(
                line["kind"].as_str(),
                Some(
                    "steering_queue"
                        | "step_started"
                        | "steering_applied"
                        | "assistant_message_started"
                )
            )
        })
        .collect();
    let steered_kinds: Vec<&str> = steered
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        steered_kinds,
        [
            "steering_queue",
            "step_started",
            "steering_applied",
            "steering_queue",
            "assistant_message_started"
        ],
        "{steered_kinds:?}"
    );
    let after = steered;
    assert!(
        serde_json::to_string(&after[0]["payload"]["messages"])
            .unwrap()
            .contains(MARK),
        "{}",
        after[0]
    );
    assert!(
        after[3]["payload"]["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        serde_json::to_string(&after[2]["payload"]["content"])
            .unwrap()
            .contains(MARK),
        "{}",
        after[2]
    );
    assert_eq!(after[2]["payload"]["source"], "driver");
    assert_eq!(after[2]["payload"]["command_id"], "c_steer");
    let exited = delegate_lines
        .iter()
        .find(|line| line["kind"] == "fiber_exited")
        .expect("the delegate's exit");
    assert_eq!(exited["payload"]["exit_code"], 0);
    // The complete, ordered durable list, on the hub stream and in the
    // delegate's log: a duplicated, missing or reordered event fails.
    assert_eq!(durable_kinds(&delegate_lines), DELEGATE_DURABLE);
    let logged = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(&delegate)
            .join("events.jsonl"),
    );
    assert_eq!(durable_kinds(&logged), DELEGATE_DURABLE);
    // The parent's finish for the job carries the steered final message,
    // and its wake turn holds the job alone.
    let mut batch = support::until(&main, "the parent's job_completed", |line| {
        line["kind"] == "job_completed"
            && is_session(line, &parent)
            && line["payload"]["job_id"] == job.as_str()
    });
    lines.append(&mut batch);
    let parent_lines = for_session(&lines, &parent);
    let parent_kinds = kinds(&parent_lines);
    let at = |kind: &str| {
        parent_kinds
            .iter()
            .position(|got| got == kind)
            .unwrap_or_else(|| panic!("no {kind} in {parent_kinds:?}"))
    };
    assert!(at("job_started") < at("delegate_started"));
    assert!(at("delegate_started") < at("delegate_finished"));
    assert!(at("delegate_finished") < at("job_completed"));
    // The prompt's turn and the wake the delegate's finish starts: the
    // wake turn's input holds one jobs item naming the job, and no
    // message item.
    let turns: Vec<&Value> = parent_lines
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .collect();
    assert_eq!(turns.len(), 2, "{parent_kinds:?}");
    assert_eq!(
        turns[1]["payload"]["input"],
        json!([{"type": "jobs", "job_ids": [job]}])
    );
    assert!(
        parent_lines
            .iter()
            .all(|line| !line["kind"].as_str().unwrap().starts_with("steering_")),
        "the parent took no steering: {parent_kinds:?}"
    );
    let finished = parent_lines
        .iter()
        .find(|line| line["kind"] == "delegate_finished")
        .unwrap();
    assert_eq!(finished["payload"]["job_id"], job.as_str());
    assert_eq!(finished["payload"]["text"], "after-steer");
    assert_eq!(durable_kinds(&parent_lines), PARENT_DURABLE);
    // The parent idles now: wait for the feed to report it before closing
    // it, so the final collection holds its status.
    wait_feed_status(&feed, &setup, &parent);
    // The parent forwarded nothing: neither its model calls nor its log
    // hold the marker.
    for request in server_a.requests() {
        assert!(
            !String::from_utf8_lossy(&request.body).contains(MARK),
            "the parent sent the steer to its model"
        );
    }
    let parent_log = sessions_in(&setup.home(), &setup.workspace())
        .join(&parent)
        .join("events.jsonl");
    let parent_text = fs::read_to_string(&parent_log).unwrap();
    assert!(!parent_text.contains(MARK), "the parent logged the steer");
    assert!(
        !parent_text.contains("steering_applied"),
        "the parent applied a steer"
    );
    // A steer after the delegate's exit is refused: a delegate resumes
    // only through its parent. The hub starts no process for it.
    main.send(&format!(
        "{{\"id\":\"c_late\",\"session_id\":\"{delegate}\",\"command\":\"steer\",\"args\":{{\"content\":[{{\"type\":\"text\",\"text\":\"late\"}}]}}}}"
    ));
    let mut batch = support::until(&main, "the late steer's refusal", |line| {
        is_answer(line, "c_late")
    });
    let late = batch.last().unwrap();
    assert_eq!(late["kind"], "command_rejected", "{late}");
    assert_eq!(late["payload"]["code"], "session_not_found");
    assert_eq!(
        late["payload"]["message"],
        "A delegate resumes only through its parent."
    );
    lines.append(&mut batch);
    assert!(
        setup
            .hub_log()
            .lines()
            .all(|line| !(line.contains(&delegate) && line.contains("session_resumed"))),
        "the hub resumed the delegate"
    );
    // Closing the parent ends it; its sessions leave no process behind.
    main.send(&format!(
        "{{\"id\":\"c_pclose\",\"session_id\":\"{parent}\",\"command\":\"close\",\"args\":{{\"now\":false}}}}"
    ));
    let mut batch = support::until(&main, "the parent's fiber_exited", |line| {
        line["kind"] == "fiber_exited" && is_session(line, &parent)
    });
    lines.append(&mut batch);
    drop(main);
    drop(lines);
    guard.wait_gone();
    // The feed held the parent's status and never the delegate: the hub
    // knew the delegate, since the test opened it above, so the absence
    // is proved, not vacuous.
    stop_hub(&hub);
    let feed_lines = feed.collect();
    assert!(
        feed_lines
            .iter()
            .all(|line| !serde_json::to_string(line).unwrap().contains(&delegate)),
        "the feed named the delegate"
    );
    assert!(
        feed_lines
            .iter()
            .any(|line| line["kind"] == "session_status" && is_session(line, &parent)),
        "the feed held the parent: {:?}",
        kinds(&feed_lines)
    );
}

/// A fresh hub lists the parent and never the running delegate (see #634):
/// the delegate stays reachable on its own connection while neither the
/// `sessions` answer nor the feed names it. The `sessions` answer waits for
/// the hub's first scan to settle: each session the scan found has sent its
/// first status, or one rescan, 500 ms, has passed since the read, so a
/// session started since the read can still be missing.
#[test]
fn a_fresh_hub_lists_the_parent_and_never_the_running_delegate() {
    let setup = support::Setup::new();
    let guard = arm(&setup);
    let (server_a, server_b) = pair(&setup);
    let hub = Arc::new(Mutex::new(None));
    let opened = open_delegate(&setup, &hub, server_a, server_b);
    let Opened {
        main,
        parent,
        delegate,
        job,
        server_a: _server_a,
        server_b,
        lines,
    } = opened;
    drop(lines);
    // The hub's stop ends no session: the parent and the delegate keep
    // running on their own sockets.
    stop_hub(&hub);
    drop(main);
    let (fresh, _) = support::connect_hub(&setup, &hub);
    // The fresh hub knows the delegate: it opens on its own connection.
    fresh.send(&format!(
        "{{\"id\":\"c_dsum\",\"session_id\":\"{delegate}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"summary\"}}}}"
    ));
    let guard_lines = support::until(&fresh, "the delegate's session_status", |line| {
        line["kind"] == "session_status" && is_session(line, &delegate)
    });
    assert_eq!(answered(&guard_lines, "c_dsum")["kind"], "command_accepted");
    assert_eq!(
        guard_lines.last().unwrap()["payload"]["parent"],
        parent.as_str()
    );
    // The listing holds the parent and never the delegate. The fresh hub
    // tracks the parent on its rescan, which load can delay past one
    // listing's settle: ask again until the parent appears, each answer
    // naming no delegate. Every wait ends at the test's deadline.
    let mut n = 0;
    let answer = loop {
        n += 1;
        let id = format!("c_sess{n}");
        fresh.send(&format!(r#"{{"id":"{id}","command":"sessions"}}"#));
        let listing = support::until(&fresh, "the sessions answer", |line| is_answer(line, &id));
        let answer = answered(&listing, &id);
        assert_eq!(answer["kind"], "command_accepted", "{answer}");
        assert!(
            !serde_json::to_string(&answer).unwrap().contains(&delegate),
            "the listing named the delegate: {answer}"
        );
        let live = answer["payload"]["result"]["live"].as_array().unwrap();
        if live.iter().any(|row| row["session_id"] == parent.as_str()) {
            break answer;
        }
    };
    let live = answer["payload"]["result"]["live"].as_array().unwrap();
    let live_ids: Vec<&str> = live
        .iter()
        .map(|row| row["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(live_ids, [parent.as_str()]);
    // The feed collects to the fresh hub's stop.
    let feed = watch_feed(&setup, &hub);
    // The old subscriptions died with the old hub: subscribing again folds
    // the whole log, so this stream holds the parent's complete durable
    // list.
    fresh.send(&format!(
        "{{\"id\":\"c_psub2\",\"session_id\":\"{parent}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let mut parent_lines = support::until(&fresh, "the parent subscribe acknowledgement", |line| {
        is_answer(line, "c_psub2")
    });
    assert_eq!(
        parent_lines.last().unwrap()["kind"],
        "command_accepted",
        "{parent_lines:?}"
    );
    server_b.release();
    let mut batch = support::until(&fresh, "the parent's job_completed", |line| {
        line["kind"] == "job_completed"
            && is_session(line, &parent)
            && line["payload"]["job_id"] == job.as_str()
    });
    parent_lines.append(&mut batch);
    let folded = for_session(&parent_lines, &parent);
    let folded_kinds = kinds(&folded);
    let at = |kind: &str| {
        folded_kinds
            .iter()
            .position(|got| got == kind)
            .unwrap_or_else(|| panic!("no {kind} in {folded_kinds:?}"))
    };
    assert!(at("job_started") < at("delegate_started"));
    assert!(at("delegate_started") < at("delegate_finished"));
    assert!(at("delegate_finished") < at("job_completed"));
    assert!(
        folded
            .iter()
            .all(|line| !line["kind"].as_str().unwrap().starts_with("steering_")),
        "the parent took no steering: {folded_kinds:?}"
    );
    assert_eq!(durable_kinds(&folded), PARENT_DURABLE);
    // The parent idles now: wait for the feed to report it before closing
    // it, so the final collection holds its status.
    wait_feed_status(&feed, &setup, &parent);
    drop(fresh);
    // Closing the parent ends it; the feed's stop ends its stream.
    let direct = support::Socket::connect(setup.deadline, &setup.session_socket(&parent));
    support::close_session(&direct);
    drop(direct);
    guard.wait_gone();
    stop_hub(&hub);
    let feed_lines = feed.collect();
    // The snapshot and every later line list the parent and never the
    // delegate.
    // The snapshot and every later line list the parent and never the
    // delegate.
    assert!(
        feed_lines
            .iter()
            .any(|line| line["kind"] == "session_status" && is_session(line, &parent)),
        "the feed held the parent: {:?}",
        kinds(&feed_lines)
    );
    for line in &feed_lines {
        assert!(
            !serde_json::to_string(line).unwrap().contains(&delegate),
            "the feed named the delegate: {line}"
        );
        if line["kind"] == "session_status" {
            assert_eq!(line["session_id"], parent.as_str(), "{line}");
        }
        if line["kind"] == "session_left" {
            assert_eq!(line["payload"]["session_id"], parent.as_str(), "{line}");
        }
    }
}
