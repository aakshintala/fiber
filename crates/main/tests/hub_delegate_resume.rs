//! A client subscribed `summary` to a delegate through the hub follows it
//! into its resumed run (`docs/invocation.md`, "Driver commands",
//! `subscribe` row): the delegate finishes, the parent resumes it with
//! `delegate_message`, and the client receives the resumed run's statuses
//! without sending anything.
//!
//! No stand-in starts the later run: the parent's own `delegate_message`
//! call resumes the delegate, and the test tells the runs apart by the
//! `seq` the hub's stream itself carried, confirmed against the delegate's
//! log only after the parent exited.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use fakes::{ProviderServer, Request, Watchdog};
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
    /// Starts `fiber` with `args` and `stdin` in `dir`, draining stdout on
    /// a thread. The caller holds the returned stdin open.
    #[track_caller]
    fn spawn(
        setup: &support::Setup,
        args: &[&str],
        stdin: Stdio,
        dir: &std::path::Path,
    ) -> (Self, Option<std::process::ChildStdin>) {
        fakes::warm_up(Path::new(env!("CARGO_BIN_EXE_fiber")), &["--bogus"], 2);
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
            let left = self.deadline.left();
            if left.is_zero() {
                panic!(
                    "waited until the deadline for {what}; got {got:?}; stderr: {}",
                    self.stderr.try_recv().unwrap_or_default()
                )
            }
            match self.deadline.recv(&self.lines) {
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

/// Installs provider `name` with model `m` on `protocol` at `url`, with
/// `extra` merged into the model's fields. One site, handwork: moving both
/// copies into `support` is a follow-up once #631 releases the file.
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

/// The sessions directory for a session in `workspace`. One site,
/// handwork, as `install_url` above.
fn sessions_in(home: &Path, workspace: &Path) -> PathBuf {
    let workspace = fs::canonicalize(workspace).unwrap();
    let key = workspace.to_string_lossy().replace('/', "-");
    home.join("projects").join(key).join("sessions")
}

/// Reads `path` as one JSON value per line, skipping blank lines.
fn read_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// A `delegate_spawn` call for `model`: the parent's first reply.
fn spawn_call(model: &str) -> Value {
    support::function_call(
        "call_delegate",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello", "model": model}),
    )
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

/// The resume message carried to the delegate's second run: unique to it,
/// so the second model call carrying it proves the parent resumed it.
const RESUME: &str = "hub-resume-marker-4c1e8a02";

#[test]
fn a_summary_subscription_through_the_hub_follows_a_delegate_into_its_resumed_run() {
    let setup = support::Setup::new();
    let _guard = support::SessionGuard::arm(setup.deadline, &setup.root.path().to_string_lossy());
    let server_a = ProviderServer::start_responding(|request: &Request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"delegate_spawn""#) {
            return support::text_reply("allow");
        }
        if text.contains("run-two-done") {
            return support::hello();
        }
        if text.contains("Resumed delegate") {
            return support::hello();
        }
        if text.contains("run-one-done") {
            let job = job_id(&text);
            return support::stream(&[support::function_call(
                "call_resume",
                "delegate_message",
                &json!({"id": job, "message": RESUME}),
            )]);
        }
        if text.contains("Started delegate") {
            return support::hello();
        }
        support::stream(&[spawn_call("fiber:pb/m")])
    })
    .unwrap();
    let server_b = ProviderServer::start([
        support::text_reply("run-one-done"),
        support::text_reply("run-two-done"),
    ])
    .unwrap();
    server_b.hold();
    install_url(
        &setup,
        "pa",
        &server_a.url(),
        "openai-responses",
        &json!({}),
    );
    install_url(
        &setup,
        "pb",
        &server_b.url(),
        "openai-responses",
        &json!({}),
    );
    support::write_json(
        &setup.home().join("config.json"),
        &json!({"model": "pa/m", "reviewer": {"model": "pa/m"},
            "session": {"idle_exit_ms": 3600000}}),
    );
    let hub_slot = Arc::new(Mutex::new(None));
    let (hub, _) = support::connect_hub(&setup, &hub_slot);

    // The parent, whose wake turn resumes its delegate with
    // `delegate_message` once run 1 ends.
    let workdir = setup.workspace();
    let (mut running, _stdin) = Running::spawn(
        &setup,
        &["ask", "scan the tree"],
        Stdio::null(),
        workdir.as_path(),
    );
    let mut taken = running.wait_line("the parent's delegate_started", |line| {
        line["kind"] == "delegate_started"
    });
    let parent = taken[0]["session_id"].as_str().unwrap().to_owned();
    let started = taken.last().unwrap();
    let delegate = started["payload"]["delegate_session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let job = started["payload"]["job_id"].as_str().unwrap().to_owned();
    // Run 1 called its model, so its socket is bound.
    assert!(
        server_b.await_requests(1, setup.deadline),
        "run 1 called its model"
    );
    // The summary subscription replays the latest status and the run's
    // `extensions_loaded`; both name the delegate before the resume.
    hub.send(&format!(
        "{{\"id\":\"c_sub\",\"session_id\":\"{delegate}\",\"command\":\"subscribe\",\"args\":{{\"level\":\"summary\"}}}}"
    ));
    let ack = support::recv_reply(&hub, "the subscribe acknowledgement");
    assert_eq!(ack["kind"], "command_accepted");
    assert_eq!(ack["payload"]["command_id"], "c_sub");
    let mut status = None;
    let mut e1 = None;
    while status.is_none() || e1.is_none() {
        let line = support::recv_reply(&hub, "the delegate's summary lines");
        let for_delegate =
            line.get("session_id").and_then(Value::as_str) == Some(delegate.as_str());
        if line["kind"] == "session_status"
            && for_delegate
            && line["payload"]["parent"] == json!(parent)
        {
            status = Some(line.clone());
        }
        if line["kind"] == "extensions_loaded" && for_delegate && e1.is_none() {
            e1 = line["seq"].as_u64();
        }
    }
    assert!(status.is_some(), "the hub knew the delegate");
    let e1 = e1.expect("the summary delivered the run's extensions_loaded");
    // Run 1 ends only after the parent's own turns ended, so its finish
    // wakes a turn of its own, whose `delegate_message` call resumes it.
    let mut ended = 0;
    taken.extend(
        running.wait_line("the spawn and ending notice's turn ends", |line| {
            ended += usize::from(line["kind"] == "turn_completed");
            ended == 2
        }),
    );
    server_b.release_one();
    // The resumed run's start writes a second `extensions_loaded` with a
    // higher `seq`: the marker, read from the stream itself, while run 2's
    // reply stays held.
    let marker = support::until(&hub, "the resumed run's extensions_loaded", |line| {
        line["kind"] == "extensions_loaded"
            && line.get("session_id").and_then(Value::as_str) == Some(delegate.as_str())
            && line["seq"].as_u64().is_some_and(|seq| seq > e1)
    });
    let m = marker.last().unwrap()["seq"].as_u64().unwrap();
    assert!(
        server_b.await_requests(2, setup.deadline),
        "the resumed run called its model"
    );
    let bodies: Vec<String> = server_b
        .requests()
        .iter()
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect();
    assert_eq!(bodies.len(), 2, "{bodies:?}");
    assert!(
        bodies[1].contains(RESUME),
        "the resumed run carried the parent's message"
    );
    server_b.release();
    // After the marker, the resumed run's status arrives on the same
    // connection, still without the test sending anything.
    let after = support::until(&hub, "the resumed run's status", |line| {
        line["kind"] == "session_status"
            && line.get("session_id").and_then(Value::as_str) == Some(delegate.as_str())
            && line["payload"]["parent"] == json!(parent)
    });
    assert_eq!(after.last().unwrap()["session_id"], delegate);
    running.wait_exit();
    // Post-hoc, from the delegate's log: two runs, the marker exactly the
    // second run's boundary, between the two exits. The parent's log holds
    // two starts of the one job, the second through `delegate_message`.
    let parent_lines = read_lines(
        &sessions_in(&setup.home(), &setup.workspace())
            .join(&parent)
            .join("events.jsonl"),
    );
    let starts: Vec<&Value> = parent_lines
        .iter()
        .filter(|line| line["kind"] == "job_started")
        .collect();
    assert_eq!(starts.len(), 2, "two runs of the one job");
    for line in &starts {
        assert_eq!(line["payload"]["job_id"], job.as_str());
    }
    assert_eq!(starts[1]["payload"]["tool"], "delegate_message");
    let child_lines = read_lines(Path::new(
        starts[0]["payload"]["output_path"].as_str().unwrap(),
    ));
    let exits: Vec<u64> = child_lines
        .iter()
        .filter(|line| line["kind"] == "fiber_exited")
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(exits.len(), 2);
    assert!(exits[0] < exits[1]);
    let exts: Vec<u64> = child_lines
        .iter()
        .filter(|line| line["kind"] == "extensions_loaded")
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(exts.len(), 2);
    assert_eq!(exts[1], m);
    assert!(exits[0] < m && m < exits[1]);
    if let Some(proc) = hub_slot.lock().unwrap().take() {
        proc.kill_and_wait();
    }
}
