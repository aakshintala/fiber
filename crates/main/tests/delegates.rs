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
use std::sync::mpsc;
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
