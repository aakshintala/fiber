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

mod extension_harness;
mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::thread;
use std::time::Duration;

use extension_harness::{send, until_session, until_session_close};
use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::pty::{Grid, Run};
use support::{Deadline, KillGroup, SessionGuard, function_call};

/// Installs provider `name` with model `m` on `protocol` at `url`, with
/// `extra` merged into the model's fields (such as `thinking_levels`).
fn install_url(
    setup: &support::Setup,
    name: &str,
    url: &str,
    protocol: &str,
    extra: &serde_json::Value,
) {
    let source = setup.root.path().join(format!("src-{name}"));
    let mut manifest = json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1});
    manifest["providers"] = Value::Object(
        [(name.to_owned(), json!([format!("{url}/v1")]))]
            .into_iter()
            .collect(),
    );
    support::write_json(&source.join("extension.json"), &manifest);
    let mut model = json!({"id": "m", "protocol": protocol, "base_url": format!("{url}/v1"), "context_window": 100000});
    for (key, value) in extra.as_object().unwrap() {
        model[key] = value.clone();
    }
    support::write_json(
        &source.join(format!("providers/{name}.json")),
        &json!({
            "name": name,
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [model]
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
    install_url(setup, "pa", parent, protocol, &json!({}));
    install_url(setup, "pb", child, protocol, &json!({}));
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"}}),
    );
}

/// Installs the parent's `pa/m` and the child's `pb/m` on `openai-responses`.
fn providers(setup: &support::Setup, parent: &ProviderServer, child: &ProviderServer) {
    providers_url(setup, &parent.url(), &child.url(), "openai-responses");
}

/// A standing allow for `delegate_spawn`, `delegate_fork` and
/// `delegate_message`: `always_reviewed` skips it, so the reviewer is
/// still asked.
fn standing_allow(setup: &support::Setup) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n{}\n{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""}),
            json!({"decision": "allow", "tool": "delegate_fork", "prefix": ""}),
            json!({"decision": "allow", "tool": "delegate_message", "prefix": ""})
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

/// An `openai-responses` stream answering `text`: what a reviewer verdict
/// and a delegate's final message read as.
fn text_reply(text: &str) -> Response {
    support::stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

/// A `delegate_fork` call with `prompt`: the parent's first reply.
fn fork_call(prompt: &str) -> Value {
    function_call(
        "call_fork",
        "delegate_fork",
        &json!({"description": "scan", "prompt": prompt}),
    )
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
        .unwrap_or_else(|| panic!("no {name} call in {:?}", owned_kinds(lines)));
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
        .unwrap_or_else(|| panic!("no {kind} in {:?}", owned_kinds(lines)));
    assert!(found.next().is_none(), "two {kind} lines");
    line["payload"].clone()
}

/// The log's kinds as owned strings, for a failure: the log keeps
/// `session_status`, which [`support::kinds`] filters out.
fn owned_kinds(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap().to_owned())
        .collect()
}

/// The stream's kinds in order, setting aside only the lines
/// `docs/testing.md` ("Event streams") leaves unpositioned: `clients`,
/// `session_status`, the hub's `attention` and `date_changed`.
fn ordered_kinds(lines: &[Value]) -> Vec<String> {
    support::kinds(lines)
        .into_iter()
        .map(str::to_owned)
        .collect()
}

#[test]
fn ordered_projections_set_aside_exactly_the_independent_kinds() {
    let lines = support::expected(&[
        "clients",
        "turn_started",
        "session_status",
        "usage_recorded",
        "attention",
        "command_accepted",
        "date_changed",
        "turn_completed",
        "job_delta",
        "unknown_future_kind",
    ]);
    let expected = [
        "turn_started",
        "usage_recorded",
        "command_accepted",
        "turn_completed",
        "job_delta",
        "unknown_future_kind",
    ];
    assert_eq!(support::kinds(&lines), expected);
    assert_eq!(support::kinds_without_attention(&lines), expected);
}

#[test]
fn event_windows_preserve_multiplicity_without_pinning_independent_order() {
    // Delegate Events promises the anchors, not finish versus copied usage.
    for members in [
        ["usage_recorded", "delegate_finished"],
        ["delegate_finished", "usage_recorded"],
    ] {
        for date_position in 0..=4 {
            let mut lines =
                support::expected(&["turn_completed", members[0], members[1], "job_completed"]);
            lines[0]["turn_id"] = json!("t_cancel");
            lines[3]["payload"] = json!({"job_id": "j_cancel"});
            lines.insert(date_position, json!({"kind": "date_changed"}));
            let lo = lines
                .iter()
                .position(|line| line["turn_id"] == "t_cancel")
                .unwrap();
            let hi = lines
                .iter()
                .position(|line| line["payload"]["job_id"] == "j_cancel")
                .unwrap();
            assert_eq!(lines[lo]["kind"], "turn_completed");
            assert_eq!(lines[hi]["kind"], "job_completed");
            support::assert_set_between(&lines, lo, hi, &["usage_recorded", "delegate_finished"]);
        }
    }
    let empty = support::expected(&["turn_completed", "job_completed"]);
    support::assert_set_between(&empty, 0, 1, &[]);
    let repeated = support::expected(&[
        "turn_completed",
        "usage_recorded",
        "usage_recorded",
        "job_completed",
    ]);
    support::assert_set_between(&repeated, 0, 3, &["usage_recorded", "usage_recorded"]);
}

#[test]
fn event_windows_reject_missing_extra_duplicate_and_invalid_anchors() {
    for members in [
        vec!["delegate_finished"],
        vec!["usage_recorded", "delegate_finished", "extra"],
        vec!["usage_recorded", "usage_recorded", "delegate_finished"],
    ] {
        let mut kinds = vec!["turn_completed"];
        kinds.extend(members);
        kinds.push("job_completed");
        let lines = support::expected(&kinds);
        assert!(
            std::panic::catch_unwind(|| support::assert_set_between(
                &lines,
                0,
                lines.len() - 1,
                &["usage_recorded", "delegate_finished"]
            ))
            .is_err(),
            "accepted {kinds:?}"
        );
    }
    let lines = support::expected(&["turn_completed", "usage_recorded", "job_completed"]);
    for (lo, hi) in [
        (0, 0),
        (1, 1),
        (2, 1),
        (0, 3),
        (0, 4),
        (3, 4),
        (usize::MAX, 0),
    ] {
        assert!(
            std::panic::catch_unwind(|| support::assert_set_between(
                &lines,
                lo,
                hi,
                &["usage_recorded"]
            ))
            .is_err(),
            "accepted anchors {lo}, {hi}"
        );
    }
    // Exact membership is not permission to reverse a promised relation.
    let reversed = support::expected(&[
        "fiber_started",
        "delegate_started",
        "job_started",
        "fiber_exited",
    ]);
    support::assert_set_between(&reversed, 0, 3, &["job_started", "delegate_started"]);
    assert!(
        std::panic::catch_unwind(|| assert_eq!(
            support::kinds(&reversed[1..3]),
            ["job_started", "delegate_started"]
        ))
        .is_err()
    );
}

/// The `job_completed` line for `job`.
fn ended<'a>(lines: &'a [Value], job: &str) -> &'a Value {
    lines
        .iter()
        .find(|line| line["kind"] == "job_completed" && line["payload"]["job_id"] == job)
        .unwrap()
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
    /// The session `start` opened, for `connect`.
    id: Option<String>,
    /// Every line `wait_line` has taken, so `wait` returns the whole stream.
    taken: Vec<Value>,
}

impl Running {
    /// Starts `fiber session --id <id>` with `extra` appended, draining
    /// stdout on a thread and keeping stderr for a failure.
    #[track_caller]
    fn start(setup: &support::Setup, id: &str, extra: &[&str]) -> Self {
        let workspace = setup.workspace();
        let mut args = vec!["session", "--id", id, "--workspace"];
        args.push(workspace.to_str().unwrap());
        args.extend(extra);
        let mut running = Self::spawn(setup, &args);
        running.id = Some(id.to_owned());
        running
    }

    /// Spawns `fiber` with `args`, draining stdout on a thread and keeping
    /// stderr for a failure.
    #[track_caller]
    fn spawn(setup: &support::Setup, args: &[&str]) -> Self {
        let (running, _) = Self::spawn_stdin(setup, args, Stdio::null());
        running
    }

    /// As [`Running::spawn`], with `stdin`: the caller holds the returned
    /// stdin open, which a delegate reads as its lifeline.
    #[track_caller]
    fn spawn_stdin(
        setup: &support::Setup,
        args: &[&str],
        stdin: Stdio,
    ) -> (Self, Option<std::process::ChildStdin>) {
        fakes::warm_up(Path::new(env!("CARGO_BIN_EXE_fiber")), &["--bogus"], 2);
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
                id: None,
                taken: Vec::new(),
            },
            stdin,
        )
    }

    /// Connects a client to the started session's socket, on a thread
    /// bounded by the deadline.
    fn connect(&self, setup: &support::Setup) -> support::Socket {
        let id = self.id.as_deref().expect("connect needs a started session");
        support::Socket::connect(setup.deadline, &setup.session_socket(id))
    }

    /// Stdout lines until one completes `done`: the drain thread signals
    /// every line over the channel. One deadline bounds the whole wait:
    /// each receive takes what remains of the test's deadline, at most
    /// 5 s, so endless unrelated output cannot postpone expiry, and a
    /// quiet hang fails fast.
    #[track_caller]
    fn wait_line(&mut self, what: &str, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let mut got = Vec::new();
        loop {
            // Capped, never reset: the remainder shrinks as the test runs.
            let left = self.deadline.left().min(Duration::from_secs(5));
            if left.is_zero() {
                panic!("waited until the deadline for {what}; got {got:?}")
            }
            match Deadline::after(left).recv(&self.lines) {
                Ok(line) => {
                    let stop = done(&line);
                    self.taken.push(line.clone());
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

    /// Stdout lines until one completes `done`, bounded by
    /// `MUST_SUCCEED_WITHIN`: the line wait loops inside a fresh success
    /// bound, so a staging hang fails fast instead of running to the
    /// test's deadline. The test's deadline is restored afterwards.
    #[track_caller]
    fn wait_line_within(&mut self, what: &str, done: impl FnMut(&Value) -> bool) -> Vec<Value> {
        let saved = self.deadline;
        self.deadline = Deadline::after(fakes::MUST_SUCCEED_WITHIN);
        let got = self.wait_line(what, done);
        self.deadline = saved;
        got
    }

    /// Waits for the process to exit: the wait runs on a thread under the
    /// deadline, and expiry kills the group and reaps it. Stands the
    /// watchdog down first: dropping it would kill the group. Every stdout
    /// line is read: the drain ends at end of file once the process is
    /// gone, so a disconnect means the output is whole. Returns the whole
    /// stream, including the lines `wait_line` returned.
    #[track_caller]
    fn wait(self) -> (ExitStatus, Vec<Value>, String) {
        let watchdog = self._watchdog;
        watchdog.stand_down(self.deadline.cleanup_phase());
        let mut out = self.taken;
        while let Ok(line) = self.lines.try_recv() {
            out.push(line);
        }
        let (done, finished) = mpsc::channel();
        let mut child = self.child;
        let group = self.group;
        let deadline = self.deadline;
        thread::spawn(move || done.send(child.wait()).unwrap());
        let status = match deadline.recv(&finished) {
            Ok(status) => status.unwrap(),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                support::expired(deadline, group, &finished, "the session to exit")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the session wait thread ended without a result")
            }
        };
        loop {
            match deadline.recv(&self.lines) {
                Ok(line) => out.push(line),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    panic!("waited for the session's last stdout lines: {out:?}")
                }
            }
        }
        let stderr = match deadline.recv(&self.stderr) {
            Ok(text) => text,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                panic!("waited for the session's stderr to close; stdout: {out:?}")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("the stderr thread ended without its text")
            }
        };
        (status, out, stderr)
    }
}

/// Subscribes `client` full, for the session's later lines. A late
/// subscription replays the log first, so lines arrive until the accept.
#[track_caller]
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
#[track_caller]
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

/// Session requests declare tools; reviewer requests do not. Each new
/// prompt's first reply delegates, and its receipt's reply ends the turn.
fn depth_responder(request: &Request) -> Response {
    let body: Value = serde_json::from_slice(&request.body).unwrap();
    if body["tools"]
        .as_array()
        .is_none_or(|tools| tools.is_empty())
    {
        return text_reply("allow");
    }
    let input = body["input"].as_array().unwrap();
    if input
        .iter()
        .any(|item| item["type"] == "function_call_output")
    {
        support::hello()
    } else {
        support::stream(&[spawn_call("fiber:pb/m")])
    }
}

#[test]
fn a_depth_one_delegate_starts_depth_two_with_the_same_tools_but_no_grandchild() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(depth_responder).unwrap();
    let child = ProviderServer::start_responding(depth_responder).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    let id = doors::mint("s_");
    let parent_id = doors::mint("s_");
    let job = doors::mint("j_");
    let (mut running, lifeline) = Running::spawn_stdin(
        &setup,
        &[
            "session",
            "--id",
            &id,
            "--workspace",
            setup.root.path().to_str().unwrap(),
            "--model",
            "pa/m",
            "--prompt",
            "delegate",
            "--parent",
            &parent_id,
            "--delegate-id",
            &job,
            "--depth",
            "1",
        ],
        Stdio::piped(),
    );
    running.wait_line("the depth-one session exits", |line| {
        line["kind"] == "fiber_exited"
    });
    drop(lifeline);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        completed_for(&lines, "delegate_spawn")["payload"]["status"],
        "completed"
    );
    let started = one(&lines, "delegate_started");
    let child_id = started["delegate_session_id"].as_str().unwrap();
    let child_lines = read_lines(&ask_sessions(&setup).join(child_id).join("events.jsonl"));
    assert_depth_refusal(&child_lines, false, false, "delegate_spawn");
    let requests = child.requests();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let mut names: Vec<_> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(names, support::TOOL_NAMES);
    assert_eq!(
        one(&child_lines, "session_started")["parent"],
        json!({"session_id": id, "delegate_id": started["job_id"]})
    );
}

/// `forked` names a fork's refusal: the fork sends the logged preamble
/// verbatim, so its log holds no `preamble_built` and no
/// `opening_message`, unlike a spawned delegate's.
fn assert_depth_refusal(lines: &[Value], resumed: bool, forked: bool, tool: &str) {
    let mut expected = vec![
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
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ];
    if forked {
        expected.remove(4);
        expected.remove(3);
    }
    if resumed {
        expected.remove(4);
        expected.remove(0);
    }
    assert_eq!(owned_kinds(lines), expected);
    let completed = completed_for(lines, tool);
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "depth_exceeded");
    assert!(lines.iter().all(|line| line["kind"] != "delegate_started"));
    assert!(lines.iter().any(|line| line["kind"] == "turn_completed"));
}

#[test]
fn a_resumed_depth_two_session_still_refuses_delegate_spawn() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let session_calls = Mutex::new(0usize);
    let parent = ProviderServer::start_responding(move |request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        if body["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty())
        {
            return text_reply("allow");
        }
        let mut calls = session_calls.lock().unwrap();
        *calls += 1;
        if [1, 3].contains(&*calls) {
            support::stream(&[spawn_call("fiber:pb/m")])
        } else {
            support::hello()
        }
    })
    .unwrap();
    let child = ProviderServer::start([support::hello()]).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    let id = doors::mint("s_");
    let parent_id = doors::mint("s_");
    let job = doors::mint("j_");
    for resume in [false, true] {
        let log = ask_sessions(&setup).join(&id).join("events.jsonl");
        let before = read_lines(&log).len();
        let mut args = vec![
            "session",
            "--id",
            &id,
            "--workspace",
            setup.root.path().to_str().unwrap(),
            "--prompt=delegate",
            "--parent",
            &parent_id,
            "--delegate-id",
            &job,
            "--depth",
            "2",
        ];
        if resume {
            args.push("--resume");
        }
        let (mut running, lifeline) = Running::spawn_stdin(&setup, &args, Stdio::piped());
        running.wait_line("the depth-two session exits", |line| {
            line["kind"] == "fiber_exited"
        });
        drop(lifeline);
        let (status, _lines, stderr) = running.wait();
        assert_eq!(status.code(), Some(0), "{stderr}");
        let lines = read_lines(&log);
        assert_depth_refusal(&lines[before..], resume, false, "delegate_spawn");
    }
    assert!(child.requests().is_empty());
    let lines = read_lines(&ask_sessions(&setup).join(&id).join("events.jsonl"));
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["kind"] == "tool_call_completed"
                && line["payload"]["error"]["code"] == "depth_exceeded")
            .count(),
        2
    );
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
    running.wait_line("the ending notice's turn end", |line| {
        ended += usize::from(line["kind"] == "turn_completed");
        ended == 2
    });
    child.release();
    let (status, lines, stderr) = running.wait();
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
    let kinds = owned_kinds(&ask.lines);
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
        parent.await_requests(1, setup.deadline),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, setup.deadline),
        "the verdict was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(3, setup.deadline),
        "the stop was requested"
    );
    assert!(
        child.await_requests(1, setup.deadline),
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
        child.await_requests(1, setup.deadline),
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
        child.await_requests(1, setup.deadline),
        "the child stalled its model call"
    );
    let client = running.connect(&setup);
    subscribe(&client);
    close_now(&client);
    let _tail = support::until_close(&client);
    drop(client);
    let (status, out, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    // The shutdown stops the delegate as a stop does: its finish, then its
    // cancelled end, each once. The end is in the log, so the child's exit
    // line is already written when it is read.
    let kinds = owned_kinds(&out);
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

/// The crash test's stream kinds in order, setting aside `session_status`,
/// `clients` and `attention` per `docs/testing.md` ("Event streams").
/// Pinned from a real run; the `clients` count is asserted separately.
const CRASH_ORDERED: &[&str] = &[
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
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "usage_recorded",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "steering_queue",
    "steering_queue",
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
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "usage_recorded",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];
#[test]
fn a_crashed_delegate_ends_failed_while_its_sibling_completes() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    // Two delegates from two prompt turns: the first receipt's reply
    // spawns the second, and the second receipt's reply ends the prompt.
    // Reviewer verdicts answer "allow", as the answering helper does. A
    // request carrying the steered text below spawns a third delegate.
    let parent = ProviderServer::start_responding({
        let session_calls = Arc::new(AtomicUsize::new(0));
        let spawned_third = Arc::new(AtomicBool::new(false));
        move |request: &Request| {
            let text = String::from_utf8_lossy(&request.body);
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            if body["tools"]
                .as_array()
                .is_none_or(|tools| tools.is_empty())
            {
                return text_reply("allow");
            }
            if text.contains("scan again") && !spawned_third.swap(true, Ordering::SeqCst) {
                return support::stream(&[spawn_call("fiber:pb/m")]);
            }
            if session_calls.fetch_add(1, Ordering::SeqCst) < 2 {
                return support::stream(&[spawn_call("fiber:pb/m")]);
            }
            support::hello()
        }
    })
    .unwrap();
    // Both children stall on their first model call; a third reply serves
    // the late delegate the steer below spawns.
    let child =
        ProviderServer::start([support::hello(), support::hello(), support::hello()]).unwrap();
    child.hold();
    providers(&setup, &parent, &child);
    slow_idle(&setup);

    let id = doors::mint("s_");
    let mut running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    // Both delegates run their turns: each stalled its model call.
    assert!(
        child.await_requests(2, setup.deadline),
        "both children stalled their model calls"
    );
    let mut child_ids = Vec::new();
    running.wait_line("both delegates start", |line| {
        if line["kind"] == "delegate_started" {
            child_ids.push(
                line["payload"]["delegate_session_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
        }
        child_ids.len() == 2
    });
    // Crash the first delegate as an outside kill does: SIGKILL to its
    // group, which it leads. Its sibling keeps stalling, so the crashed
    // end and its wake turn complete before the release: the sibling's end
    // lands in its own later turn and the stream order is fixed.
    support::kill_matching(setup.deadline, &child_ids[0]).unwrap();
    let mut ends = running.wait_line_within("the crashed delegate end", |line| {
        line["kind"] == "job_completed"
    });
    ends.retain(|line| line["kind"] == "job_completed");
    assert_eq!(ends.len(), 1, "the crashed delegate ends: {ends:?}");
    running.wait_line_within("the crashed end's wake turn", |line| {
        line["kind"] == "turn_completed"
    });
    child.release();
    // The sibling's end arrives, each delegate ending once.
    while ends.len() < 2 {
        let more = running.wait_line_within("the sibling delegate end", |line| {
            line["kind"] == "job_completed"
        });
        ends.extend(
            more.into_iter()
                .filter(|line| line["kind"] == "job_completed"),
        );
    }
    assert_eq!(ends.len(), 2, "each delegate ends once: {ends:?}");
    // The sibling's wake turn completes before the steer, so the steered
    // spawn starts its own turn and the stream order is fixed.
    running.wait_line_within("the sibling end's wake turn", |line| {
        line["kind"] == "turn_completed"
    });
    // The registry still answers commands: steer the parent, which spawns
    // a third delegate that completes on its released provider.
    let client = running.connect(&setup);
    subscribe(&client);
    client.send(
        r#"{"id":"c_again","command":"steer","args":{"content":[{"type":"text","text":"scan again"}]}}"#,
    );
    while ends.len() < 3 {
        let more = running.wait_line("the third delegate end", |line| {
            line["kind"] == "job_completed"
        });
        ends.extend(
            more.into_iter()
                .filter(|line| line["kind"] == "job_completed"),
        );
    }
    assert_eq!(ends.len(), 3, "each delegate ends once: {ends:?}");
    // The late delegate's end wakes one more turn: let it complete
    // before closing, so the close lands idle and interrupts nothing.
    running.wait_line("the late delegate's wake turn", |line| {
        line["kind"] == "turn_completed"
    });
    close_now(&client);
    let _tail = support::until_close(&client);
    drop(client);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    // The crash is contained to the dead delegate: it ends failed with
    // the signal named, never a `failed signal` status; its sibling and
    // the late delegate complete, and every parent turn completes.
    assert_eq!(ordered_kinds(&lines), CRASH_ORDERED);
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["kind"] == "clients")
            .count(),
        1,
        "one attach, one clients line"
    );
    let starts: Vec<Value> = lines
        .iter()
        .filter(|line| line["kind"] == "delegate_started")
        .cloned()
        .collect();
    assert_eq!(starts.len(), 3, "two delegates, then the steered one");
    let job_of = |session: &str| {
        starts
            .iter()
            .find(|line| line["payload"]["delegate_session_id"] == session)
            .unwrap()["payload"]["job_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let crashed_done = ended(&lines, &job_of(&child_ids[0]));
    assert_eq!(crashed_done["payload"]["status"], "failed");
    assert_eq!(crashed_done["payload"]["error"]["code"], "signal");
    let sibling_done = ended(&lines, &job_of(&child_ids[1]));
    assert_eq!(sibling_done["payload"]["status"], "completed");
    let finished: Vec<Value> = lines
        .iter()
        .filter(|line| line["kind"] == "delegate_finished")
        .cloned()
        .collect();
    assert_eq!(finished.len(), 3, "one finish per delegate: {finished:?}");
    for line in lines.iter().filter(|line| line["kind"] == "turn_completed") {
        assert_eq!(line["payload"]["outcome"], "completed", "{line}");
    }
}

/// The escalation test's parent stream kinds in order, setting aside
/// `session_status`, `clients` and `attention` per `docs/testing.md`
/// ("Event streams"). Pinned from a real run.
const ESC_PARENT_ORDERED: &[&str] = &[
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
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "turn_started",
    "step_started",
    "jobs_pending_notified",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "usage_recorded",
    "usage_recorded",
    "usage_recorded",
    "usage_recorded",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The escalation test's delegate log kinds in order: the log file holds no
/// unpositioned lines, so this is the complete list.
const ESC_CHILD_ORDERED: &[&str] = &[
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
    "usage_recorded",
    "permission_resolved",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn a_delegate_escalation_is_a_block_the_parent_never_sees() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    // The delegate tries to spawn again, which its reviewer blocks: with
    // nobody to answer, the block denies the call and the delegate
    // carries on. The parent's own verdict answers "allow".
    let parent = ProviderServer::start_responding({
        let session_calls = Arc::new(AtomicUsize::new(0));
        let reviewer_calls = Arc::new(AtomicUsize::new(0));
        move |request: &Request| {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            if body["tools"]
                .as_array()
                .is_none_or(|tools| tools.is_empty())
            {
                return if reviewer_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    text_reply("allow")
                } else {
                    text_reply("block no further delegates")
                };
            }
            if session_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                return support::stream(&[spawn_call("fiber:pb/m")]);
            }
            support::hello()
        }
    })
    .unwrap();
    // The delegate's first reply spawns, which the block denies; its
    // second ends its turn, carrying on past the block.
    let child = ProviderServer::start([
        support::stream(&[spawn_call("fiber:pb/m")]),
        support::hello(),
    ])
    .unwrap();
    providers(&setup, &parent, &child);

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(ordered_kinds(&lines), ESC_PARENT_ORDERED);
    assert!(
        lines
            .iter()
            .all(|line| line["kind"] != "permission_requested"),
        "the parent log holds no approval"
    );
    let child_id = one(&lines, "delegate_started")["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child_lines = read_lines(&ask_sessions(&setup).join(child_id).join("events.jsonl"));
    assert_eq!(owned_kinds(&child_lines), ESC_CHILD_ORDERED);
    assert!(
        child_lines
            .iter()
            .all(|line| line["kind"] != "permission_requested"),
        "the delegate log holds no approval"
    );
    // The block denied the call under the reviewer, and the delegate's
    // turn still completed: the escalation was a block, and it carried on.
    let resolved = one(&child_lines, "permission_resolved");
    assert_eq!(resolved["decision"], "deny");
    assert_eq!(resolved["decided_by"], "reviewer");
    let denied = completed_for(&child_lines, "delegate_spawn");
    assert_eq!(denied["payload"]["status"], "denied");
    assert_eq!(denied["payload"]["reason"], "reviewer");
    assert_eq!(one(&child_lines, "turn_completed")["outcome"], "completed");
    let done = one(&lines, "job_completed");
    assert_eq!(done["status"], "completed");
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
        parent.await_requests(1, setup.deadline),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, setup.deadline),
        "the verdict was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(3, setup.deadline),
        "the read was requested"
    );
    assert!(
        child.await_requests(1, setup.deadline),
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
    let client = running.connect(&setup);
    subscribe(&client);
    close_now(&client);
    let _tail = support::until_close(&client);
    drop(client);
    let (status, _, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
}

/// An `openai-completions` chunk answering `text` with generation `id`.
fn ctext_id(id: &str, text: &str) -> Value {
    json!({"id": id, "object": "chat.completion.chunk", "choices": [{
        "index": 0, "delta": {"role": "assistant", "content": text},
        "finish_reason": "stop"}]})
}

/// An `openai-completions` usage chunk with generation `id` reporting
/// `cost`: distinct ids keep delegate generations from colliding with the
/// parent's own (`docs/delegates.md`, "Streams" counts one line per id).
fn cusage_id(id: &str, cost: f64) -> Value {
    json!({"id": id, "choices": [], "usage": {"prompt_tokens": 15,
        "completion_tokens": 9, "total_tokens": 24, "cost": cost,
        "prompt_tokens_details": {"cached_tokens": 14}}})
}

/// An `openai-completions` chunk calling `name` with generation `id`.
fn ccall_id(id: &str, name: &str, arguments: &Value) -> Value {
    json!({"id": id, "object": "chat.completion.chunk", "choices": [{
        "index": 0,
        "delta": {"role": "assistant", "tool_calls": [{
            "id": "call_1", "type": "function",
            "function": {"name": name, "arguments": arguments.to_string()}}]},
        "finish_reason": "tool_calls"}]})
}

#[test]
fn a_delegate_spend_tripping_the_budget_stops_the_whole_tree() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    // The root spawns the child, then parks in `jobs wait`; after the wait
    // a read forces the next model request, which the trip's spend refuses.
    // The child spawns the grandchild, then parks in `jobs wait` the same
    // way; the grandchild's costly reply bubbles up through both logs.
    fs::write(setup.workspace().join("note.txt"), "the note\n").unwrap();
    // The wait and the read name the job the receipt minted, which no
    // script can know: each server counts its session's requests, answering
    // reviewer requests (which carry no tools) with an allow.
    let parent = ProviderServer::start_responding({
        let stage = std::sync::atomic::AtomicUsize::new(0);
        move |request: &Request| {
            let text = String::from_utf8_lossy(&request.body);
            // Session requests declare tools; reviewer requests carry the
            // call in history but declare none.
            if !text.contains(r#""tools":[{"#) {
                return cstream(&[ctext_id("r_allow", "allow"), cusage_id("r_allow", 0.0)]);
            }
            match stage.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => cstream(&[
                    ccall_id(
                        "r_spawn",
                        "delegate_spawn",
                        &json!({"description": "scan", "prompt": "scan shallow", "model": "fiber:pb/m"}),
                    ),
                    cusage_id("r_spawn", 0.0),
                ]),
                1 => cstream(&[
                    ccall_id(
                        "r_wait",
                        "jobs",
                        &json!({"action": "wait", "job_id": job_id(&text), "timeout_ms": 30000}),
                    ),
                    cusage_id("r_wait", 0.0),
                ]),
                _ => cstream(&[
                    ccall_id("r_read", "read", &json!({"path": "note.txt"})),
                    cusage_id("r_read", 0.0),
                ]),
            }
        }
    })
    .unwrap();
    // The wait names the job the receipt minted, which no script can know:
    // the server rebuilds the wait call from the receipt it sees. Held
    // until the root parks in the wait: the grandchild's reply then lands
    // while both ancestors wait, whatever the scheduling.
    let child = ProviderServer::start_responding({
        let stage = std::sync::atomic::AtomicUsize::new(0);
        move |request: &Request| {
            let text = String::from_utf8_lossy(&request.body);
            // Session requests declare tools; reviewer requests carry the
            // call in history but declare none.
            if !text.contains(r#""tools":[{"#) {
                return cstream(&[ctext_id("c_allow", "allow"), cusage_id("c_allow", 0.0)]);
            }
            // The receipt names the delegate the wait then names; it
            // carries the spawn call too, so it comes before the prompt.
            if text.contains("Started delegate") {
                return match stage.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                    0 => cstream(&[
                        ccall_id(
                            "c_wait",
                            "jobs",
                            &json!({"action": "wait", "job_id": job_id(&text), "timeout_ms": 30000}),
                        ),
                        cusage_id("c_wait", 0.0),
                    ]),
                    _ => cstream(&[ctext_id("c_done", "done"), cusage_id("c_done", 0.0)]),
                };
            }
            // The grandchild's reply costs past the budget on its own.
            if text.contains("scan deep") {
                return cstream(&[
                    ctext_id("g_cost", "deep scan done"),
                    cusage_id("g_cost", 100.0),
                ]);
            }
            cstream(&[
                ccall_id(
                    "c_spawn",
                    "delegate_spawn",
                    &json!({"description": "scan", "prompt": "scan deep", "model": "fiber:pb/m"}),
                ),
                cusage_id("c_spawn", 0.0),
            ])
        }
    })
    .unwrap();
    providers_url(&setup, &parent.url(), &child.url(), "openai-completions");
    standing_allow(&setup);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "budget": {"usd": 1.0}, "session": {"idle_exit_ms": 3600000}}),
    );

    // Held until the root parks in the wait: the grandchild's reply then
    // lands while both ancestors wait, whatever the scheduling.
    child.hold();
    let id = doors::mint("s_");
    let mut running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    let parked = running.wait_line("the root parked in jobs wait", |line| {
        line["kind"] == "tool_call_requested" && line["payload"]["name"] == "jobs"
    });
    // The spawn ran first, then the wait the root now parks in.
    assert_eq!(parked.last().unwrap()["payload"]["name"], "jobs");
    assert!(
        parked
            .iter()
            .any(|line| line["kind"] == "tool_call_requested"
                && line["payload"]["name"] == "delegate_spawn"),
        "the spawn ran before the wait"
    );
    child.release();
    // The crossing copy trips the budget while the root waits: the wait
    // ends, the next request is refused, and the turn ends failed.
    let first = running.wait_line("the budget end", |line| {
        line["kind"] == "turn_completed" && line["payload"]["outcome"] == "failed"
    });
    let completed = first.last().unwrap();
    assert_eq!(completed["payload"]["error"]["code"], "budget_exceeded");
    // The grandchild's spend lands in the root log once, naming the
    // grandchild: one line per generation id. The spawn lines arrived
    // before the wait the root parked in.
    let before: Vec<Value> = parked.into_iter().chain(first.iter().cloned()).collect();
    let copies: Vec<Value> = before
        .iter()
        .filter(|line| {
            line["kind"] == "usage_recorded" && line["payload"]["generation_id"] == "g_cost"
        })
        .cloned()
        .collect();
    assert_eq!(copies.len(), 1, "the copy arrives once: {copies:?}");
    assert_eq!(copies[0]["payload"]["cost"], 100.0);
    let root_started = one(&before, "delegate_started");
    let child_id = root_started["delegate_session_id"].as_str().unwrap();
    let child_lines = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(child_id)
            .join("events.jsonl"),
    );
    let child_started = one(&child_lines, "delegate_started");
    let grandchild_id = child_started["delegate_session_id"].as_str().unwrap();
    assert_eq!(copies[0]["payload"]["origin_session_id"], grandchild_id);
    // The child's own copy names the grandchild too: the relay preserves
    // the originator.
    let relayed: Vec<Value> = child_lines
        .iter()
        .filter(|line| {
            line["kind"] == "usage_recorded" && line["payload"]["generation_id"] == "g_cost"
        })
        .cloned()
        .collect();
    assert_eq!(relayed.len(), 1);
    assert_eq!(relayed[0]["payload"]["origin_session_id"], grandchild_id);
    // The child ends cancelled by the trip, exactly once; the grandchild
    // completed on its own before the stop reached it.
    let mut batch: Vec<Value> = first
        .iter()
        .filter(|line| line["kind"] == "job_completed")
        .cloned()
        .collect();
    while batch.is_empty() {
        let more = running.wait_line("the delegate end", |line| line["kind"] == "job_completed");
        batch.extend(
            more.into_iter()
                .filter(|line| line["kind"] == "job_completed"),
        );
    }
    assert_eq!(batch.len(), 1, "the child ends once: {batch:?}");
    assert_eq!(batch[0]["payload"]["status"], "cancelled");
    let child_exited = child_lines.last().unwrap().clone();
    assert_eq!(child_exited["kind"], "fiber_exited");
    assert_eq!(child_exited["payload"]["exit_code"], 143);
    let grandchild_lines = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(grandchild_id)
            .join("events.jsonl"),
    );
    let grandchild_exited = grandchild_lines.last().unwrap().clone();
    assert_eq!(grandchild_exited["kind"], "fiber_exited");
    // The grandchild's normal exit after its reply races the stop: it
    // either finished first or was killed with the tree. Either way it
    // is done, and the stalled child above proves the stop itself.
    assert!(
        grandchild_exited["payload"]["exit_code"] == 0
            || grandchild_exited["payload"]["exit_code"] == 143,
        "unexpected grandchild exit: {grandchild_exited:?}"
    );
    // A late spawn past the trip never runs: the next turn still fails on
    // the budget before any model call is sent, so no delegate starts.
    let client = running.connect(&setup);
    subscribe(&client);
    client.send(r#"{"id":"c_late","command":"steer","args":{"content":[{"type":"text","text":"spawn another"}]}}"#);
    let late = running.wait_line("the late turn", |line| {
        line["kind"] == "turn_completed" && line["payload"]["outcome"] == "failed"
    });
    let late_completed = late.last().unwrap();
    assert_eq!(
        late_completed["payload"]["error"]["code"],
        "budget_exceeded"
    );
    let started: Vec<Value> = before
        .iter()
        .chain(late.iter())
        .filter(|line| line["kind"] == "delegate_started")
        .cloned()
        .collect();
    assert_eq!(started.len(), 1, "no late delegate starts: {started:?}");
    let ends: Vec<Value> = before
        .iter()
        .chain(late.iter())
        .filter(|line| line["kind"] == "job_completed")
        .cloned()
        .collect();
    assert_eq!(ends.len(), 1, "one end per job: {ends:?}");
    close_now(&client);
    let _tail = support::until_close(&client);
    drop(client);
    let (status, _, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
}

#[test]
fn a_delegate_whose_call_fails_without_a_claimed_trip_keeps_its_own_exit_code() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    // The root spawns one child and parks in `jobs wait`; the child's
    // only model call fails fast. No budget is configured anywhere, so
    // no trip can claim and no tree stop is coming: the child's failed
    // turn must drain with its own code, not the tree-stop code.
    let parent = ProviderServer::start_responding({
        let stage = std::sync::atomic::AtomicUsize::new(0);
        move |request: &Request| {
            let text = String::from_utf8_lossy(&request.body);
            if !text.contains(r#""tools":[{"#) {
                return cstream(&[ctext_id("r_allow", "allow"), cusage_id("r_allow", 0.0)]);
            }
            match stage.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => cstream(&[
                    ccall_id(
                        "r_spawn",
                        "delegate_spawn",
                        &json!({"description": "scan", "prompt": "scan shallow", "model": "fiber:pb/m"}),
                    ),
                    cusage_id("r_spawn", 0.0),
                ]),
                1 => cstream(&[
                    ccall_id(
                        "r_wait",
                        "jobs",
                        &json!({"action": "wait", "job_id": job_id(&text), "timeout_ms": 30000}),
                    ),
                    cusage_id("r_wait", 0.0),
                ]),
                _ => cstream(&[ctext_id("r_done", "done"), cusage_id("r_done", 0.0)]),
            }
        }
    })
    .unwrap();
    // Every model call fails at once: the veto header rules out retries,
    // so the turn fails on the first attempt.
    let child = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""tools":[{"#) {
            return cstream(&[ctext_id("c_allow", "allow"), cusage_id("c_allow", 0.0)]);
        }
        Response::status(500, "boom").header("x-should-retry", "false")
    })
    .unwrap();
    install_url(
        &setup,
        "pa",
        &parent.url(),
        "openai-completions",
        &json!({}),
    );
    install_url(&setup, "pb", &child.url(), "openai-completions", &json!({}));
    standing_allow(&setup);
    slow_idle(&setup);
    let id = doors::mint("s_");
    let mut running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    let parked = running.wait_line("the root parked in jobs wait", |line| {
        line["kind"] == "tool_call_requested" && line["payload"]["name"] == "jobs"
    });
    assert!(
        parked
            .iter()
            .any(|line| line["kind"] == "tool_call_requested"
                && line["payload"]["name"] == "delegate_spawn"),
        "the spawn ran before the wait"
    );
    // The child's end: failed on its call, not cancelled by any stop.
    let mut batch: Vec<Value> = Vec::new();
    while batch.is_empty() {
        let more = running.wait_line("the delegate end", |line| line["kind"] == "job_completed");
        batch.extend(
            more.into_iter()
                .filter(|line| line["kind"] == "job_completed"),
        );
    }
    assert_eq!(batch.len(), 1, "the child ends once: {batch:?}");
    assert_eq!(batch[0]["payload"]["status"], "failed");
    let root_started = one(&parked, "delegate_started");
    let child_id = root_started["delegate_session_id"].as_str().unwrap();
    let child_lines = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(child_id)
            .join("events.jsonl"),
    );
    // The failed turn keeps its own code: an error is recorded (a stop
    // would leave none) and the exit is 1, not the tree-stop 143.
    let child_exited = child_lines.last().unwrap().clone();
    assert_eq!(child_exited["kind"], "fiber_exited");
    assert_eq!(child_exited["payload"]["exit_code"], 1);
    assert!(
        child_exited["payload"]["error"]["code"].is_string(),
        "the failure names its code: {child_exited:?}"
    );
    let client = running.connect(&setup);
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
        parent.await_requests(4, setup.deadline),
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
    direct_running.wait_line("the direct run's exit", |line| {
        line["kind"] == "fiber_exited"
    });
    drop(direct_stdin);
    let (direct_status, direct_lines, direct_stderr) = direct_running.wait();
    // Its run ended on its own: one turn and out, with no idle wait.
    assert_eq!(direct_status.code(), Some(0), "stderr: {direct_stderr}");
    let direct_all: Vec<&Value> = direct_lines.iter().collect();
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
        parent.await_requests(1, setup.deadline),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, setup.deadline),
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
    // A configured role: the refusal lists it with the references.
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000},
            "roles": {"deep": "fiber:pb/m"}}),
    );
    standing_allow(&setup);
    // `slow_idle` would overwrite the roles above, so its long idle is
    // inlined in the config written here: an exit proves the run ended
    // on its own.

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
    assert!(
        message.contains("Unknown delegate model \"fiber:fake/none\"."),
        "{message}"
    );
    assert!(message.contains("deep: fiber:pb/m"), "{message}");
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

/// A `delegate_spawn` call without `model`: the schema leaves it optional
/// and untyped, so it reaches the tool's own check.
fn spawn_call_without_model() -> Value {
    function_call(
        "call_no_model",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello"}),
    )
}

/// A `delegate_spawn` call with a non-string `model`.
fn spawn_call_with_model_value(model: &Value) -> Value {
    let mut arguments = json!({"description": "scan", "prompt": "say hello"});
    arguments["model"] = model.clone();
    function_call("call_bad_model", "delegate_spawn", &arguments)
}

/// The refused spawn's parent durable kinds, in order, pinned from a real
/// run: two refused `delegate_spawn` calls, each reviewed and failed.
const REFUSED_MODEL_DURABLE: &[&str] = &[
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
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "usage_recorded",
    "permission_resolved",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn a_delegate_spawn_without_a_model_lists_the_roles_and_references() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call_without_model()]),
        text_reply("allow"),
        support::stream(&[spawn_call_with_model_value(&json!(5))]),
        text_reply("allow"),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([support::hello()]).unwrap();
    providers(&setup, &parent, &child);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000},
            "roles": {"deep": "fiber:pb/m"}}),
    );
    standing_allow(&setup);

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
    assert_eq!(durable_kinds(&lines), REFUSED_MODEL_DURABLE);
    let actions: Vec<&str> = lines
        .iter()
        .filter(|line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "delegate_spawn"
        })
        .map(|line| line["action_id"].as_str().unwrap())
        .collect();
    assert_eq!(actions.len(), 2);
    for (action, prefix) in [
        (actions[0], "Give model: a role or a full reference."),
        (actions[1], "model must be a string."),
    ] {
        let completed = lines
            .iter()
            .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == action)
            .unwrap();
        let error = completed["payload"]["error"].as_object().unwrap();
        assert_eq!(error["code"], "invalid_arguments");
        let message = error["message"].as_str().unwrap();
        assert!(message.starts_with(prefix), "{message}");
        for part in ["deep: fiber:pb/m", "fiber:pa/m", "fiber:pb/m"] {
            assert!(message.contains(part), "{message}");
        }
    }
    // No child started: no delegate line, and the child saw no request.
    assert!(
        lines.iter().all(|line| line["kind"] != "delegate_started"),
        "a delegate started"
    );
    assert!(child.requests().is_empty());
}

#[test]
fn delegate_models_returns_the_roles_and_references_and_its_definition_lists_neither() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[function_call("call_models", "delegate_models", &json!({}))]),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([support::hello()]).unwrap();
    providers(&setup, &parent, &child);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000},
            "roles": {"deep": "fiber:pb/m"}}),
    );

    let output = support::run_to_exit(
        setup.deadline,
        "fiber ask",
        setup.fiber(&["ask", "list the models"]),
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
    // A reads-only call then the final text: no reviewer, no permission
    // line, exactly as for a reads-only workspace call. `session_status`
    // lines are ephemeral observer output, never pinned.
    assert_eq!(
        lines
            .iter()
            .filter(|line| line["kind"] != "session_status")
            .map(|line| line["kind"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>(),
        support::read_kinds()
    );
    let requests = parent.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        lines
            .iter()
            .all(|line| line["kind"] != "permission_resolved"),
        "a reviewer was asked"
    );
    let completed = completed_for(&lines, "delegate_models");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = completed["payload"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        text,
        "Roles:\ndeep: fiber:pb/m\nReferences:\nfiber:pa/m\nfiber:pb/m\nQuota: loading"
    );
    let first: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let tools = first["tools"].as_array().unwrap();
    assert!(
        tools.iter().any(|tool| tool["name"] == "delegate_models"),
        "{tools:?}"
    );
    let encoded = serde_json::to_string(&first["tools"]).unwrap();
    assert!(!encoded.contains("deep"), "{encoded}");
    assert!(!encoded.contains("pb/m"), "{encoded}");
}

/// The first `delegate_models` call answers `Quota: loading`: `pa` holds
/// two credential labels (its environment key and a configured `work`
/// label), and the loader has not checked either provider yet. A built
/// `fiber` holds no loader pause, so no signal proves the loader ended:
/// the loaded entries are pinned in the quota unit tests instead.
#[test]
fn the_first_delegate_models_call_reads_quota_loading_with_two_credential_labels() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[function_call("call_models", "delegate_models", &json!({}))]),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([support::hello()]).unwrap();
    providers(&setup, &parent, &child);
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000},
            "roles": {"deep": "fiber:pb/m"},
            "providers": {"pa": {"credentials": {"work": {"command": ["sh", "-c", "echo key"]}}}}}),
    );

    let output = support::run_to_exit(
        setup.deadline,
        "fiber ask",
        setup.fiber(&["ask", "list the models"]),
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
    let completed = completed_for(&lines, "delegate_models");
    assert_eq!(completed["payload"]["status"], "completed");
    let text = completed["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.ends_with("\nQuota: loading"),
        "the first call answers loading: {text:?}"
    );
}

/// A role run's parent durable kinds, in order, pinned from a real run:
/// the spawn turn, the ending notice while the delegate runs, and the
/// wake turn carrying its finish.
const ROLE_PARENT_DURABLE: &[&str] = &[
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
    "jobs_pending_notified",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// A role run's child durable kinds, in order, pinned from a real run:
/// one reply completing, then the turn ends.
const ROLE_CHILD_DURABLE: &[&str] = &[
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
    "turn_completed",
    "fiber_exited",
];

/// The resumed run's parent durable kinds, in order, pinned from a real
/// run: the spawn turn, the ending notice while the delegate runs, and
/// the wake turn carrying its finish. A resume writes no
/// `session_started` and no `opening_message`: the log already holds
/// both.
const RESUMED_PARENT_DURABLE: &[&str] = &[
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
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
    "jobs_pending_notified",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn a_role_runs_its_reference_and_a_remap_applies_at_resume_without_changing_any_definition() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let thinking = json!({"thinking_levels": ["low", "high"], "thinking_default": "low"});
    let parent = ProviderServer::start_with_fallback(
        [
            support::stream(&[spawn_call("qzdeep")]),
            text_reply("allow"),
            support::hello(),
            support::hello(),
            support::hello(),
            support::stream(&[spawn_call("qzdeep")]),
            text_reply("allow"),
            support::hello(),
            support::hello(),
            support::hello(),
        ],
        support::hello(),
    )
    .unwrap();
    let pb = ProviderServer::start([support::hello()]).unwrap();
    let pc = ProviderServer::start([support::hello()]).unwrap();
    pb.hold();
    install_url(&setup, "pa", &parent.url(), "openai-responses", &json!({}));
    install_url(&setup, "pb", &pb.url(), "openai-responses", &thinking);
    install_url(&setup, "pc", &pc.url(), "openai-responses", &json!({}));
    let config_with = |target: &str| {
        json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000},
            "roles": {"qzdeep": target}})
    };
    support::write_json(
        &setup.home().join("config.json"),
        &config_with("fiber:pb/m:high"),
    );
    standing_allow(&setup);

    // Run 1: the role runs its reference with its level.
    let mut running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let mut ended = 0;
    running.wait_line("the ending notice's turn end", |line| {
        ended += usize::from(line["kind"] == "turn_completed");
        ended == 2
    });
    pb.release();
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    assert_eq!(durable_kinds(&without_copies(&lines)), ROLE_PARENT_DURABLE);
    let delegate = one(&lines, "delegate_started");
    assert_eq!(delegate["model"], "fiber:pb/m:high");
    assert_eq!(pb.requests().len(), 1);
    assert!(pc.requests().is_empty());
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let child_lines = read_lines(&ask_sessions(&setup).join(child_id).join("events.jsonl"));
    assert_eq!(durable_kinds(&child_lines), ROLE_CHILD_DURABLE);
    let built = one(&child_lines, "preamble_built");
    assert_eq!(built["model"], "pb/m");
    assert_eq!(built["thinking"], "high");
    let parent_id = lines[0]["session_id"].as_str().unwrap().to_owned();
    let first: Value = serde_json::from_slice(&parent.requests()[0].body).unwrap();
    let run1_requests = parent.requests().len();

    // Run 2: the remapped role runs the new reference, and neither the
    // tools nor the instructions changed with it.
    support::write_json(
        &setup.home().join("config.json"),
        &config_with("fiber:pc/m"),
    );
    pc.hold();
    let mut resumed = Running::spawn(&setup, &["ask", "--resume", &parent_id, "scan again"]);
    let mut re_ended = 0;
    resumed.wait_line("the resumed ending notice's turn end", |line| {
        re_ended += usize::from(line["kind"] == "turn_completed");
        re_ended == 2
    });
    pc.release();
    let (re_status, re_lines, re_stderr) = resumed.wait();
    assert_eq!(re_status.code(), Some(0), "stderr: {re_stderr}");
    assert_eq!(
        durable_kinds(&without_copies(&re_lines)),
        RESUMED_PARENT_DURABLE
    );
    let re_delegate = one(&re_lines, "delegate_started");
    assert_eq!(re_delegate["model"], "fiber:pc/m");
    assert_eq!(pc.requests().len(), 1);
    assert_eq!(pb.requests().len(), 1, "the first run's child ran again");
    let re_child_id = re_delegate["delegate_session_id"].as_str().unwrap();
    let re_child_lines = read_lines(&ask_sessions(&setup).join(re_child_id).join("events.jsonl"));
    assert_eq!(durable_kinds(&re_child_lines), ROLE_CHILD_DURABLE);
    let re_built = one(&re_child_lines, "preamble_built");
    assert_eq!(re_built["model"], "pc/m");
    assert!(re_built.get("thinking").is_none(), "{re_built}");
    let re_requests = parent.requests();
    let second: Value = serde_json::from_slice(&re_requests[run1_requests].body).unwrap();
    assert_eq!(second["tools"], first["tools"]);
    assert_eq!(second["instructions"], first["instructions"]);
    for field in [&second["tools"], &second["instructions"]] {
        let encoded = serde_json::to_string(field).unwrap();
        for part in ["qzdeep", "pb/m", "pc/m"] {
            assert!(!encoded.contains(part), "{encoded}");
        }
    }
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

/// The parent log without the delegate spend copies: a copied
/// `usage_recorded` carries its writer in `origin_session_id`, while the
/// parent's own lines leave it absent. Delegate spend relay is pinned by
/// the spend tree test and the loop copy tests; the durable-shape
/// assertions below predate it and pin the parent's own lines.
fn without_copies(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| {
            line["kind"] != "usage_recorded" || line["payload"]["origin_session_id"].is_null()
        })
        .cloned()
        .collect()
}

/// The acknowledgement of command `id`.
fn answered(lines: &[Value], id: &str) -> Value {
    lines
        .iter()
        .find(|line| is_answer(line, id))
        .unwrap_or_else(|| panic!("no answer for {id} in {:?}", owned_kinds(lines)))
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
#[track_caller]
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
        server_b.await_requests(1, setup.deadline),
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
        .unwrap_or_else(|| panic!("no session_started in {:?}", owned_kinds(&folded)));
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

#[track_caller]
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
#[track_caller]
fn wait_feed_status(watch: &FeedWatch, setup: &support::Setup, parent: &str) {
    loop {
        let line = setup
            .deadline
            .recv(&watch.each)
            .expect("the feed to report the parent before the deadline");
        if line["kind"] == "session_status" && is_session(&line, parent) {
            return;
        }
    }
}

/// Stops the hub in `slot` with SIGTERM, as a person stopping it would.
#[track_caller]
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
        server_b.await_requests(2, setup.deadline),
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
    // After the first reply completes, the steered step follows: the next
    // step, the applied steer, the emptied queue, and the second reply's
    // start. The admission queue came earlier, while the first reply still
    // streamed: the reader queues, emits and answers at once, so a
    // non-empty queue holding the steer precedes the first completed
    // message. Other lines (the probe's own answer, statuses) travel the
    // same stream and are not pinned here; the durable list below is the
    // complete ordered one.
    let first_done = delegate_lines
        .iter()
        .position(|line| line["kind"] == "assistant_message_completed")
        .expect("the delegate's first completed message");
    let admission = delegate_lines[..first_done]
        .iter()
        .filter(|line| {
            line["kind"] == "steering_queue"
                && line["payload"]["messages"]
                    .as_array()
                    .is_some_and(|messages| !messages.is_empty())
        })
        .collect::<Vec<_>>();
    assert!(!admission.is_empty(), "the at-once admission queue");
    let held = admission.last().unwrap()["payload"]["messages"][0].clone();
    assert_eq!(
        held["command_id"], "c_steer",
        "the admission queue holds the steer"
    );
    assert!(
        serde_json::to_string(&held["content"])
            .unwrap()
            .contains(MARK),
        "the admission queue holds the steered text: {held}"
    );
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
            "step_started",
            "steering_queue",
            "steering_applied",
            "assistant_message_started"
        ],
        "{steered_kinds:?}"
    );
    let after = steered;
    // The drained queue is empty: the admission queue left the window
    // with the at-once emit, so the emptied one precedes the apply.
    assert!(
        after[1]["payload"]["messages"]
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
    let parent_kinds = owned_kinds(&parent_lines);
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
    assert_eq!(
        durable_kinds(&without_copies(&parent_lines)),
        PARENT_DURABLE
    );
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
        owned_kinds(&feed_lines)
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
    // The listing holds the parent and never the delegate.
    fresh.send(r#"{"id":"c_sess","command":"sessions"}"#);
    let listing = support::until(&fresh, "the sessions answer", |line| {
        is_answer(line, "c_sess")
    });
    let answer = answered(&listing, "c_sess");
    assert_eq!(answer["kind"], "command_accepted", "{answer}");
    let live = answer["payload"]["result"]["live"].as_array().unwrap();
    let live_ids: Vec<&str> = live
        .iter()
        .map(|row| row["session_id"].as_str().unwrap())
        .collect();
    assert_eq!(live_ids, [parent.as_str()]);
    assert!(
        !serde_json::to_string(&answer).unwrap().contains(&delegate),
        "the listing named the delegate: {answer}"
    );
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
    let folded_kinds = owned_kinds(&folded);
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
    assert_eq!(durable_kinds(&without_copies(&folded)), PARENT_DURABLE);
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
    assert!(
        feed_lines
            .iter()
            .any(|line| line["kind"] == "session_status" && is_session(line, &parent)),
        "the feed held the parent: {:?}",
        owned_kinds(&feed_lines)
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

/// A credential variable nothing in this test sets: the child's read of it
/// fails before any session exists. `Setup::fiber` clears the command
/// environment, so it cannot leak in through the parent either.
const ABSENT_CHILD_CREDENTIAL: &str = "FIBER_TEST_ABSENT_CHILD_KEY_2347";

/// Installs provider `pb` with model `m` whose credential comes from an
/// environment variable nothing sets: resolving `fiber:pb/m` succeeds, so
/// the delegate spawns, but starting it fails before its session exists.
fn install_pb_without_credential(setup: &support::Setup, url: &str) {
    let source = setup.root.path().join("src-pb-nocred");
    let manifest = json!({"name": "pb", "version": "v0.0.0", "fiber": "0.0.0", "api": 1,
        "providers": {"pb": [format!("{url}/v1")]}});
    support::write_json(&source.join("extension.json"), &manifest);
    support::write_json(
        &source.join("providers/pb.json"),
        &json!({
            "name": "pb",
            "credential": {"env": ABSENT_CHILD_CREDENTIAL},
            "models": [{"id": "m", "protocol": "openai-responses",
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

/// The parent's durable kinds, in order, for the early-failure run below:
/// the spawn call and its reviewer verdict, the wait that claims the
/// failure, the list that shows it, and the final text. Two placements
/// of the finish pair are permitted: the wait's tool run claims the end
/// (`_WAIT`), or the notice lands in the inbox before the next step's
/// drain and the step boundary writes it right after `step_started`
/// (`_DRAIN`, before the assistant requests the wait). `Loop::step`
/// drains the inbox before building the request, so whichever wins the
/// race is a valid execution.
const EARLY_FAILURE_PARENT_DURABLE_WAIT: &[&str] = &[
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
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "delegate_finished",
    "job_completed",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The same run when the child's end reaches the inbox before the next
/// step's drain: the boundary writes the finish pair right after
/// `step_started`, and the wait the assistant then requests finds the
/// job already ended.
const EARLY_FAILURE_PARENT_DURABLE_DRAIN: &[&str] = &[
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
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn a_delegate_failing_before_session_reports_its_error_and_no_log_path() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let child = ProviderServer::start([support::hello()]).unwrap();
    // The wait names the job the receipt minted, which no script can know,
    // and the list follows the wait: each step is driven by what the
    // receipt and the wait call already put in the history, with distinct
    // call ids, never a sleep or a guessed id. Reviewer requests carry no
    // tools, so they never match the session check.
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if !text.contains("Started delegate") {
            return support::stream(&[spawn_call("fiber:pb/m")]);
        }
        if !text.contains("call_wait") {
            return support::stream(&[function_call(
                "call_wait",
                "jobs",
                &json!({"action": "wait", "job_id": job_id(&text), "timeout_ms": 30000}),
            )]);
        }
        if !text.contains("call_list") {
            return support::stream(&[function_call(
                "call_list",
                "jobs",
                &json!({"action": "list"}),
            )]);
        }
        support::hello()
    })
    .unwrap();
    install_url(&setup, "pa", &parent.url(), "openai-responses", &json!({}));
    install_pb_without_credential(&setup, &child.url());
    standing_allow(&setup);
    slow_idle(&setup);

    // The probe runs the child's exact command for the same model: a
    // missing credential must fail the same way the delegate will, so its
    // code and message are the expected failure, not an invented one.
    // Stdin stays open: it is the delegate lifeline, and end of file on it
    // reads as the parent going away.
    let probe_id = doors::mint("s_");
    let probe_parent = doors::mint("s_");
    let probe_job = doors::mint("j_");
    let workspace = setup.workspace().to_str().unwrap().to_owned();
    let (probe, held) = Running::spawn_stdin(
        &setup,
        &[
            "session",
            "--id",
            &probe_id,
            "--workspace",
            &workspace,
            "--model",
            "pb/m",
            "--prompt",
            "hi",
            "--parent",
            &probe_parent,
            "--delegate-id",
            &probe_job,
        ],
        Stdio::piped(),
    );
    let mut probe = probe;
    probe.wait_line("the probe's exit line", |line| {
        line["kind"] == "fiber_exited"
    });
    drop(held);
    let (probe_status, probe_lines, probe_stderr) = probe.wait();
    assert_eq!(probe_status.code(), Some(1), "stderr: {probe_stderr}");
    assert_eq!(probe_lines.len(), 1, "{probe_lines:?}");
    let probe_exited = &probe_lines[0];
    assert!(probe_exited.get("session_id").is_none(), "{probe_exited}");
    assert_eq!(probe_exited["schema_version"], 1);
    let expected_code = probe_exited["payload"]["error"]["code"]
        .as_str()
        .unwrap()
        .to_owned();
    let expected_message = probe_exited["payload"]["error"]["message"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(probe_exited["payload"]["exit_code"], 1);
    assert!(
        probe_stderr.contains(&expected_message),
        "stderr: {probe_stderr}"
    );
    assert!(
        !sessions_in(&setup.home(), &setup.workspace())
            .join(&probe_id)
            .exists(),
        "the probe made a session directory"
    );

    // The real parent: its child re-executes this same binary through the
    // production launch wiring, so the failure above is what arrives.
    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    // The child never reached its provider: no model call, no credential
    // use, nothing on the wire.
    assert!(
        child.requests().is_empty(),
        "the failed child called its provider"
    );

    // One delegate, failed with the probe's own error: never the fallback
    // for a missing result, with the reaped exit code beside it.
    let started = one(&lines, "delegate_started");
    let job = started["job_id"].as_str().unwrap().to_owned();
    let child_id = started["delegate_session_id"].as_str().unwrap().to_owned();
    let kinds = owned_kinds(&lines);
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| *kind == "delegate_started")
            .count(),
        1
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| *kind == "delegate_finished")
            .count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds.iter().filter(|kind| *kind == "job_completed").count(),
        1,
        "{kinds:?}"
    );
    // `session_status` lines are ephemeral observer output, never pinned:
    // one can land between any two lines, including this pair. The
    // adjacency below is over the stream with them set aside.
    let ordered: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] != "session_status")
        .collect();
    let finished_at = ordered
        .iter()
        .position(|line| line["kind"] == "delegate_finished")
        .unwrap();
    let completed_at = ordered
        .iter()
        .position(|line| line["kind"] == "job_completed")
        .unwrap();
    // The finish is written just before the end, with nothing between.
    assert_eq!(completed_at, finished_at + 1, "{kinds:?}");
    let finished = ordered[finished_at]["payload"].clone();
    let done = ordered[completed_at]["payload"].clone();
    assert_eq!(finished["job_id"], job);
    assert_eq!(done["job_id"], job);
    assert_eq!(done["status"], "failed");
    assert_eq!(done["error"]["code"], expected_code);
    assert_eq!(done["error"]["message"], expected_message);
    assert_ne!(
        done["error"]["message"],
        "The delegate ended without a result."
    );
    assert_eq!(done["process"]["exit_code"], 1);
    // Nothing was ever written to finish with, and no model call ran.
    assert_eq!(finished["text"], "");
    assert_eq!(finished["usage"]["tokens"]["input"], 0);
    assert_eq!(finished["usage"]["tokens"]["output"], 0);

    // The wait and the list each ran once, found by their own call: two
    // calls share the `jobs` name, so the shared helper cannot tell them
    // apart.
    let requested = |action: &str| {
        lines
            .iter()
            .find(|line| {
                line["kind"] == "tool_call_requested"
                    && line["payload"]["name"] == "jobs"
                    && line["payload"]["arguments"]["action"] == action
            })
            .unwrap_or_else(|| panic!("no jobs {action} call in {kinds:?}"))
            .clone()
    };
    let wait_action = requested("wait")["action_id"].as_str().unwrap().to_owned();
    let list_action = requested("list")["action_id"].as_str().unwrap().to_owned();
    assert_ne!(wait_action, list_action);
    let completed = |action: &str| {
        lines
            .iter()
            .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == action)
            .unwrap_or_else(|| panic!("no completion for {action} in {kinds:?}"))
            .clone()
    };
    let wait_done = completed(&wait_action);
    let list_done = completed(&list_action);
    assert_eq!(wait_done["payload"]["status"], "completed");
    let wait_text = wait_done["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(wait_text.contains("failed"), "{wait_text}");
    assert!(wait_text.contains(&expected_message), "{wait_text}");
    // Whether the failure landed at a step boundary or in the wait, the
    // pair carries the run's own id: a notice carries none, a claimed end
    // carries the wait's.
    let pair_action = ordered[finished_at]["action_id"].clone();
    assert!(
        pair_action.is_null() || pair_action == json!(wait_action),
        "{pair_action}"
    );

    // The same parent's list shows the stored error and no log path: the
    // child's log was never created, and the receipt's path is gone whole.
    let spawn_done = completed_for(&lines, "delegate_spawn");
    let receipt = spawn_done["payload"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(receipt.contains(&job), "{receipt}");
    let marker = "Its log is ";
    let receipt_path = receipt[receipt.find(marker).unwrap() + marker.len()..]
        .trim_end_matches('.')
        .to_owned();
    assert!(receipt_path.ends_with("events.jsonl"), "{receipt}");
    assert!(receipt_path.contains(&child_id), "{receipt}");
    assert_eq!(list_done["payload"]["status"], "completed");
    let list_text = list_done["payload"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        list_text,
        format!("{job} failed scan \u{2014} {expected_message}\n")
    );
    assert!(!list_text.contains(&receipt_path), "{list_text}");
    let child_dir = sessions_in(&setup.home(), &setup.workspace()).join(&child_id);
    assert!(!child_dir.exists(), "the child made a session directory");
    assert!(
        !child_dir.join("events.jsonl").exists(),
        "the child wrote a log"
    );

    // The whole parent stream, in order, and its durable lines equal its
    // log: a duplicated, missing or reordered event fails here. Either
    // finish placement is valid, so the stream must equal one of the two
    // pinned orders.
    let durable = durable_kinds(&lines);
    assert!(
        durable == EARLY_FAILURE_PARENT_DURABLE_WAIT
            || durable == EARLY_FAILURE_PARENT_DURABLE_DRAIN,
        "{durable:?}"
    );
    let parent_id = lines[0]["session_id"].as_str().unwrap();
    let logged = read_lines(&ask_sessions(&setup).join(parent_id).join("events.jsonl"));
    let durable_stdout: Vec<Value> = lines
        .iter()
        .filter(|line| line.get("seq").is_some())
        .cloned()
        .collect();
    assert_eq!(durable_stdout, logged);
}

// `delegate_message` at binary level (see #540): steering a running
// delegate at its next step boundary, resuming a finished one as a second
// run of the same job, refusing a job this session never started, and
// refusing at depth 2. Every test arms the watchdog first, runs `fiber`
// through `Running`, and waits only through `Running::wait_line`,
// `Running::wait`, `ProviderServer::await_requests` and `support`
// receives, all under the test's one deadline. No test sleeps or reads a
// clock.

/// The parent's durable kinds for the steer run, in order, pinned from a
/// real run: the spawn turn through its receipt, the message call with no
/// job records, the hello step ending the call's turn, the ending notice
/// while the delegate still runs, and the wake carrying its finish.
const STEER_PARENT_DURABLE: &[&str] = &[
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
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "usage_recorded",
    "permission_resolved",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "turn_started",
    "step_started",
    "jobs_pending_notified",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn delegate_message_steers_a_running_delegate_at_its_next_step_boundary() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(answering(
        support::stream(&[spawn_call("fiber:pb/m")]),
        text_reply("allow"),
        "call_message",
        |job| {
            support::stream(&[function_call(
                "call_message",
                "delegate_message",
                &json!({"id": job, "message": MARK}),
            )])
        },
        support::hello(),
    ))
    .unwrap();
    parent.hold();
    let child = ProviderServer::start([text_reply("first"), text_reply("after-steer")]).unwrap();
    child.hold();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let mut running = Running::spawn(&setup, &["ask", "scan the tree"]);
    assert!(
        parent.await_requests(1, setup.deadline),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, setup.deadline),
        "the verdict was requested"
    );
    parent.release_one();
    assert!(
        child.await_requests(1, setup.deadline),
        "the delegate's first step is in flight"
    );
    parent.release();
    // The delta proves the steer reached the delegate's inbox: the probe
    // is answered once the delegate's reader queued the message, and the
    // steer joins at the step boundary after the released reply. A held
    // stream is no admission point, so the run is released only after it.
    let mut job = String::new();
    running.wait_line("the queued delta", |line| {
        if line["kind"] == "delegate_started" {
            job = line["payload"]["job_id"].as_str().unwrap().to_owned();
        }
        !job.is_empty()
            && line["kind"] == "tool_call_delta"
            && line["payload"]["text"].as_str()
                == Some(&format!(
                    "Queued for delegate {job}; it joins at the delegate's next step boundary."
                ))
    });
    child.release_one();
    assert!(
        child.await_requests(2, setup.deadline),
        "the delegate's second model call after the steer"
    );
    let mut ended = 0;
    running.wait_line("the call's and ending notice's turn ends", |line| {
        ended += usize::from(line["kind"] == "turn_completed");
        ended == 2
    });
    child.release();
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    assert!(!job.is_empty(), "the run started a delegate");
    let bodies: Vec<String> = child
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
    let delegate_id = lines
        .iter()
        .find(|line| line["kind"] == "delegate_started")
        .unwrap()["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child_lines = read_lines(&ask_sessions(&setup).join(&delegate_id).join("events.jsonl"));
    assert_eq!(durable_kinds(&child_lines), DELEGATE_DURABLE);
    let applied = one(&child_lines, "steering_applied");
    assert_eq!(applied["source"], "driver");
    assert!(
        serde_json::to_string(&applied["content"])
            .unwrap()
            .contains(MARK),
        "{applied}"
    );
    let completed = completed_for(&lines, "delegate_message");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(
        completed["payload"]["content"][0]["text"],
        format!("Steered delegate {job}: the message joins its turn at its next step boundary.")
    );
    let action = completed["action_id"].as_str().unwrap();
    let started = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started" && line["action_id"] == action)
        .unwrap();
    assert_eq!(started["payload"]["effects"], json!(["executes"]));
    assert_eq!(started["payload"]["reversible"], false);
    assert!(
        started["payload"]
            .get("always_reviewed")
            .is_none_or(|flag| flag == &json!(true)),
        "{started}"
    );
    let resolved = lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved" && line["action_id"] == action)
        .unwrap();
    assert_eq!(resolved["payload"]["decided_by"], "reviewer");
    for kind in ["job_started", "job_completed"] {
        let found: Vec<&Value> = lines.iter().filter(|line| line["kind"] == kind).collect();
        assert_eq!(found.len(), 1, "{kind} wrote {:?}", owned_kinds(&lines));
        assert_eq!(found[0]["payload"]["job_id"], job.as_str());
    }
    let finished = one(&lines, "delegate_finished");
    assert_eq!(finished["text"], "after-steer");
    assert_eq!(durable_kinds(&without_copies(&lines)), STEER_PARENT_DURABLE);
}

/// The resume message carried to the delegate's second run: unique to it,
/// so its absence from the first run proves the blank call started
/// nothing.
const RESUME_MARK: &str = "resume-marker-9d2b4e77";

/// The resumed run's child durable kinds, in order: one `session_started`,
/// then the resumed run's lines, with no second `session_started` and no
/// `opening_message`.
const RESUMED_CHILD_DURABLE: &[&str] = &[
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The parent's durable kinds for the resume run, in order, pinned from a
/// real run: the spawn turn and the ending notice, the wake turn carrying
/// the blank call's refusal and the resume, the run's own wake, and the
/// exit. One `jobs_pending_notified`: the ending notice fires once per
/// process, so the resume's wake turn carries none.
const RESUME_PARENT_DURABLE: &[&str] = &[
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
    "jobs_pending_notified",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "turn_started",
    "step_started",
    "delegate_finished",
    "job_completed",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "usage_recorded",
    "permission_resolved",
    "tool_call_started",
    "tool_call_completed",
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
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn delegate_message_resumes_a_finished_delegate_as_a_second_run_of_the_same_job() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains("run-two-done") {
            return support::hello();
        }
        if text.contains("Resumed delegate") {
            return support::hello();
        }
        if text.contains("must not be empty") {
            let job = job_id(&text);
            return support::stream(&[function_call(
                "call_resume",
                "delegate_message",
                &json!({"id": job, "message": RESUME_MARK}),
            )]);
        }
        if text.contains("run-one-done") {
            let job = job_id(&text);
            return support::stream(&[function_call(
                "call_blank",
                "delegate_message",
                &json!({"id": job, "message": " \n\t"}),
            )]);
        }
        if text.contains("Started delegate") {
            return support::hello();
        }
        support::stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    let child =
        ProviderServer::start([text_reply("run-one-done"), text_reply("run-two-done")]).unwrap();
    child.hold();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let mut running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let mut ended = 0;
    running.wait_line("the spawn and ending notice's turn ends", |line| {
        ended += usize::from(line["kind"] == "turn_completed");
        ended == 2
    });
    child.release_one();
    assert!(
        child.await_requests(2, setup.deadline),
        "the resumed run's model call"
    );
    let mut re_ended = 0;
    running.wait_line("the wake turn's end", |line| {
        re_ended += usize::from(line["kind"] == "turn_completed");
        re_ended == 1
    });
    child.release();
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    let bodies: Vec<String> = child
        .requests()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(
        !bodies[0].contains(RESUME_MARK),
        "the blank call started nothing"
    );
    assert!(
        bodies[1].contains(RESUME_MARK),
        "the resume carried the message"
    );
    // The two message calls, in request order: the blank refusal, then
    // the resume.
    let requested: Vec<&Value> = lines
        .iter()
        .filter(|line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "delegate_message"
        })
        .collect();
    assert_eq!(requested.len(), 2, "{:?}", owned_kinds(&lines));
    fn action_of(line: &Value) -> &str {
        line["action_id"].as_str().unwrap()
    }
    let blank_action = action_of(requested[0]);
    let resume_action = action_of(requested[1]);
    let completed = |action: &str| {
        lines
            .iter()
            .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == action)
            .unwrap()
    };
    let blank_done = completed(blank_action);
    assert_eq!(blank_done["payload"]["status"], "failed");
    assert_eq!(blank_done["payload"]["error"]["code"], "invalid_arguments");
    assert_eq!(
        blank_done["payload"]["error"]["message"],
        "`message` must not be empty."
    );
    // One id for life: two runs of the same job, each writing its start
    // pair before its finish pair.
    let of_kind = |kind: &str| {
        lines
            .iter()
            .filter(|line| line["kind"] == kind)
            .collect::<Vec<&Value>>()
    };
    let job = of_kind("job_started")[0]["payload"]["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    for kind in [
        "job_started",
        "delegate_started",
        "delegate_finished",
        "job_completed",
    ] {
        let found = of_kind(kind);
        assert_eq!(found.len(), 2, "{kind} wrote {:?}", owned_kinds(&lines));
        for line in &found {
            assert_eq!(line["payload"]["job_id"], job.as_str());
        }
    }
    let run_kinds: Vec<&str> = lines
        .iter()
        .filter(|line| {
            matches!(
                line["kind"].as_str(),
                Some("job_started" | "delegate_started" | "delegate_finished" | "job_completed")
            )
        })
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        run_kinds,
        [
            "job_started",
            "delegate_started",
            "delegate_finished",
            "job_completed",
            "job_started",
            "delegate_started",
            "delegate_finished",
            "job_completed",
        ],
        "{run_kinds:?}"
    );
    // The resume is a new run of the same job: the resuming call's action
    // and turn on the second start pair, and the first run's description,
    // log and session carried over.
    let starts = of_kind("job_started");
    let run2 = starts[1];
    assert_eq!(run2["payload"]["tool"], "delegate_message");
    assert_eq!(
        run2["payload"]["description"],
        starts[0]["payload"]["description"]
    );
    assert_eq!(
        run2["payload"]["output_path"],
        starts[0]["payload"]["output_path"]
    );
    let resume_started = lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started" && line["action_id"] == resume_action)
        .unwrap();
    assert_eq!(run2["action_id"], resume_started["action_id"]);
    assert_eq!(run2["turn_id"], resume_started["turn_id"]);
    let delegates = of_kind("delegate_started");
    assert_eq!(delegates[1]["action_id"], resume_started["action_id"]);
    assert_eq!(delegates[1]["turn_id"], resume_started["turn_id"]);
    assert_eq!(
        delegates[1]["payload"]["delegate_session_id"],
        delegates[0]["payload"]["delegate_session_id"]
    );
    assert_eq!(
        delegates[1]["payload"]["model"],
        delegates[0]["payload"]["model"]
    );
    // The resume's start is queued and written through the call's run
    // before its answer: the start pair falls inside the call.
    let position = |wanted: &Value| lines.iter().position(|line| line == wanted).unwrap();
    let resume_done = completed(resume_action);
    assert!(position(resume_started) < position(run2));
    assert!(position(run2) < position(delegates[1]));
    assert!(position(delegates[1]) < position(resume_done));
    let run1_done = position(of_kind("job_completed")[0]);
    assert!(run1_done < position(requested[0]));
    assert_eq!(resume_done["payload"]["status"], "completed");
    assert_eq!(
        resume_done["payload"]["content"][0]["text"],
        format!(
            "Resumed delegate {job}. Its log is {}.",
            starts[0]["payload"]["output_path"].as_str().unwrap()
        )
    );
    let resume_resolved = lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved" && line["action_id"] == resume_action)
        .unwrap();
    assert_eq!(resume_resolved["payload"]["decided_by"], "reviewer");
    // The delegate's log: one run, then the resumed run's lines, with the
    // message in the second turn's input and the second finish naming the
    // second reply.
    let delegate_id = delegates[0]["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child_lines = read_lines(&ask_sessions(&setup).join(&delegate_id).join("events.jsonl"));
    let mut expected: Vec<String> = ROLE_CHILD_DURABLE
        .iter()
        .map(|kind| kind.to_string())
        .collect();
    expected.extend(RESUMED_CHILD_DURABLE.iter().map(|kind| kind.to_string()));
    assert_eq!(durable_kinds(&child_lines), expected);
    let fibered: Vec<&Value> = child_lines
        .iter()
        .filter(|line| line["kind"] == "fiber_started")
        .collect();
    assert_eq!(fibered.len(), 2);
    assert_eq!(fibered[1]["payload"]["resumed"], true);
    let turns: Vec<&Value> = child_lines
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .collect();
    assert_eq!(turns.len(), 2);
    assert!(
        serde_json::to_string(&turns[1]["payload"]["input"])
            .unwrap()
            .contains(RESUME_MARK),
        "{}",
        turns[1]["payload"]["input"]
    );
    let finished = of_kind("delegate_finished");
    assert_eq!(finished[0]["payload"]["text"], "run-one-done");
    assert_eq!(finished[1]["payload"]["text"], "run-two-done");
    assert_eq!(
        durable_kinds(&without_copies(&lines)),
        RESUME_PARENT_DURABLE
    );
}

/// The parent's durable kinds for the unknown-job run, in order, pinned
/// from a real run: one step with two calls, the reviewer's verdict for
/// the message call, each call's start and completion in request order,
/// then the hello step and the exit.
const UNKNOWN_PARENT_DURABLE: &[&str] = &[
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "preamble_built",
    "opening_message",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "usage_recorded",
    "permission_resolved",
    "tool_call_started",
    "tool_call_started",
    "tool_call_completed",
    "tool_call_completed",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

#[test]
fn delegate_message_to_a_job_this_session_never_started_fails_as_jobs_does() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let missing = doors::mint("j_");
    let missing_call = missing.clone();
    let parent = ProviderServer::start_responding(move |request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"jobs""#) {
            return text_reply("allow");
        }
        if text.contains("No job has id") {
            return support::hello();
        }
        support::stream(&[
            function_call(
                "call_wait",
                "jobs",
                &json!({"action": "wait", "job_id": missing_call.clone(), "timeout_ms": 1000}),
            ),
            function_call(
                "call_message",
                "delegate_message",
                &json!({"id": missing_call.clone(), "message": "hi"}),
            ),
        ])
    })
    .unwrap();
    let child = ProviderServer::start([]).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    let missing_text = format!("No job has id `{missing}`.\n");
    let waited = completed_for(&lines, "jobs");
    let messaged = completed_for(&lines, "delegate_message");
    for done in [&waited, &messaged] {
        assert_eq!(done["payload"]["status"], "failed");
        assert_eq!(done["payload"]["error"]["code"], "invalid_arguments");
        assert_eq!(done["payload"]["content"][0]["text"], missing_text);
    }
    assert_eq!(
        waited["payload"]["error"]["message"],
        messaged["payload"]["error"]["message"]
    );
    assert!(
        lines
            .iter()
            .all(|line| line["kind"] != "job_started" && line["kind"] != "delegate_started"),
        "a refused message started nothing: {:?}",
        owned_kinds(&lines)
    );
    assert!(child.requests().is_empty());
    assert_eq!(durable_kinds(&lines), UNKNOWN_PARENT_DURABLE);
}

#[test]
fn a_depth_two_session_refuses_delegate_message() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(|request: &Request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        if body["tools"]
            .as_array()
            .is_none_or(|tools| tools.is_empty())
        {
            return text_reply("allow");
        }
        let input = body["input"].as_array().unwrap();
        if input
            .iter()
            .any(|item| item["type"] == "function_call_output")
        {
            support::hello()
        } else {
            support::stream(&[function_call(
                "call_message",
                "delegate_message",
                &json!({"id": "j_0123456789abcdef", "message": "hi"}),
            )])
        }
    })
    .unwrap();
    let child = ProviderServer::start([support::hello()]).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    let id = doors::mint("s_");
    let parent_id = doors::mint("s_");
    let job = doors::mint("j_");
    let (mut running, lifeline) = Running::spawn_stdin(
        &setup,
        &[
            "session",
            "--id",
            &id,
            "--workspace",
            setup.root.path().to_str().unwrap(),
            "--prompt=delegate",
            "--parent",
            &parent_id,
            "--delegate-id",
            &job,
            "--depth",
            "2",
        ],
        Stdio::piped(),
    );
    running.wait_line("the depth-two session exits", |line| {
        line["kind"] == "fiber_exited"
    });
    drop(lifeline);
    let (status, _lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "{stderr}");
    let lines = read_lines(&ask_sessions(&setup).join(&id).join("events.jsonl"));
    assert_depth_refusal(&lines, false, false, "delegate_message");
    let completed = completed_for(&lines, "delegate_message");
    assert_eq!(
        completed["payload"]["error"]["message"],
        "This delegate is at depth 2, the deepest a delegate can be, \
         so it cannot use `delegate_message`; do the work in this session."
    );
    assert!(child.requests().is_empty());
}

/// Runs one `ask` whose first reply spawns `model`: `roles` are configured
/// and `stored` stored for provider `pb`. Waits for the delegate to finish
/// and the session to exit, and returns the parent's lines with the child's
/// `preamble_built` payload.
fn delegate_child_preamble(model: &str, roles: &Value, stored: &[&str]) -> (Vec<Value>, Value) {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call(model)]),
        text_reply("allow"),
        support::hello(),
        support::hello(),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([text_reply("done")]).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);
    for label in stored {
        config::store_credential(
            &setup.home(),
            "pb",
            label,
            &config::Secret::new(format!("{label}-key")),
        )
        .unwrap();
    }
    let path = setup.home().join("config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["roles"] = roles.clone();
    support::write_json(&path, &config);

    let mut running = Running::spawn(&setup, &["ask", "scan the tree"]);
    running.wait_line("the delegate to finish", |line| {
        line["kind"] == "delegate_finished"
    });
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    let delegate = one(&lines, "delegate_started");
    let child_id = delegate["delegate_session_id"].as_str().unwrap();
    let child_lines = read_lines(&ask_sessions(&setup).join(child_id).join("events.jsonl"));
    let preamble = one(&child_lines, "preamble_built");
    (lines, preamble)
}

#[test]
fn a_delegate_started_with_a_role_runs_on_the_role_label() {
    let (lines, preamble) = delegate_child_preamble(
        "w",
        &json!({"w": {"model": "fiber:pb/m", "credential": "work"}}),
        &["work"],
    );
    // The child's session records the role's label, while the provider's
    // configured label is `default`.
    assert_eq!(preamble["credential"], "work");
    let completed = completed_for(&lines, "delegate_spawn");
    assert_eq!(completed["payload"]["status"], "completed");

    // A second spawn with a full reference records the configured label.
    let (lines, preamble) = delegate_child_preamble("fiber:pb/m", &json!({}), &[]);
    assert_eq!(preamble["credential"], "default");
    let completed = completed_for(&lines, "delegate_spawn");
    assert_eq!(completed["payload"]["status"], "completed");
}

#[test]
fn a_role_naming_a_missing_label_fails_the_spawn_with_credential_missing() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start([
        support::stream(&[spawn_call("w")]),
        text_reply("allow"),
        support::hello(),
    ])
    .unwrap();
    let child = ProviderServer::start([text_reply("unused")]).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);
    // `work` is stored, but the role names `gone`.
    config::store_credential(
        &setup.home(),
        "pb",
        "work",
        &config::Secret::new("work-key".to_owned()),
    )
    .unwrap();
    let path = setup.home().join("config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["roles"] = json!({"w": {"model": "fiber:pb/m", "credential": "gone"}});
    support::write_json(&path, &config);

    let mut running = Running::spawn(&setup, &["ask", "scan the tree"]);
    running.wait_line("the refused spawn", |line| {
        line["kind"] == "tool_call_completed"
    });
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    let completed = completed_for(&lines, "delegate_spawn");
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "credential_missing");
    assert_eq!(
        completed["payload"]["error"]["message"],
        "`pb` has no credential label `gone`. The labels for `pb` are: default, work"
    );
    assert!(lines.iter().all(|line| line["kind"] != "delegate_started"));
    assert!(child.requests().is_empty());
}

/// The probe extension: its command calls `host.delegate` with an unknown
/// key under `pcall` and writes the raised code to its data directory.
const PROBE: &str = "fiber.command(\"probe\", { timeout = 8000, run = function()\n\
  local ok, err = pcall(host.delegate, { description = \"scan\", bogus = true }, { timeout = 5000, run = function() end })\n\
  local code = ok and \"started\" or err.code\n\
  host.fs.write(host.data_dir(\"machine\") .. \"/code\", code)\n\
  host.status(code)\n\
end })\n";

/// The code the probe wrote, once its command has run: the one directory
/// under `data/` is the probe's, whatever the slug rule spells it.
fn probe_code(setup: &support::Setup) -> Option<String> {
    let data = setup.home().join("data");
    let slug = fs::read_dir(&data)
        .ok()?
        .filter_map(|entry| entry.ok())
        .find(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))?
        .file_name();
    fs::read_to_string(data.join(slug).join("code")).ok()
}

/// `host.delegate` reaches the session's service in a new session and
/// again in a resumed one: an unknown key reads `invalid_arguments`, not
/// `closing`, and no child starts.
#[test]
fn an_extension_command_reaches_host_delegate_in_a_new_and_a_resumed_session() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let server = ProviderServer::start([support::hello(), support::hello()]).unwrap();
    setup.provider(&server);
    setup.lua("probe", PROBE);
    let id = doors::mint("s_");
    // `--prompt` runs a turn, so the session counts as prompted and its
    // directory survives the close for the resume; the probe still runs
    // through the driver, beside the turn. A resume takes no prompt: only
    // a delegate resumes with one.
    for resume in [false, true] {
        let mut args = vec![];
        if resume {
            args.push("--resume");
        } else {
            args.extend(["--prompt", "say hi"]);
        }
        let mut running = setup.start_session(&id, &args);
        // A prompted session prints its first line before its socket is
        // bound, so the connect below would miss it: wait for the socket
        // and the load together, keeping every line for the drain `wait`
        // collects at the end.
        let mut loaded = false;
        for _ in 0..400 {
            if setup.socket(&id).exists() && loaded {
                break;
            }
            if let Ok(line) =
                fakes::Deadline::after(Duration::from_millis(100)).recv(&running.lines)
            {
                let value: Value = serde_json::from_str(&line).unwrap();
                loaded = loaded || value["kind"] == "extensions_loaded";
                running.first.push(line);
            }
        }
        assert!(loaded, "the session loads its extensions");
        let client = running.connect(&setup.socket(&id));
        send(
            &client,
            r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
        );
        send(
            &client,
            r#"{"id":"c_probe","command":"command","args":{"name":"probe"}}"#,
        );
        // The status arrives after the write in the command's run, so the
        // file it names reads the raised code with no poll.
        let _seen = until_session(&client, "the probe status", |line| {
            line["kind"] == "extension_ui" && line["payload"]["extension"] == "fiber.test/probe"
        });
        let code = probe_code(&setup);
        assert_eq!(code.as_deref(), Some("invalid_arguments"));
        send(&client, r#"{"id":"c_close","command":"close"}"#);
        let _tail = until_session_close(&client);
        drop(client);
        let (status, out, stderr) = running.wait();
        if code.is_none() {
            let kinds: Vec<_> = out
                .iter()
                .map(|line| line["kind"].as_str().unwrap_or("?"))
                .collect();
            eprintln!("KINDS: {kinds:?}\nSTDERR: {stderr}");
        }
        assert!(status.success(), "stderr: {stderr}");
        assert!(
            out.iter()
                .all(|line| line["kind"] != "delegate_started" && line["kind"] != "job_started"),
            "no child starts"
        );
        let data = setup.home().join("data");
        for entry in fs::read_dir(&data).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                fs::remove_file(entry.path().join("code")).unwrap();
            }
        }
    }
    // The new session's prompt turn, and nothing else: the resume runs no
    // turn, and no delegate child ran one of its own against the server.
    assert_eq!(server.requests().len(), 1);
}

#[test]
fn a_finished_delegate_replays_its_transcript_to_a_full_subscribe() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains("Started delegate") {
            return support::hello();
        }
        support::stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    let child =
        ProviderServer::start_responding(|_: &Request| text_reply("the scan is done")).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    // The first wait: the delegate runs to completion and the parent
    // exits on its own.
    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    let done = one(&lines, "job_completed");
    assert_eq!(done["status"], "completed");
    let started = one(&lines, "delegate_started");
    let delegate = started["delegate_session_id"].as_str().unwrap().to_owned();
    let parent_id = lines[0]["session_id"].as_str().unwrap().to_owned();

    // The second wait: a `full` subscribe to the finished delegate
    // session replays its transcript from `session_started`.
    let hub_slot = Arc::new(Mutex::new(None));
    let (hub, _) = support::connect_hub(&setup, &hub_slot);
    hub.send(&format!(
        "{{\"id\":\"c_dsub\",\"session_id\":\"{delegate}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let ack = support::recv_answer(&hub, "c_dsub", "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted", "{ack}");
    let fold = support::until(&hub, "the finished delegate's replayed fold", |line| {
        line.get("session_id").and_then(Value::as_str) == Some(delegate.as_str())
            && line["kind"] == "fiber_exited"
    });
    let started_line = fold
        .iter()
        .find(|line| {
            line["kind"] == "session_started"
                && line.get("session_id").and_then(Value::as_str) == Some(delegate.as_str())
        })
        .unwrap_or_else(|| panic!("no session_started in {fold:?}"));
    assert_eq!(
        started_line["payload"]["parent"]["session_id"], parent_id,
        "{started_line}"
    );
    if let Some(proc) = hub_slot.lock().unwrap().take() {
        proc.kill_and_wait();
    }
}
/// A `fiber session` answering text on every model call: what a fork
/// child's one turn reads as.
fn fork_text(_request: &Request) -> Response {
    text_reply("forked hello")
}

/// Starts a fork child of `parent` directly, continuing `from` at `seq`:
/// the argv `command` builds for a `Fresh(Fork)` launch, with `stdin`.
fn start_fork_child(
    setup: &support::Setup,
    new: &str,
    from: &str,
    seq: u64,
    parent: &str,
    job: &str,
    stdin: Stdio,
) -> (Running, Option<std::process::ChildStdin>) {
    let seq = seq.to_string();
    let workspace = setup.workspace().to_str().unwrap().to_owned();
    Running::spawn_stdin(
        setup,
        &[
            "session",
            "--id",
            new,
            "--workspace",
            &workspace,
            "--prompt",
            "check it",
            "--forked-from",
            from,
            "--forked-seq",
            &seq,
            "--parent",
            parent,
            "--delegate-id",
            job,
            "--depth",
            "1",
        ],
        stdin,
    )
}

/// The new session holds no log: a fork that writes `session_started`
/// first fails this.
fn assert_no_fork_log(setup: &support::Setup, new: &str) {
    let log = sessions_in(&setup.home(), &setup.workspace())
        .join(new)
        .join("events.jsonl");
    assert!(
        !log.is_file(),
        "a fork that cannot start writes no session log"
    );
}

/// A parent session with one completed turn: the setup the fork tests
/// share, its armed watchdog guard, the responding providers that must
/// stay alive while the fork child runs, the parent id, its sessions
/// directory and its log lines. Both fork tests start from this parent
/// and then pick the seq they fork from.
struct ForkParent {
    setup: support::Setup,
    _guard: SessionGuard,
    _servers: (ProviderServer, ProviderServer),
    parent_id: String,
    sessions: PathBuf,
    parent_lines: Vec<Value>,
}

/// Starts the parent both fork tests share: one turn on `pa/m`, closed
/// once the turn completes, so the log holds a preamble before its last
/// line. A plain session idles after its turn, so this closes it once
/// the turn completes.
#[track_caller]
fn fork_parent() -> ForkParent {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(fork_text).unwrap();
    let child = ProviderServer::start_responding(fork_text).unwrap();
    providers(&setup, &parent, &child);
    let parent_id = doors::mint("s_");
    let mut parent_running = Running::start(
        &setup,
        &parent_id,
        &["--model", "pa/m", "--prompt", "say hello"],
    );
    parent_running.wait_line("the parent's turn completes", |line| {
        line["kind"] == "turn_completed"
    });
    let client = parent_running.connect(&setup);
    subscribe(&client);
    close_now(&client);
    let _tail = support::until_close(&client);
    drop(client);
    let (parent_status, _, parent_stderr) = parent_running.wait();
    assert_eq!(parent_status.code(), Some(0), "{parent_stderr}");
    let sessions = sessions_in(&setup.home(), &setup.workspace());
    let parent_lines = read_lines(&sessions.join(&parent_id).join("events.jsonl"));
    ForkParent {
        setup,
        _guard,
        _servers: (parent, child),
        parent_id,
        sessions,
        parent_lines,
    }
}

#[test]
fn a_fork_child_that_cannot_start_fails_before_any_session_log() {
    let fork = fork_parent();
    let setup = &fork.setup;
    let parent_id = &fork.parent_id;
    let parent_lines = &fork.parent_lines;
    assert_eq!(parent_lines.first().unwrap()["kind"], "session_started");
    let first_seq = parent_lines.first().unwrap()["seq"].as_u64().unwrap();
    assert!(
        parent_lines
            .iter()
            .any(|line| line["kind"] == "preamble_built"),
        "the parent built its preamble"
    );
    // Case (a): `--forked-from` names a missing session.
    let missing = doors::mint("s_");
    let new_a = doors::mint("s_");
    let (failed_a, _) = start_fork_child(
        setup,
        &new_a,
        &missing,
        3,
        &missing,
        &doors::mint("j_"),
        Stdio::null(),
    );
    let (status, lines, stderr) = failed_a.wait();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert_eq!(
        one(&lines, "fiber_exited")["error"]["code"],
        "session_not_found",
        "{lines:?}"
    );
    assert_no_fork_log(setup, &new_a);
    // Case (b): `--forked-from` names a corrupt log.
    let corrupt = doors::mint("s_");
    let corrupt_dir = fork.sessions.join(&corrupt);
    fs::create_dir_all(&corrupt_dir).unwrap();
    fs::write(corrupt_dir.join("events.jsonl"), "this is not a log line\n").unwrap();
    let new_b = doors::mint("s_");
    let (failed_b, _) = start_fork_child(
        setup,
        &new_b,
        &corrupt,
        1,
        &corrupt,
        &doors::mint("j_"),
        Stdio::null(),
    );
    let (status, lines, stderr) = failed_b.wait();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert_eq!(
        one(&lines, "fiber_exited")["error"]["code"],
        "log_corrupt",
        "{lines:?}"
    );
    assert_no_fork_log(setup, &new_b);
    // Case (c): the point precedes any `preamble_built`, so there is no
    // build to fork from.
    let new_c = doors::mint("s_");
    let (failed_c, _) = start_fork_child(
        setup,
        &new_c,
        parent_id,
        first_seq,
        parent_id,
        &doors::mint("j_"),
        Stdio::null(),
    );
    let (status, lines, stderr) = failed_c.wait();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert_eq!(
        one(&lines, "fiber_exited")["error"]["code"],
        "invalid_arguments",
        "{lines:?}"
    );
    assert_no_fork_log(setup, &new_c);
    // Case (d): the point's session directory cannot be listed at all.
    // A self-looping link fails the metadata read with an error other
    // than missing, so the failure is `io_failed`, not
    // `session_not_found`.
    let looped = doors::mint("s_");
    let looped_link = fork.sessions.join(&looped);
    std::os::unix::fs::symlink(&looped_link, &looped_link).unwrap();
    let new_d = doors::mint("s_");
    let (failed_d, _) = start_fork_child(
        setup,
        &new_d,
        &looped,
        1,
        &looped,
        &doors::mint("j_"),
        Stdio::null(),
    );
    let (status, lines, stderr) = failed_d.wait();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert_eq!(
        one(&lines, "fiber_exited")["error"]["code"],
        "io_failed",
        "{lines:?}"
    );
    assert_no_fork_log(setup, &new_d);
    // Case (e): a file where the point's session directory belongs is
    // no session either: `session_not_found`, as a missing one.
    let filed = doors::mint("s_");
    fs::write(fork.sessions.join(&filed), "not a session\n").unwrap();
    let new_e = doors::mint("s_");
    let (failed_e, _) = start_fork_child(
        setup,
        &new_e,
        &filed,
        1,
        &filed,
        &doors::mint("j_"),
        Stdio::null(),
    );
    let (status, lines, stderr) = failed_e.wait();
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert_eq!(
        one(&lines, "fiber_exited")["error"]["code"],
        "session_not_found",
        "{lines:?}"
    );
    assert_no_fork_log(setup, &new_e);
}

#[test]
fn a_fork_child_of_a_worktree_parent_holds_the_shared_lock() {
    let fork = fork_parent();
    let setup = &fork.setup;
    let parent_id = &fork.parent_id;
    let parent_log = fork.sessions.join(parent_id).join("events.jsonl");
    let parent_lines = read_lines(&parent_log);
    let last_seq = parent_lines.last().unwrap()["seq"].as_u64().unwrap();
    // The parent ran in a worktree: its `session_started` names one at
    // a directory that exists, so the child can share it.
    let worktree = setup.root.path().join("wt");
    fs::create_dir(&worktree).unwrap();
    let text = fs::read_to_string(&parent_log).unwrap();
    let mut rewritten = String::new();
    for (index, line) in text.lines().enumerate() {
        if index == 0 {
            let mut first: Value = serde_json::from_str(line).unwrap();
            assert_eq!(first["kind"], "session_started");
            first["payload"]["worktree"] = json!({
                "path": worktree.to_str().unwrap(),
                "branch": "fork-test",
                "repository": setup.root.path().to_str().unwrap(),
            });
            rewritten.push_str(&serde_json::to_string(&first).unwrap());
        } else {
            rewritten.push_str(line);
        }
        rewritten.push('\n');
    }
    fs::write(&parent_log, rewritten).unwrap();
    // The lock file sits beside the worktree, as `doors` lays it out:
    // an exclusive take fails while a share is held, and succeeds
    // once every hold is dropped.
    let lock = worktree.with_file_name(format!(
        "{}.lock",
        worktree.file_name().unwrap().to_str().unwrap()
    ));
    let job = doors::mint("j_");
    let new = doors::mint("s_");
    // Stdin held open: it is the delegate's lifeline.
    let (mut running, lifeline) = start_fork_child(
        setup,
        &new,
        parent_id,
        last_seq,
        parent_id,
        &job,
        Stdio::piped(),
    );
    running.wait_line("the fork child starts", |line| {
        line["kind"] == "fiber_started"
    });
    let probe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    assert!(
        matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "an exclusive take fails while the child shares the worktree"
    );
    drop(probe);
    // The child names its parent and its point, and no rewind.
    let started = one(
        &read_lines(&fork.sessions.join(&new).join("events.jsonl")),
        "session_started",
    );
    assert_eq!(
        started["parent"],
        json!({"session_id": parent_id, "delegate_id": job})
    );
    assert_eq!(
        started["forked_from"],
        json!({"session_id": parent_id, "seq": last_seq})
    );
    assert!(started.get("rewind").is_none(), "{started}");
    assert_eq!(started["worktree"]["path"], worktree.to_str().unwrap());
    // Its run ended on its own: one turn and out, with no idle wait.
    running.wait_line("the fork child exits", |line| {
        line["kind"] == "fiber_exited"
    });
    drop(lifeline);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert!(
        lines.iter().any(|line| line["kind"] == "turn_completed"),
        "the child ran its prompt as one turn: {:?}",
        lines.iter().map(|line| &line["kind"]).collect::<Vec<_>>()
    );
    // The lock releases with the child.
    let freed = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock)
        .unwrap();
    assert!(
        freed.try_lock().is_ok(),
        "an exclusive take succeeds once the child has exited"
    );
}

/// The finished Delegates-card row: the `delegate_spawn` description the
/// scripted parent sends, with the finished glyph.
const FINISHED_ROW: &str = "✓ scan";
/// The delegate child's final message: what the replayed transcript shows.
const CHILD_TEXT: &str = "the scan is done";

/// The first row containing `needle`, or a panic naming it.
fn row_with(grid: &Grid, needle: &str) -> u16 {
    u16::try_from(
        grid.rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} in\n{}", grid.contents)),
    )
    .unwrap_or(u16::MAX)
}

/// The column where `needle` starts on `y`, matching the cells one by
/// one, so a wide cell before it changes nothing.
fn col_at(grid: &Grid, y: u16, needle: &str) -> u16 {
    let want: Vec<char> = needle.chars().collect();
    let (width, _) = grid.size();
    for x in 0..width {
        let mut ok = true;
        for (dx, want) in want.iter().enumerate() {
            let Some(xi) = x.checked_add(u16::try_from(dx).unwrap_or(u16::MAX)) else {
                ok = false;
                break;
            };
            if xi >= width || grid.cell(xi, y).symbol != want.to_string() {
                ok = false;
                break;
            }
        }
        if ok && !want.is_empty() {
            return x;
        }
    }
    panic!("{needle:?} on row {y}")
}

/// Clicks the cell at `(x, y)`: a press then its release on the same cell.
fn click(run: &mut Run, x: u16, y: u16) {
    run.write(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", x + 1, y + 1, x + 1, y + 1).as_bytes());
}

/// The rows holding a reversed cell: navigate mode's focus signature.
fn focused_rows(grid: &Grid) -> Vec<u16> {
    let (cols, rows) = grid.size();
    (0..rows)
        .filter(|&y| (0..cols).any(|x| grid.cell(x, y).inverse))
        .collect()
}

/// Whether `needle`'s row draws focused (reversed): the keyboard twin of
/// clicking the row.
fn row_focused(grid: &Grid, needle: &str) -> bool {
    let Some(y) = grid
        .rows
        .iter()
        .position(|row| row.contains(needle))
        .and_then(|y| u16::try_from(y).ok())
    else {
        return false;
    };
    let x = col_at(grid, y, needle);
    let len = u16::try_from(needle.chars().count()).unwrap_or(u16::MAX);
    (0..len).all(|dx| grid.cell(x + dx, y).inverse)
}

/// Tabs through navigate mode until `needle`'s row draws focused
/// (reversed), so Enter opens it: the keyboard twin of clicking the row.
/// Shift+Tab focuses the newest conversation item, Tab steps into the
/// panel's first stop, and Down walks the panel's stops top to bottom.
fn focus_row(run: &mut Run, needle: &str) {
    run.write(b"\t");
    run.wait_screen("the panel takes focus", |grid| {
        !focused_rows(grid).is_empty()
    });
    for _ in 0..60 {
        let grid = run.screen();
        if row_focused(&grid, needle) {
            return;
        }
        let before = focused_rows(&grid);
        // Down steps to the next stop, holding at the panel's end.
        run.write(b"\x1b[B");
        run.wait_screen("focus steps down", |grid| {
            row_focused(grid, needle) || focused_rows(grid) != before
        });
    }
    panic!("{needle:?} never took focus in\n{}", run.screen().contents);
}

/// Quits the terminal: two interrupts and Enter, then the exit.
fn quit(run: Run) {
    let mut run = run;
    run.write(b"\x03\x03\r");
    let finished = run.wait();
    assert_eq!(finished.status.code(), Some(0));
}

#[test]
fn a_finished_delegate_opens_its_replayed_view_from_a_click_and_from_enter() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains("Started delegate") {
            return support::hello();
        }
        support::stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    let child = ProviderServer::start_responding(|_: &Request| text_reply(CHILD_TEXT)).unwrap();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    // The hub exits a second after its last client leaves, so the run's
    // wait sees it go; the session itself keeps its 30-minute default.
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "hub": {"idle_exit_ms": 1000}}),
    );

    let env: Vec<(&str, &str)> = [("FIBER_TEST_FAKE_KEY", "sk-test")]
        .into_iter()
        .chain(support::TRUECOLOUR)
        .collect();
    let mut run = Run::spawn(&setup, 160, 48, &[], &env);
    run.ready();
    run.write(b"scan the tree\r");
    // The delegate runs to completion: the card keeps its finished row.
    let grid = run.wait_screen("the finished delegate row", |grid| {
        grid.contents.contains(FINISHED_ROW)
    });
    let card = row_with(&grid, FINISHED_ROW);
    // The click path: the finished Delegates-card row opens the replayed
    // transcript, not the parent's conversation.
    click(&mut run, col_at(&grid, card, FINISHED_ROW) + 2, card);
    run.wait_screen("the breadcrumb", |grid| grid.contents.contains("› ◆ scan"));
    run.wait_screen("the replayed transcript", |grid| {
        grid.contents.contains(CHILD_TEXT)
    });
    // Esc returns to the conversation with the finished row still there.
    run.write(b"\x1b[27u");
    run.wait_screen("the conversation back", |grid| {
        grid.contents.contains(FINISHED_ROW) && !grid.contents.contains(CHILD_TEXT)
    });
    // The keyboard path: Shift+Tab enters navigate mode, Tab steps to the
    // finished row, Enter opens it through the same serial seam.
    run.write(b"\x1b[Z");
    focus_row(&mut run, FINISHED_ROW);
    run.write(b"\r");
    run.wait_screen("the replayed transcript again", |grid| {
        grid.contents.contains(CHILD_TEXT)
    });
    // Esc returns to the conversation before quitting: the first Esc
    // clears the navigate focus the Enter path leaves, the second
    // closes the view.
    run.write(b"\x1b[27u\x1b[27u");
    run.wait_screen("the conversation back again", |grid| {
        grid.contents.contains(FINISHED_ROW) && !grid.contents.contains(CHILD_TEXT)
    });
    quit(run);
}
/// Answers a session tree on one provider server: `spawn` answers
/// the root's first request, `reviewer` every reviewer request,
/// `delegate_first` the depth-1 delegate's first request (named by the
/// spawn prompt that started it), `forked` the depth-2 fork's first
/// request, and `rest` every request carrying a receipt and every later
/// fork request. A refused `delegate_fork` completes as failed with no
/// receipt text, so only the fork's first request takes `forked`.
fn tree_answering(
    spawn: Response,
    reviewer: Response,
    delegate_first: Response,
    forked: Response,
    rest: Response,
) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    let forked_taken = Arc::new(AtomicBool::new(false));
    move |request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        // The reviewer sends an empty tools array, like the depth test's
        // router: only a session request carries tools.
        if !text.contains("\"tools\":[") || text.contains("\"tools\":[]") {
            return reviewer.clone();
        }
        if text.contains("Started delegate") {
            return rest.clone();
        }
        if text.contains("you are a fork of session") {
            if !forked_taken.swap(true, Ordering::SeqCst) {
                return forked.clone();
            }
            return rest.clone();
        }
        if text.contains("run the fork") {
            return delegate_first.clone();
        }
        spawn.clone()
    }
}

/// Installs the parent's model `pa/m` with a thinking default, so the
/// fork's first request must carry the same thinking level as the
/// parent's. The fork runs on the parent's build, on this same server.
fn fork_providers(setup: &support::Setup, server: &ProviderServer) {
    install_url(
        setup,
        "pa",
        &server.url(),
        "openai-responses",
        &json!({"thinking_levels": ["low", "high"], "thinking_default": "low"}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"}}),
    );
}

/// The `seq` just before the calling assistant message: the latest
/// `assistant_message_started` before the log's one `delegate_fork`
/// `tool_call_requested` line, minus one.
fn fork_point(lines: &[Value]) -> u64 {
    let requested = lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "delegate_fork"
        })
        .expect("the parent called delegate_fork");
    let call_seq = requested["seq"].as_u64().unwrap();
    lines
        .iter()
        .filter(|line| {
            line["kind"] == "assistant_message_started"
                && line["seq"].as_u64().is_some_and(|seq| seq < call_seq)
        })
        .map(|line| line["seq"].as_u64().unwrap())
        .max()
        .expect("a message before the call")
        - 1
}

/// The fork's first request on `server`: the first request carrying the
/// fork-task text, which only the fork's requests hold.
fn fork_first(server: &ProviderServer) -> Value {
    let requests = server.requests();
    requests
        .iter()
        .map(|request| serde_json::from_slice::<Value>(&request.body).unwrap())
        .find(|body| body.to_string().contains("you are a fork of session"))
        .expect("the fork made its first request")
}

/// The request whose reply carries the `delegate_fork` call for `prompt`:
/// the grandparent's own first request carries the tool too, so the
/// calling prompt names the calling session's request. The receipt and
/// the fork-task exclude the requests after the call.
fn fork_call_request(server: &ProviderServer, prompt: &str) -> Value {
    server
        .requests()
        .iter()
        .map(|request| serde_json::from_slice::<Value>(&request.body).unwrap())
        .find(|body| {
            // The fork keeps the tool declared, so its requests carry
            // it too; the reviewer sends an empty tools array.
            let declares_fork = body["tools"]
                .as_array()
                .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "delegate_fork"));
            declares_fork
                && body.to_string().contains(prompt)
                && !body.to_string().contains("Started delegate")
                && !body.to_string().contains("you are a fork of session")
        })
        .expect("the parent asked for the fork")
}

/// The fork continues its parent's request: its first request matches the
/// parent's last request up to the new content, keyed on the chain's root
/// with the parent's thinking level. It hands the two inputs back for the
/// fork-task checks below.
fn assert_continues<'a>(
    parent_body: &'a Value,
    fork_body: &'a Value,
    key: &str,
) -> (&'a Vec<Value>, &'a Vec<Value>) {
    let parent_input = parent_body["input"].as_array().unwrap();
    let fork_input = fork_body["input"].as_array().unwrap();
    assert!(
        fork_input.len() > parent_input.len(),
        "the fork's request adds its first-turn items and fork-task"
    );
    for (position, item) in parent_input.iter().enumerate() {
        assert_eq!(
            serde_json::to_string(&fork_input[position]).unwrap(),
            serde_json::to_string(item).unwrap(),
            "the inherited prefix is byte-identical at item {position}"
        );
    }
    let mut prefix_only = fork_body.clone();
    prefix_only["input"] = Value::Array(parent_input.clone());
    assert_eq!(prefix_only, *parent_body);
    assert_eq!(fork_body["prompt_cache_key"], json!(key));
    assert_eq!(fork_body["reasoning"], json!({"effort": "low"}));
    (parent_input, fork_input)
}

/// The fork's durable kinds, in order, pinned from a real run: one reply
/// completing, then the turn ends. The fork sends the logged preamble
/// verbatim, so its log holds no `preamble_built` and no
/// `opening_message`, unlike a spawned delegate's.
const FORK_DURABLE: &[&str] = &[
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "turn_started",
    "step_started",
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// A parent's `delegate_fork` call starts a fork of its calling turn: the
/// fork's `session_started` carries `forked_from` with the parent and the
/// point just before the calling message, and its log holds no event
/// copied from the parent's. The fork's first request matches the
/// parent's last request up to the new content, and its first user
/// message is the call's `fork-task`. The standing allow still asks the
/// reviewer.
#[test]
fn a_delegate_fork_starts_a_fork_of_the_calling_turn() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let server = ProviderServer::start_responding(tree_answering(
        support::stream(&[fork_call("say hello")]),
        text_reply("allow"),
        // No `run the fork` prompt at depth 0, so this never answers.
        support::hello(),
        support::hello(),
        support::hello(),
    ))
    .unwrap();
    fork_providers(&setup, &server);
    standing_allow(&setup);
    slow_idle(&setup);

    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "{stderr}");
    // The reviewer was asked, despite the standing allow: `delegate_fork`
    // declares `always_reviewed`, as `delegate_spawn` does.
    let resolved = one(&lines, "permission_resolved");
    assert_eq!(resolved["decision"], "allow");
    assert_eq!(resolved["decided_by"], "reviewer");
    let completed = completed_for(&lines, "delegate_fork");
    assert_eq!(completed["payload"]["status"], "completed");
    let parent_id = lines[0]["session_id"].as_str().unwrap();
    let parent_lines = read_lines(&ask_sessions(&setup).join(parent_id).join("events.jsonl"));
    let expected_seq = fork_point(&parent_lines);
    // The fork's `session_started` names its point: the parent and the
    // `seq` just before the calling message.
    let started = one(&lines, "delegate_started");
    let fork_id = started["delegate_session_id"].as_str().unwrap();
    let fork_lines = read_lines(&ask_sessions(&setup).join(fork_id).join("events.jsonl"));
    let fork_started = one(&fork_lines, "session_started");
    assert_eq!(
        fork_started["forked_from"],
        json!({"session_id": parent_id, "seq": expected_seq})
    );
    // Nothing copied: every fork line belongs to the fork, and the
    // parent's call and its review leave no line behind.
    assert!(
        fork_lines.iter().all(|line| line["session_id"] == fork_id),
        "every fork line belongs to the fork"
    );
    for kind in [
        "tool_call_requested",
        "tool_call_completed",
        "permission_requested",
        "permission_resolved",
        "job_started",
        "delegate_started",
    ] {
        assert!(
            fork_lines.iter().all(|line| line["kind"] != kind),
            "no {kind} in the fork's log"
        );
    }
    // The fork's log holds one completed turn and nothing else, in
    // order: a missing, duplicated or reordered event fails here.
    assert_eq!(durable_kinds(&fork_lines), FORK_DURABLE);
    // The fork's first request matches the parent's last request up to
    // the new content: preamble, model, thinking level and cache key.
    let parent_body = fork_call_request(&server, "scan the tree");
    let fork_body = fork_first(&server);
    let (parent_input, fork_input) = assert_continues(&parent_body, &fork_body, parent_id);
    // The new content ends with the call's `fork-task` as the first user
    // message, carrying the call's prompt.
    let new_content = &fork_input[parent_input.len()..];
    let task = new_content.last().unwrap();
    assert_eq!(task["role"], json!("user"));
    let text = task["content"].as_str().unwrap().to_owned();
    for part in [
        format!("you are a fork of session {parent_id}"),
        "started at delegate depth 1".to_owned(),
        "say hello".to_owned(),
    ] {
        assert!(
            text.contains(&part),
            "the fork-task carries {part:?}: {text}"
        );
    }
}

/// A depth-1 delegate's `delegate_fork` call starts a depth-2 fork whose
/// first request matches the delegate's last request up to the new
/// content, keyed on the delegate session. The depth-2 fork's own
/// `delegate_fork` call is refused, as a depth-2 spawn is.
#[test]
fn a_depth_one_delegates_fork_starts_depth_two_on_the_root_key() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let server = ProviderServer::start_responding(tree_answering(
        support::stream(&[function_call(
            "call_delegate",
            "delegate_spawn",
            &json!({"description": "scan", "prompt": "run the fork", "model": "fiber:pa/m"}),
        )]),
        text_reply("allow"),
        support::stream(&[fork_call("go deep")]),
        support::stream(&[fork_call("go deeper")]),
        support::hello(),
    ))
    .unwrap();
    fork_providers(&setup, &server);
    standing_allow(&setup);
    slow_idle(&setup);
    // The root session spawns the depth-1 delegate; the delegate forks
    // to depth 2. All three run on this one server.
    let running = Running::spawn(&setup, &["ask", "run the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "{stderr}");
    assert_eq!(
        completed_for(&lines, "delegate_spawn")["payload"]["status"],
        "completed"
    );
    let spawned = one(&lines, "delegate_started");
    let id = spawned["delegate_session_id"].as_str().unwrap();
    let parent_lines = read_lines(&ask_sessions(&setup).join(id).join("events.jsonl"));
    let expected_seq = fork_point(&parent_lines);
    assert_eq!(
        completed_for(&parent_lines, "delegate_fork")["payload"]["status"],
        "completed"
    );
    let started = one(&parent_lines, "delegate_started");
    assert_eq!(
        started["forked_from"],
        json!({"session_id": id, "seq": expected_seq})
    );
    let fork_id = started["delegate_session_id"].as_str().unwrap();
    // The fork's first request matches the delegate's last request up to
    // the new content. The fork continues the delegate's prefix, so its
    // cache key is the delegate's id, the fork chain's root
    // (`docs/prompt-cache.md`, "Cache markers and keys").
    let parent_body = fork_call_request(&server, "run the fork");
    let fork_body = fork_first(&server);
    let (_, fork_input) = assert_continues(&parent_body, &fork_body, id);
    // The `fork-task` names the delegate depth the fork runs at.
    let task = fork_input.last().unwrap();
    let text = task["content"].as_str().unwrap().to_owned();
    for part in [
        format!("you are a fork of session {id}"),
        "started at delegate depth 2".to_owned(),
        "go deep".to_owned(),
    ] {
        assert!(
            text.contains(&part),
            "the fork-task carries {part:?}: {text}"
        );
    }
    let fork_lines = read_lines(&ask_sessions(&setup).join(fork_id).join("events.jsonl"));
    assert!(
        fork_lines.iter().all(|line| line["session_id"] == fork_id),
        "every fork line belongs to the fork"
    );
    // At depth 2 the fork's own `delegate_fork` call is refused: no
    // grandchild starts.
    assert_depth_refusal(&fork_lines, false, true, "delegate_fork");
}

/// The delegate's one question to its driver: a free-text question with a
/// short header and unique text, so the wake line matches whole.
const ASK_COLOR_HEADER: &str = "Color";
const ASK_COLOR_TEXT: &str = "Which color for the porch light, teal or amber?";
/// The parent's relay question to the person, carrying the delegate's
/// question.
const RELAY_HEADER: &str = "Relay";
const RELAY_TEXT: &str = "The delegate asks: which color for the porch light, teal or amber?";
/// The person's answer as the result line the parent forwards verbatim.
const ANSWER_LINE: &str = "Relay: \"blue\"";
/// The answer line as a provider request body carries it: JSON escapes the
/// quotes.
const ANSWER_LINE_BODY: &str = "Relay: \\\"blue\\\"";
/// The delegate's final message once resumed.
const PORCH_DONE: &str = "porch-light-done-7f3a9c1e";

/// The delegate's first run, pinned from a real run: its one `ask_user`
/// call ends the turn `completed` with the questions, and the run exits.
const ASK_CHILD_DURABLE: &[&str] = &[
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
    "tool_call_started",
    "tool_call_completed",
    "turn_completed",
    "fiber_exited",
];

/// The parent's durable kinds, pinned from a real run: the spawn turn,
/// the wake turn carrying the delegate's questions with the person's form
/// and the resume, and the final wake. A hub-started session idles
/// after its turn instead of taking an ending-notice turn, so the
/// delegate's finish wakes a turn of its own. The log is read before the
/// test closes the parent, so it ends with the last wake turn.
const ASK_WAKE_PARENT_DURABLE: &[&str] = &[
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
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "interaction_requested",
    "interaction_resolved",
    "tool_call_completed",
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
    "assistant_message_started",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
];

#[test]
fn delegate_ask_user_reaches_parent_through_wake_and_answers_resume_it() {
    let setup = support::Setup::new();
    let guard = arm(&setup);
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if text.contains(PORCH_DONE) {
            return support::hello();
        }
        if text.contains("Resumed delegate") {
            return support::hello();
        }
        if text.contains(ANSWER_LINE_BODY) {
            let job = job_id(&text);
            return support::stream(&[function_call(
                "call_resume",
                "delegate_message",
                &json!({"id": job, "message": ANSWER_LINE}),
            )]);
        }
        if text.contains("It ended waiting for answers") {
            return support::stream(&[function_call(
                "call_relay",
                "ask_user",
                &json!({"questions": [{"header": RELAY_HEADER, "question": RELAY_TEXT}]}),
            )]);
        }
        if text.contains("Started delegate") {
            return support::hello();
        }
        support::stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    let child = ProviderServer::start([
        support::stream(&[function_call(
            "call_ask",
            "ask_user",
            &json!({"questions": [{"header": ASK_COLOR_HEADER, "question": ASK_COLOR_TEXT}]}),
        )]),
        text_reply(PORCH_DONE),
    ])
    .unwrap();
    child.hold();
    providers(&setup, &parent, &child);
    standing_allow(&setup);
    slow_idle(&setup);

    let hub = Arc::new(Mutex::new(None));
    let main = support::connect_hub(&setup, &hub).0;
    let workspace = setup.workspace().to_string_lossy().into_owned();
    let parent_id = support::start_session(&main, &workspace, "scan the tree");
    main.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{parent_id}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"full\"}}}}"
    ));
    let mut lines = support::until(&main, "the parent subscribe acknowledgement", |line| {
        is_answer(line, "c_sub")
    });
    assert_eq!(
        lines.last().unwrap()["kind"],
        "command_accepted",
        "{lines:?}"
    );
    let mut batch = support::until(&main, "the parent's delegate_started", |line| {
        line["kind"] == "delegate_started" && is_session(line, &parent_id)
    });
    let started = batch.last().unwrap();
    let delegate = started["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = started["payload"]["job_id"].as_str().unwrap().to_owned();
    lines.append(&mut batch);
    // The spawn turn ends while the delegate's first model call stays
    // held; the parent idles, so the delegate's finish wakes a turn of
    // its own.
    let mut ended = 0;
    let mut batch = support::until(&main, "the spawn turn's end", |line| {
        ended += usize::from(line["kind"] == "turn_completed" && is_session(line, &parent_id));
        ended == 1
    });
    lines.append(&mut batch);
    // A second client subscribes, so it can answer the person's form:
    // the hub rejects any other command before the first subscribe.
    let other = direct_client(&setup);
    other.send(&format!(
        "{{\"id\":\"c_sub_b\",\"session_id\":\"{parent_id}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"summary\"}}}}"
    ));
    let subbed = support::until(
        &other,
        "the second client's subscribe acknowledgement",
        |line| is_answer(line, "c_sub_b"),
    );
    assert_eq!(
        subbed.last().unwrap()["kind"],
        "command_accepted",
        "{subbed:?}"
    );
    child.release_one();
    // The delegate ends its first turn asking: the parent's wake carries
    // the questions, and the parent asks the person first.
    let mut batch = support::until(&main, "the parent's interaction_requested", |line| {
        line["kind"] == "interaction_requested" && is_session(line, &parent_id)
    });
    let requested = batch.last().unwrap().clone();
    lines.append(&mut batch);
    assert_eq!(requested["payload"]["kind"], "form");
    assert_eq!(
        requested["payload"]["fields"],
        json!([{"header": RELAY_HEADER, "question": RELAY_TEXT}])
    );
    let form = requested["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The delegate saw no second request across the form window: its run
    // ended asking, and only the resume starts another.
    assert_eq!(
        child.requests().len(),
        1,
        "the delegate was not asked again meanwhile"
    );
    // A second client answers the person's form.
    other.send(&format!(
        "{{\"id\":\"c_reply\",\"session_id\":\"{parent_id}\",\"command\":\"reply\",\"args\":{{\"request_id\":\"{form}\",\"answers\":[{{\"labels\":[],\"text\":\"blue\"}}]}}}}"
    ));
    let mut batch = support::until(&main, "the parent's interaction_resolved", |line| {
        line["kind"] == "interaction_resolved" && is_session(line, &parent_id)
    });
    let resolved = batch.last().unwrap().clone();
    lines.append(&mut batch);
    assert_eq!(resolved["payload"]["request_id"], form.as_str());
    assert_eq!(resolved["payload"]["by"], "person");
    assert_eq!(
        resolved["payload"]["answers"],
        json!([{"labels": [], "text": "blue"}])
    );
    let ack = support::until(&other, "the reply acknowledgement", |line| {
        is_answer(line, "c_reply")
    });
    assert_eq!(ack.last().unwrap()["kind"], "command_accepted", "{ack:?}");
    // The resume carries the verbatim answer lines as the next prompt.
    assert!(
        child.await_requests(2, setup.deadline),
        "the resumed run called its model"
    );
    let bodies: Vec<String> = child
        .requests()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(
        bodies[0].contains("say hello"),
        "the first call carries the opening prompt"
    );
    assert!(
        bodies[1].contains(ANSWER_LINE_BODY),
        "the resumed run carries the verbatim answer line"
    );
    child.release();
    // The resumed run ends on its final message, and the parent's last
    // wake turn ends on its own: three turns in all.
    let mut turns = 1;
    let mut batch = support::until(&main, "the parent's last turn ends", |line| {
        turns += usize::from(line["kind"] == "turn_completed" && is_session(line, &parent_id));
        turns == 3
    });
    lines.append(&mut batch);
    drop(other);

    // The wake request carries the delegate's question whole: header and
    // text on one line.
    let wake_line = format!("{ASK_COLOR_HEADER}: {ASK_COLOR_TEXT}");
    let carrying: Vec<String> = parent
        .requests()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .filter(|body| body.contains("It ended waiting for answers"))
        .collect();
    assert!(!carrying.is_empty(), "a wake request carries the questions");
    for body in &carrying {
        assert!(
            body.contains(&wake_line),
            "the wake carries the whole question line: {body}"
        );
    }
    // Six session requests and two reviewer verdicts: the spawn, the
    // receipt, the wake, the answer, the resume receipt, and the final
    // wake. The person's form is never reviewed.
    assert_eq!(parent.requests().len(), 8, "two reviews and six turns");

    let parent_log = sessions_in(&setup.home(), &setup.workspace())
        .join(&parent_id)
        .join("events.jsonl");
    let parent_lines = read_lines(&parent_log);
    // One job for life: two runs, each writing its start pair before its
    // finish pair, the second through `delegate_message`.
    let of_kind = |kind: &str| {
        parent_lines
            .iter()
            .filter(|line| line["kind"] == kind)
            .collect::<Vec<&Value>>()
    };
    for kind in [
        "job_started",
        "delegate_started",
        "delegate_finished",
        "job_completed",
    ] {
        let found = of_kind(kind);
        assert_eq!(
            found.len(),
            2,
            "{kind} wrote {:?}",
            owned_kinds(&parent_lines)
        );
        for line in &found {
            assert_eq!(line["payload"]["job_id"], job.as_str());
        }
    }
    let starts = of_kind("job_started");
    assert_eq!(starts[1]["payload"]["tool"], "delegate_message");
    // The first finish carries the questions; the asking run said no
    // text, so its final message is empty. The second carries the
    // resumed run's final message.
    let finished = of_kind("delegate_finished");
    assert_eq!(
        finished[0]["payload"]["questions"],
        json!([{"header": ASK_COLOR_HEADER, "question": ASK_COLOR_TEXT}])
    );
    assert_eq!(finished[0]["payload"]["text"], "");
    assert_eq!(finished[1]["payload"]["text"], PORCH_DONE);
    for done in of_kind("job_completed") {
        assert_eq!(done["payload"]["status"], "completed");
    }
    // The wake turn's input holds one jobs item naming the job, and the
    // parent's own form resolved by the person with one result line.
    let turns: Vec<&Value> = parent_lines
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .collect();
    assert_eq!(turns.len(), 3, "{:?}", owned_kinds(&parent_lines));
    assert_eq!(
        turns[1]["payload"]["input"],
        json!([{"type": "jobs", "job_ids": [job]}])
    );
    let asked = parent_lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested" && line["payload"]["name"] == "ask_user")
        .expect("the parent asked the person");
    let ask_action = asked["action_id"].as_str().unwrap();
    let ask_done = parent_lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed" && line["action_id"] == ask_action)
        .unwrap();
    assert_eq!(ask_done["payload"]["status"], "completed");
    assert_eq!(ask_done["payload"]["content"][0]["text"], ANSWER_LINE);
    assert!(
        parent_lines.iter().all(|line| !(
            line["kind"] == "permission_resolved" && line["action_id"] == ask_action
        )),
        "the person's form is never reviewed"
    );
    let messaged = parent_lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "delegate_message"
        })
        .expect("the parent resumed the delegate");
    let message_action = messaged["action_id"].as_str().unwrap();
    let decided = parent_lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved" && line["action_id"] == message_action)
        .expect("the reviewer decided the resume");
    assert_eq!(decided["payload"]["decided_by"], "reviewer");
    let spawned = parent_lines
        .iter()
        .find(|line| {
            line["kind"] == "tool_call_requested" && line["payload"]["name"] == "delegate_spawn"
        })
        .unwrap();
    let spawn_action = spawned["action_id"].as_str().unwrap();
    let spawn_decided = parent_lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved" && line["action_id"] == spawn_action)
        .unwrap();
    assert_eq!(spawn_decided["payload"]["decided_by"], "reviewer");
    // The person's form was raised in the parent session: exactly one
    // request and one resolution there.
    assert_eq!(of_kind("interaction_requested").len(), 1);
    assert_eq!(of_kind("interaction_resolved").len(), 1);

    // The delegate's log: the asking run, then the resumed run carrying
    // the answers in its second turn's input. No form was ever raised
    // there: the delegate is unanswerable end to end.
    let child_lines = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(&delegate)
            .join("events.jsonl"),
    );
    let mut expected: Vec<String> = ASK_CHILD_DURABLE
        .iter()
        .map(|kind| kind.to_string())
        .collect();
    expected.extend(RESUMED_CHILD_DURABLE.iter().map(|kind| kind.to_string()));
    assert_eq!(durable_kinds(&child_lines), expected);
    let asking = child_lines
        .iter()
        .filter(|line| line["kind"] == "turn_completed")
        .collect::<Vec<&Value>>();
    assert_eq!(asking.len(), 2);
    assert_eq!(asking[0]["payload"]["outcome"], "completed");
    assert_eq!(
        asking[0]["payload"]["questions"],
        json!([{"header": ASK_COLOR_HEADER, "question": ASK_COLOR_TEXT}])
    );
    let runs: Vec<&Value> = child_lines
        .iter()
        .filter(|line| line["kind"] == "turn_started")
        .collect();
    assert_eq!(runs.len(), 2);
    assert!(
        serde_json::to_string(&runs[1]["payload"]["input"])
            .unwrap()
            .contains(ANSWER_LINE_BODY),
        "{}",
        runs[1]["payload"]["input"]
    );
    assert!(
        child_lines
            .iter()
            .all(|line| line["kind"] != "interaction_requested"),
        "no form was raised in the delegate"
    );
    for exited in child_lines
        .iter()
        .filter(|line| line["kind"] == "fiber_exited")
    {
        assert_eq!(exited["payload"]["exit_code"], 0);
    }
    assert_eq!(
        durable_kinds(&without_copies(&parent_lines)),
        ASK_WAKE_PARENT_DURABLE
    );

    // Closing the parent ends it; its sessions leave no process behind.
    main.send(&format!(
        "{{\"id\":\"c_pclose\",\"session_id\":\"{parent_id}\",\"command\":\"close\",\"args\":{{\"now\":false}}}}"
    ));
    let mut batch = support::until(&main, "the parent's fiber_exited", |line| {
        line["kind"] == "fiber_exited" && is_session(line, &parent_id)
    });
    lines.append(&mut batch);
    drop(main);
    drop(lines);
    guard.wait_gone();
    stop_hub(&hub);
}

/// The delegate child's final message: the sentinel the parent's provider
/// requests must never carry.
const SENTINEL: &str = "sentinel-scan-done-4d2a9c1e";

/// A `go` command starting an extension delegate on `pb/m`: its `run`
/// writes an unconditional invocation marker before touching the finish,
/// then the final message to the workspace, then logs that it wrote.
/// The same start backs a `go` tool, since a model turn can only call
/// tools: the ask test's prompt reaches the delegate through it.
const DELEGATE_INIT: &str = r#"
local function start()
  local id = host.delegate(
    { description = "scan", prompt = "scan the tree", model = "fiber:pb/m" },
    { timeout = 60000, run = function(finish)
        host.fs.write("run-invoked.txt", "invoked")
        host.fs.write("run-out.txt", finish.text)
        host.log("wrote:" .. finish.text)
      end })
  return "started:" .. id
end
fiber.command("go", { timeout = 5000, run = function()
  host.status(start())
end })
fiber.tool("go", { description = "Start the scan delegate.", input_schema = { type = "object" },
  effects = { effects = { "executes" }, reversible = false },
  timeout = 5000, run = function() return start() end })
"#;

/// The parent's durable kinds for one command-started delegate, pinned
/// from a real run: the start pair, the delegate's own call copied with
/// its origin added, then the finish pair, with no turn anywhere, since
/// the parent never consulted the model itself (`docs/events.md`,
/// "Usage and notices": one `usage_recorded` per call however it
/// ended, and the parent copies each delegate line).
const DELEGATE_COMMAND_DURABLE: &[&str] = &[
    "session_started",
    "fiber_started",
    "extensions_loaded",
    "job_started",
    "delegate_started",
    "usage_recorded",
    "delegate_finished",
    "job_completed",
];

/// Sends `command` on a fresh full subscription, returning the client.
#[track_caller]
fn subscribed_command_session(
    setup: &support::Setup,
    id: &str,
) -> (extension_harness::Socket, extension_harness::Running) {
    let mut running = setup.start_session(id, &[]);
    let client = running.connect(&setup.socket(id));
    send(
        &client,
        r#"{"id":"c_sub","command":"subscribe","args":{"level":"full"}}"#,
    );
    until_session(&client, "the subscribe acknowledgement", |line| {
        line["kind"] == "command_accepted"
    });
    (client, running)
}

/// The socket's `extension_ui` statuses from `extension`.
fn ui_statuses(lines: &[Value], extension: &str) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line["kind"] == "extension_ui" && line["payload"]["extension"] == extension)
        .map(|line| line["payload"].to_string())
        .collect()
}

#[test]
fn an_extension_delegate_run_writes_the_final_message_where_the_model_never_reads() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        text_reply("parent-ok")
    })
    .unwrap();
    let child = ProviderServer::start_responding(|_: &Request| text_reply(SENTINEL)).unwrap();
    providers(&setup, &parent, &child);
    setup.lua("scan", DELEGATE_INIT);
    let id = doors::mint("s_");
    let (client, running) = subscribed_command_session(&setup, &id);
    send(
        &client,
        r#"{"id":"c_go","command":"command","args":{"name":"go"}}"#,
    );
    // The command's `host.delegate` call binds the job: the status names it.
    let started = until_session(&client, "the delegate start", |line| {
        line["kind"] == "extension_ui" && line["payload"].to_string().contains("started:")
    });
    assert!(
        ui_statuses(&started, "fiber.test/scan")
            .iter()
            .any(|status| status.contains("started:")),
        "the command reported its delegate id: {started:?}"
    );
    // The run wrote the bounded final message, then logged that it wrote:
    // the log line proves the file is whole before the test reads it. The
    // end may already sit among these lines, on either side of the write.
    let wrote = until_session(&client, "the run's write", |line| {
        line["kind"] == "extension_log" && line["payload"]["message"] == format!("wrote:{SENTINEL}")
    });
    assert_eq!(
        fs::read_to_string(setup.workspace().join("run-invoked.txt")).unwrap(),
        "invoked",
        "the run entered before reading the finish"
    );
    assert_eq!(
        fs::read_to_string(setup.workspace().join("run-out.txt")).unwrap(),
        SENTINEL,
        "the run file holds the bounded final message"
    );
    // The delegate's end: the log holds the whole order. Read before
    // close: a never-prompted session's directory is deleted on close,
    // so after the exit the file is gone.
    if !wrote.iter().any(|line| line["kind"] == "job_completed") {
        until_session(&client, "the delegate end", |line| {
            line["kind"] == "job_completed"
        });
    }
    let lines = read_lines(&support::session_dir(&setup, &id).join("events.jsonl"));
    assert_eq!(durable_kinds(&lines), DELEGATE_COMMAND_DURABLE);
    assert_eq!(one(&lines, "job_completed")["status"], "completed");
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    let _tail = until_session_close(&client);
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
    // No provider request carries the sentinel: the parent was never asked,
    // and the child's own requests hold its prompt, not its final message.
    assert!(
        parent.requests().is_empty(),
        "the model was never consulted: {:?}",
        parent
            .requests()
            .iter()
            .map(|request| String::from_utf8_lossy(&request.body).into_owned())
            .collect::<Vec<_>>()
    );
    assert!(
        !child.requests().is_empty(),
        "the search ran over the child's requests"
    );
    for request in child.requests() {
        let body = String::from_utf8_lossy(&request.body);
        assert!(
            !body.contains(SENTINEL),
            "no child request carries the final message: {body}"
        );
    }
}

#[test]
fn a_reload_that_disables_the_extension_ends_its_delegate_cancelled() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    let parent = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        text_reply("parent-ok")
    })
    .unwrap();
    // Held: the delegate stays running across the reload below.
    let child = ProviderServer::start([text_reply(SENTINEL)]).unwrap();
    child.hold();
    providers(&setup, &parent, &child);
    setup.lua("scan", DELEGATE_INIT);
    let id = doors::mint("s_");
    let (client, running) = subscribed_command_session(&setup, &id);
    send(
        &client,
        r#"{"id":"c_go","command":"command","args":{"name":"go"}}"#,
    );
    let mut stream = until_session(&client, "the delegate start", |line| {
        line["kind"] == "extension_ui" && line["payload"].to_string().contains("started:")
    });
    assert!(
        child.await_requests(1, setup.deadline),
        "the delegate child is running"
    );
    // A reload with the extension disabled: the reloaded set drops it, so
    // `retain` stops its running delegate.
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "extensions": {"fiber.test/scan": {"enabled": false}}}),
    );
    send(&client, r#"{"id":"c_reload","command":"reload"}"#);
    let reloaded = until_session(&client, "the reload", |line| line["kind"] == "reloaded");
    stream.extend(reloaded.clone());
    assert!(
        reloaded.last().unwrap()["payload"].get("failed").is_none(),
        "the reload dropped the extension cleanly: {reloaded:?}"
    );
    // The stop ends the delegate `cancelled`, and its `run` never ran:
    // no invocation marker, so no finish read either.
    stream.extend(until_session(&client, "the cancelled end", |line| {
        line["kind"] == "job_completed" && line["payload"]["status"] == "cancelled"
    }));
    assert!(
        !setup.workspace().join("run-invoked.txt").is_file(),
        "the stopped delegate ran no run"
    );
    assert!(
        !setup.workspace().join("run-out.txt").is_file(),
        "the stopped delegate wrote no finish"
    );
    assert_eq!(
        child.requests().len(),
        1,
        "the delegate ran exactly its held first request"
    );
    // The whole durable stream, in order, read before close: a
    // never-prompted session's directory is deleted on close. The
    // cancelled delegate's unfinished call still settles its
    // `usage_recorded`, copied with the delegate as origin
    // (`docs/events.md`, "Usage and notices").
    assert_eq!(
        durable_kinds(&read_lines(
            &support::session_dir(&setup, &id).join("events.jsonl")
        )),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "job_started",
            "delegate_started",
            "reloaded",
            "extensions_loaded",
            "preamble_built",
            "usage_recorded",
            "delegate_finished",
            "job_completed",
        ]
    );
    send(&client, r#"{"id":"c_close","command":"close"}"#);
    stream.extend(until_session_close(&client));
    // The whole socket stream, in order, from the command's send to the
    // session's exit: the replayed startup, the command's delegate start,
    // the reload, and the cancelled end. `clients`, `session_status` and
    // `attention` are independently triggered (`docs/testing.md`, "Event
    // streams"), so they are set aside; every other line is pinned.
    // `extension_ui` floats against `job_started`/`delegate_started`: the
    // command writes its status after `host.delegate` returns, while the
    // loop writes `job_started` with no ordering edge between them, and
    // `delegate_started` follows the child's spawn. `job_started` always
    // precedes `delegate_started`; `extension_ui` may land before, between,
    // or after them. Those three placements are valid; anything duplicated,
    // missing or otherwise reordered fails.
    let kinds = owned_kinds(&stream);
    let ordered: Vec<&str> = kinds
        .iter()
        .filter(|kind| *kind != "session_status" && *kind != "clients" && *kind != "attention")
        .map(String::as_str)
        .collect();
    let started_first: [&str; 16] = [
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "command_accepted",
        "job_started",
        "delegate_started",
        "extension_ui",
        "reloaded",
        "command_accepted",
        "extensions_loaded",
        "preamble_built",
        // The cancelled delegate's unfinished call settles here too
        // (`docs/events.md`, "Usage and notices").
        "usage_recorded",
        "delegate_finished",
        "job_completed",
        "command_accepted",
        "fiber_exited",
    ];
    let mut ui_first = started_first;
    ui_first.swap(5, 6);
    let mut ui_before_job = started_first;
    ui_before_job[4] = "extension_ui";
    ui_before_job[5] = "job_started";
    ui_before_job[6] = "delegate_started";
    assert!(
        ordered == started_first || ordered == ui_first || ordered == ui_before_job,
        "unexpected stream: {ordered:?}"
    );
    drop(client);
    let (status, _out, stderr) = running.wait();
    assert!(status.success(), "stderr: {stderr}");
}

#[test]
fn an_ask_that_ends_while_its_command_delegate_runs_cancels_it_before_the_exit() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    // The model's first turn calls the `go` tool; the second ends the
    // turn, so the prompt ends the session. Reviewer verdicts answer
    // "allow", as the answering helper does.
    // Held: the delegate is still running when the prompt ends the session.
    // Built before the parent and shared, so the parent's responder can
    // wait for the delegate's model call before ending the turn.
    let child = Arc::new(ProviderServer::start([text_reply(SENTINEL)]).unwrap());
    child.hold();
    let deadline = setup.deadline;
    let waiting = Arc::clone(&child);
    let called = Arc::new(AtomicBool::new(false));
    let parent = ProviderServer::start_responding(move |request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        if called.swap(true, Ordering::SeqCst) {
            assert!(
                waiting.await_requests(1, deadline),
                "the delegate called its model"
            );
            return text_reply("all done");
        }
        support::stream(&[function_call("call_go", "go", &json!({}))])
    })
    .unwrap();
    providers(&setup, &parent, &child);
    setup.lua("scan", DELEGATE_INIT);
    let running = Running::spawn(&setup, &["ask", "scan the tree"]);
    let (status, lines, stderr) = running.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");
    // The whole stream, in order, with the independently triggered
    // `session_status` lines set aside: they carry no position relative
    // to the session's other lines (`docs/testing.md`, "Event streams").
    let kinds = owned_kinds(&lines);
    let mut ordered: Vec<&str> = kinds
        .iter()
        .filter(|kind| *kind != "session_status")
        .map(String::as_str)
        .collect();
    // The cancelled delegate's call writes its usage at once
    // (`docs/events.md`, "Usage and notices"), but the doc gives that line no
    // place against `delegate_finished`: pin it once between the turn's end
    // and the job's, and set it aside from the ordered list.
    let turn_end = ordered
        .iter()
        .position(|kind| *kind == "turn_completed")
        .expect("no turn_completed");
    let job_end = ordered
        .iter()
        .position(|kind| *kind == "job_completed")
        .expect("no job_completed");
    let usage: Vec<usize> = (turn_end..job_end)
        .filter(|at| ordered[*at] == "usage_recorded")
        .collect();
    assert_eq!(
        usage.len(),
        1,
        "one usage_recorded after the turn: {ordered:?}"
    );
    ordered.remove(usage[0]);
    assert_eq!(
        ordered,
        [
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
            "tool_call_completed",
            "step_started",
            "job_started",
            "delegate_started",
            "assistant_message_started",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "delegate_finished",
            "job_completed",
            "fiber_exited",
        ]
    );
    // The normal end releases the running delegate: the ordered stream
    // already pins its `cancelled` end before the exit, and its `run`
    // never ran: no invocation marker, so no finish read either.
    let end = kinds
        .iter()
        .position(|kind| kind == "job_completed")
        .expect("no job_completed in {kinds:?}");
    let exit = kinds
        .iter()
        .position(|kind| kind == "fiber_exited")
        .expect("no fiber_exited in {kinds:?}");
    assert!(end < exit, "the cancelled end precedes the exit: {kinds:?}");
    assert_eq!(one(&lines, "job_completed")["status"], "cancelled");
    assert!(
        !setup.workspace().join("run-invoked.txt").is_file(),
        "the released delegate ran no run"
    );
    assert!(
        !setup.workspace().join("run-out.txt").is_file(),
        "the released delegate wrote no finish"
    );
    // The sentinel reached no provider request: the wake input carries
    // the job id, and the tool result carries only the start receipt.
    for request in parent.requests() {
        let body = String::from_utf8_lossy(&request.body);
        assert!(
            !body.contains(SENTINEL),
            "no parent request carries the final message: {body}"
        );
    }
}

#[test]
fn killing_the_parent_backfills_the_orphan_s_unseen_usage() {
    let setup = support::Setup::new();
    let _guard = arm(&setup);
    fs::write(setup.workspace().join("note.txt"), "the note\n").unwrap();
    // The parent spawns, then stalls on the receipt: the stall holds the
    // turn open, so no live copy can land while the child records below.
    let parent = ProviderServer::start_responding({
        let stage = std::sync::atomic::AtomicUsize::new(0);
        move |request: &Request| {
            let text = String::from_utf8_lossy(&request.body);
            if !text.contains(r#""tools":[{"#) {
                return cstream(&[ctext_id("p_allow", "allow"), cusage_id("p_allow", 0.0)]);
            }
            match stage.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => cstream(&[
                    ccall_id(
                        "p_spawn",
                        "delegate_spawn",
                        &json!({"description": "scan", "prompt": "say hello", "model": "fiber:pb/m"}),
                    ),
                    cusage_id("p_spawn", 0.0),
                ]),
                _ => stall(),
            }
        }
    })
    .unwrap();
    // The child reads three times, then stalls mid-turn: three generations
    // are recorded to its log, and the turn never finishes them.
    let child = ProviderServer::start_responding({
        let stage = std::sync::atomic::AtomicUsize::new(0);
        move |request: &Request| {
            let text = String::from_utf8_lossy(&request.body);
            if !text.contains(r#""tools":[{"#) {
                return cstream(&[ctext_id("c_allow", "allow"), cusage_id("c_allow", 0.0)]);
            }
            match stage.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => cstream(&[
                    ccall_id("c_1", "read", &json!({"path": "note.txt"})),
                    cusage_id("c_1", 0.02),
                ]),
                1 => cstream(&[
                    ccall_id("c_2", "read", &json!({"path": "note.txt"})),
                    cusage_id("c_2", 0.02),
                ]),
                2 => cstream(&[
                    ccall_id("c_3", "read", &json!({"path": "note.txt"})),
                    cusage_id("c_3", 0.02),
                ]),
                _ => stall(),
            }
        }
    })
    .unwrap();
    providers_url(&setup, &parent.url(), &child.url(), "openai-completions");
    standing_allow(&setup);
    slow_idle(&setup);

    // Held until each side parks: the parent stalls on the receipt before
    // the child records anything, whatever the scheduling.
    parent.hold();
    child.hold();
    let id = doors::mint("s_");
    let mut running = Running::start(
        &setup,
        &id,
        &["--model", "pa/m", "--prompt", "scan the tree"],
    );
    assert!(
        parent.await_requests(1, setup.deadline),
        "the spawn call was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(2, setup.deadline),
        "the verdict was requested"
    );
    parent.release_one();
    assert!(
        parent.await_requests(3, setup.deadline),
        "the turn stalled on the receipt"
    );
    parent.release();
    // The parent is blocked in its stall: the child now records three
    // generations no live copy can meet, then stalls its fourth call.
    child.release();
    assert!(
        child.await_requests(4, setup.deadline),
        "the child recorded three reads and stalled"
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
    // The kill-to-backfilled window runs under MUST_SUCCEED_WITHIN: the
    // child's hangup shutdown fits SHUTDOWN_BOUND, and the bound below
    // leaves shutdown plus observation slack on top of it.
    assert!(
        fakes::MUST_SUCCEED_WITHIN
            >= doors::SHUTDOWN_BOUND + Duration::from_secs(2) + Duration::from_secs(2),
        "the success bound clears the shutdown bound with slack to spare"
    );
    let bound = support::Deadline::after(fakes::MUST_SUCCEED_WITHIN);
    // Subscribed before the kill: the child's exit is awaited on this
    // socket, so a shutdown that beats a later subscription cannot hide
    // the exit line.
    let child_socket = support::Socket::connect(bound, &setup.session_socket(&child_id));
    subscribe(&child_socket);
    // The parent is gone however it died: SIGKILL to its group, which the
    // child left when it became its own leader. The remaining deadline
    // from here is the kill-to-backfilled window the resume asserts on.
    let killing = setup.deadline.left();
    support::kill_group(setup.deadline, running.group, "KILL").unwrap();
    let (status, _, _) = running.wait();
    assert_eq!(status.code(), None);
    // End of file on the lifeline is a hangup: the child shuts down with
    // 129 within the bound, releasing its session lock.
    let lines = support::until(&child_socket, "the child's exit", |line| {
        line["kind"] == "fiber_exited"
    });
    let exited = lines.last().unwrap();
    assert_eq!(exited["payload"]["exit_code"], 129);

    // The resume marks the orphan, then backfills every unseen record
    // after the marking: the copies arrive in delegate-log order, each
    // naming the child as the originator. The close below only answers
    // once the resume (marking plus backfill) is done serving, so the
    // log then holds everything the resume wrote.
    let mut resumed = Running::start(&setup, &id, &["--resume"]);
    resumed.wait_line("the orphan marking", |line| {
        line["kind"] == "job_completed" && line["payload"]["job_id"] == job_id
    });
    let backfilled = resumed.wait_line("the backfilled copies", |line| {
        line["kind"] == "usage_recorded"
            && line["payload"]["origin_session_id"] == child_id
            && line["payload"]["generation_id"] == "c_3"
    });
    // The kill-to-backfilled window fits MUST_SUCCEED_WITHIN: the
    // shutdown leg above ran under the success bound already, and this
    // pins the whole window against it.
    let consumed = killing
        .checked_sub(setup.deadline.left())
        .expect("the deadline only shrinks");
    assert!(
        consumed < fakes::MUST_SUCCEED_WITHIN,
        "kill to backfilled takes {consumed:?}, within {:?}",
        fakes::MUST_SUCCEED_WITHIN,
    );
    let copies: Vec<_> = backfilled
        .iter()
        .filter(|line| {
            line["kind"] == "usage_recorded" && line["payload"]["origin_session_id"] == child_id
        })
        .collect();
    assert_eq!(
        copies.len(),
        3,
        "the three reads arrive in order: {copies:?}"
    );
    assert_eq!(copies[0]["payload"]["generation_id"], "c_1");
    assert_eq!(copies[1]["payload"]["generation_id"], "c_2");
    assert_eq!(copies[2]["payload"]["generation_id"], "c_3");
    let client = resumed.connect(&setup);
    subscribe(&client);
    close_now(&client);
    let _tail = support::until_close(&client);
    drop(client);
    let (status, out, stderr) = resumed.wait();
    assert_eq!(status.code(), Some(0), "stderr: {stderr}");

    // The child's own log is the expectation: every `usage_recorded` it
    // holds, in order, is a missing source occurrence the parent never
    // saw live. The child ends with its hangup exit.
    let child_lines = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(&child_id)
            .join("events.jsonl"),
    );
    let exited = child_lines.last().unwrap().clone();
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["exit_code"], 129);
    let recorded: Vec<(String, Value)> = child_lines
        .iter()
        .filter(|line| line["kind"] == "usage_recorded")
        .map(|line| {
            (
                line["payload"]["generation_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                line["payload"]["cost"].clone(),
            )
        })
        .collect();
    assert!(
        recorded.len() >= 3,
        "the child recorded its three reads: {recorded:?}"
    );
    assert_eq!(
        &recorded[..3],
        &[
            ("c_1".to_owned(), json!(0.02)),
            ("c_2".to_owned(), json!(0.02)),
            ("c_3".to_owned(), json!(0.02)),
        ]
    );

    // The log pins the order the resume wrote: the orphan marking first,
    // then exactly the child's records, and no copy before the marking,
    // so none arrived live while the parent was stalled.
    let parent_lines = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(&id)
            .join("events.jsonl"),
    );
    let at = parent_lines
        .iter()
        .position(|line| line["kind"] == "job_completed" && line["payload"]["job_id"] == job_id)
        .expect("the orphan marking stands in the log");
    let ends: Vec<_> = parent_lines
        .iter()
        .filter(|line| line["kind"] == "job_completed" && line["payload"]["job_id"] == job_id)
        .collect();
    assert_eq!(ends.len(), 1, "one end for the job: {ends:?}");
    assert_eq!(ends[0]["payload"]["status"], "failed");
    assert_eq!(ends[0]["payload"]["error"]["code"], "orphaned");
    let logged: Vec<(String, Value)> = parent_lines
        .iter()
        .filter(|line| {
            line["kind"] == "usage_recorded" && line["payload"]["origin_session_id"] == child_id
        })
        .map(|line| {
            (
                line["payload"]["generation_id"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
                line["payload"]["cost"].clone(),
            )
        })
        .collect();
    assert_eq!(logged, recorded, "exactly the missing records");
    for line in parent_lines.iter().take(at) {
        assert!(
            !(line["kind"] == "usage_recorded" && line["payload"]["origin_session_id"] == child_id),
            "no live copy before the marking: {line:?}"
        );
    }
    // The resume reports exactly the appended lines: the marking, then
    // the copies in delegate-log order.
    let mut stream = out.into_iter().filter(|line| {
        (line["kind"] == "job_completed" && line["payload"]["job_id"] == job_id)
            || (line["kind"] == "usage_recorded"
                && line["payload"]["origin_session_id"] == child_id)
    });
    let first = stream.next().expect("the marking is reported");
    assert_eq!(first["kind"], "job_completed");
    let reported: Vec<String> = stream
        .map(|line| {
            line["payload"]["generation_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(
        reported,
        recorded
            .iter()
            .map(|(generation, _)| generation.clone())
            .collect::<Vec<_>>(),
        "the resume reports exactly the appended lines"
    );
}
