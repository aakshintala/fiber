//! Binary-level tests of the hub resuming an exited session
//! (`docs/invocation.md`, "Lifecycle" and "What the hub speaks"): the
//! built `fiber` runs `hub serve` in its own process group with its own
//! `FIBER_HOME`, holding an ordinary provider whose base URL is the fake
//! server. A command for a session whose socket accepts no connection
//! resumes it, then is delivered.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use contract::events::{Event, Parent, SessionStarted, Variables, VariablesSource};
use contract::{JobId, SessionId};
use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, group_alive};

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fr");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { deadline, root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    fn workspace_text(&self) -> String {
        self.workspace().to_string_lossy().into_owned()
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
    fn provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write_json(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
            }),
        );
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.0.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
        self.idle(None);
    }

    /// The configured model, with `session.idle_exit_ms` when given.
    fn idle(&self, ms: Option<u64>) {
        let config = match ms {
            Some(ms) => json!({"model": "fake/m", "session": {"idle_exit_ms": ms}}),
            None => json!({"model": "fake/m"}),
        };
        write_json(&self.home().join("config.json"), &config);
    }

    /// A standing ask for `shell` with `echo hi`: with a client the loop
    /// asks a person.
    fn standing_ask(&self) {
        fs::write(
            self.home().join("rules"),
            format!(
                "{}\n",
                json!({"decision": "ask", "tool": "shell", "prefix": "echo hi"})
            ),
        )
        .unwrap();
    }

    /// One `fiber` invocation with `args` in the workspace: the environment
    /// every test runs under.
    fn fiber(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        command
    }

    /// Runs `fiber` once with `args` to its exit under [`DEADLINE`]:
    /// its exit code and stdout lines.
    fn run(&self, args: &[&str]) -> (Option<i32>, Vec<Value>) {
        let child = self.fiber(args).spawn().unwrap();
        let group = child.id();
        let watchdog = Watchdog::group(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                support::kill_group(self.deadline, group, "KILL").unwrap();
                panic!(
                    "waited until the deadline for `fiber {}` to exit",
                    args.join(" ")
                );
            }
        };
        assert!(
            !group_alive(self.deadline, group),
            "`fiber {}` left a process in its group behind",
            args.join(" ")
        );
        watchdog.stand_down(self.deadline.cleanup());
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        (output.status.code(), lines)
    }

    /// The log of session `id`, wherever its project is.
    fn log(&self, id: &str) -> Vec<Value> {
        let projects = self.home().join("projects");
        let log = fs::read_dir(projects)
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .path()
                    .join("sessions")
                    .join(id)
                    .join("events.jsonl")
            })
            .find(|log| log.is_file())
            .expect("the session's log exists");
        fs::read_to_string(log)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn hub_log(&self) -> String {
        fs::read_to_string(self.home().join("logs").join("hub.log")).unwrap_or_default()
    }

    /// The `sessions` directory of the workspace's project.
    fn sessions(&self) -> PathBuf {
        let workspace = fs::canonicalize(self.workspace()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        self.home().join("projects").join(key).join("sessions")
    }
}

fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Whether any process remains in process group `group`.

/// The process clock behind `contract::clock::Clock`.
struct SystemClock;

impl contract::clock::Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::now"
    )]
    fn now(&self) -> std::time::Instant {
        std::time::Instant::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::wall"
    )]
    fn wall(&self) -> std::time::SystemTime {
        std::time::SystemTime::now()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the process clock behind contract::clock::Clock::sleep"
    )]
    fn sleep(&self, d: Duration) {
        thread::sleep(d);
    }

    fn wait_until(
        &self,
        until: Option<std::time::Instant>,
        wait: &mut dyn FnMut(Option<Duration>),
    ) {
        let bound = until.map(|until| until.saturating_duration_since(self.now()));
        wait(bound);
    }

    fn subscribe(&self, _waker: std::sync::Weak<dyn contract::clock::Wake>) {}
}

/// A hub the test started, and every session process it starts: each
/// carries the workspace path on its command line. Dropping it kills the
/// hub's group and every process holding the path; the watchdogs do the
/// same if the test process dies.
struct Hub {
    child: Option<Child>,
    group: u32,
    watchdog: Option<Watchdog>,
    sessions: Option<Watchdog>,
    workspace: String,
}

impl Hub {
    fn spawn(setup: &Setup) -> Self {
        let workspace = setup.workspace_text();
        let sessions = Watchdog::matching(&workspace);
        let mut child = setup.fiber(&["hub", "serve"]).spawn().unwrap();
        let group = child.id();
        let _ = child.stdout.take();
        let _ = child.stderr.take();
        Self {
            child: Some(child),
            group,
            watchdog: Some(Watchdog::group(group)),
            sessions: Some(sessions),
            workspace,
        }
    }

    /// Kills the hub, then waits for every session holding the workspace
    /// path to exit on its own.
    fn finish(mut self) {
        support::kill_group(self.deadline, self.group, "KILL").unwrap();
        if let Some(mut child) = self.child.take() {
            let (done, finished) = mpsc::channel();
            thread::spawn(move || done.send(child.wait().is_ok()).unwrap_or(()));
            assert!(
                finished.recv_timeout(self.deadline.left()).is_ok(),
                "the hub exited"
            );
        }
        assert!(
            fakes::matching_exits(&self.workspace, self.deadline.left()),
            "waited until the deadline for every session to exit"
        );
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.stand_down(self.deadline.cleanup());
        }
        if let Some(watchdog) = self.sessions.take() {
            watchdog.stand_down(self.deadline.cleanup());
        }
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        match support::kill_group_detached(self.group, "KILL") {
            Ok(_) | Err(_) => {}
        }
        match support::kill_matching_detached(&self.workspace) {
            Ok(()) | Err(_) => {}
        }
        if let Some(child) = self.child.as_mut() {
            match child.wait() {
                Ok(_) | Err(_) => {}
            }
        }
    }
}

/// A client on the hub's socket. Reads wait [`DEADLINE`] each: expiry
/// panics naming what was awaited.
struct Socket {
    write: Mutex<UnixStream>,
    read: Mutex<BufReader<UnixStream>>,
}

impl Socket {
    fn from(stream: UnixStream) -> Self {
        let read = stream.try_clone().unwrap();
        read.set_read_timeout(Some(DEADLINE)).unwrap();
        Self {
            write: Mutex::new(stream),
            read: Mutex::new(BufReader::new(read)),
        }
    }

    fn send(&self, line: &Value) {
        let mut write = self.write.lock().unwrap();
        let mut bytes = serde_json::to_vec(line).unwrap();
        bytes.push(b'\n');
        write.write_all(&bytes).unwrap();
        write.flush().unwrap();
    }

    fn next(&self, what: &str, got: &[Value]) -> Value {
        let mut buf = String::new();
        match self.read.lock().unwrap().read_line(&mut buf) {
            Ok(0) => panic!("the hub closed the socket while waiting for {what}; got {got:?}"),
            Ok(_) => serde_json::from_str(buf.trim_end()).unwrap(),
            Err(error)
                if error.kind() == ErrorKind::TimedOut || error.kind() == ErrorKind::WouldBlock =>
            {
                panic!("waited until the deadline for {what}; got {got:?}")
            }
            Err(error) => panic!("reading the hub socket while waiting for {what}: {error}"),
        }
    }
}

/// Collects lines until `done`, waiting [`DEADLINE`] for each.
fn until(client: &Socket, what: &str, mut done: impl FnMut(&Value) -> bool) -> Vec<Value> {
    let mut lines = Vec::new();
    loop {
        let line = client.next(what, &lines);
        let stop = done(&line);
        lines.push(line);
        if stop {
            return lines;
        }
    }
}

/// Connects to the hub, starting `fiber hub serve` when none runs.
fn connect_hub(setup: &Setup, hub: &Arc<Mutex<Option<Hub>>>) -> Socket {
    let slot = Arc::clone(hub);
    let mut start = move || {
        *slot.lock().unwrap() = Some(Hub::spawn(setup));
        Ok(())
    };
    let (stream, _hello) = doors::hub::connect(&setup.home(), &mut start, &SystemClock).unwrap();
    Socket::from(setup.deadline, stream)
}

fn command(id: &str, session: &str, command: &str, args: Value) -> Value {
    json!({"id": id, "session_id": session, "command": command, "args": args})
}

fn subscribe(id: &str, session: &str) -> Value {
    command(id, session, "subscribe", json!({"level": "full"}))
}

fn prompt(id: &str, session: &str, text: &str) -> Value {
    command(
        id,
        session,
        "prompt",
        json!({"content": [{"type": "text", "text": text}]}),
    )
}

fn close(session: &str) -> Value {
    json!({"id": "c_close", "session_id": session, "command": "close"})
}

fn allow(id: &str, session: &str, request: &str) -> Value {
    command(
        id,
        session,
        "reply",
        json!({"request_id": request, "decision": "allow"}),
    )
}

/// Whether `line` acknowledges command `id`.
fn acknowledges(line: &Value, id: &str) -> bool {
    line["kind"] == "command_accepted" && line["payload"]["command_id"] == id
}

/// An `openai-responses` stream of `events`, then a completed reply.
fn stream(events: &[Value]) -> Response {
    let mut body = String::new();
    for event in events {
        body.push_str(&format!(
            "event: {}\ndata: {event}\n\n",
            event["type"].as_str().unwrap()
        ));
    }
    let done = json!({"type": "response.completed", "response": {
        "id": "resp_1", "status": "completed",
        "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
    }});
    body.push_str(&format!(
        "event: {}\ndata: {done}\n\n",
        done["type"].as_str().unwrap()
    ));
    Response::stream(body)
}

/// An `openai-responses` stream answering `Hello.`.
fn hello() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
    }})])
}

/// A stream asking for `shell` with `echo hi`.
fn echo_call() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "function_call",
        "id": "fc_call_1",
        "call_id": "call_1",
        "name": "shell",
        "arguments": json!({"command": "echo hi"}).to_string()
    }})])
}

/// What the log holds of one writer at a time: one `session_started`,
/// `fiber_started` lines of which only the first is not resumed, and
/// `seq` carrying on without a gap.
fn assert_one_continued_log(lines: &[Value], started: usize) {
    let count = |kind: &str| lines.iter().filter(|line| line["kind"] == kind).count();
    assert_eq!(count("session_started"), 1);
    let fibers: Vec<&Value> = lines
        .iter()
        .filter(|line| line["kind"] == "fiber_started")
        .collect();
    assert_eq!(fibers.len(), started, "{lines:?}");
    assert_eq!(fibers[0]["payload"]["resumed"], false);
    assert!(
        fibers[1..]
            .iter()
            .all(|line| line["payload"]["resumed"] == true)
    );
    let seqs: Vec<u64> = lines
        .iter()
        .map(|line| line["seq"].as_u64().unwrap())
        .collect();
    assert!(
        seqs.windows(2).all(|pair| pair[1] == pair[0] + 1),
        "{seqs:?}"
    );
}

/// Waits for the relayed `fiber_exited`, then for the session process to
/// be gone: until then it still holds its connections and its log, and
/// answers a command `closing`.
fn until_exited(client: &Socket, setup: &Setup) -> Vec<Value> {
    let lines = until(client, "fiber_exited", |line| {
        line["kind"] == "fiber_exited"
    });
    assert!(
        fakes::matching_exits(&setup.workspace_text(), setup.deadline.left()),
        "waited until the deadline for the session process to exit"
    );
    lines
}

#[test]
fn a_prompt_through_the_hub_resumes_an_exited_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    let (code, lines) = setup.run(&["ask", "one"]);
    assert_eq!(code, Some(0));
    let id = lines[0]["session_id"].as_str().unwrap().to_owned();

    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub);
    client.send(&subscribe("c_sub", &id));
    let replay = until(&client, "the subscribe acknowledgement", |line| {
        acknowledges(line, "c_sub")
    });
    assert_eq!(
        replay.len(),
        1,
        "the acknowledgement comes first: {replay:?}"
    );
    client.send(&prompt("c_prompt", &id, "two"));
    // The replay holds the first turn; the prompt's turn follows its
    // acknowledgement, after the resumed `fiber_started`.
    let mut resumed = false;
    let mut prompted = false;
    let turn = until(&client, "the resumed turn", |line| {
        resumed |= line["kind"] == "fiber_started" && line["payload"]["resumed"] == true;
        prompted |= acknowledges(line, "c_prompt");
        resumed && prompted && line["kind"] == "turn_completed"
    });
    assert!(
        turn.iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "{turn:?}"
    );

    // A second client finds the session running and attaches.
    let second = connect_hub(&setup, &hub);
    second.send(&subscribe("c_sub2", &id));
    until(&second, "the second subscribe", |line| {
        acknowledges(line, "c_sub2")
    });

    // A valid id with no log names no session.
    client.send(&subscribe("c_none", "s_0000000000000000"));
    let rejected = until(&client, "the rejection", |line| {
        line["kind"] == "command_rejected"
    });
    let rejected = rejected.last().unwrap();
    assert_eq!(rejected["payload"]["code"], "session_not_found");
    assert_eq!(rejected["payload"]["command_id"], "c_none");

    client.send(&close(&id));
    until_exited(&client, &setup);
    drop((client, second));
    assert_one_continued_log(&setup.log(&id), 2);
    let log = setup.hub_log();
    assert_eq!(
        log.matches("\"code\":\"session_resumed\"").count(),
        1,
        "{log}"
    );
    assert!(!log.contains(&setup.workspace_text()), "{log}");
    hub.lock().unwrap().take().expect("the hub ran").finish();
}

#[test]
fn a_reply_after_the_session_exited_resumes_it_on_the_same_connection() {
    let setup = Setup::new();
    let server = ProviderServer::start([echo_call(), hello()]).unwrap();
    server.hold();
    setup.provider(&server);
    setup.standing_ask();
    // Long enough to cover the start handshake, short enough to idle out
    // on the approval.
    setup.idle(Some(2000));
    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub);
    client.send(&json!({
        "id": "c_start",
        "command": "start",
        "args": {"workspace": setup.workspace_text(), "content": [{"type": "text", "text": "run it"}]},
    }));
    let started = client.next("the start acknowledgement", &[]);
    assert!(acknowledges(&started, "c_start"), "{started}");
    let id = started["payload"]["result"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    // Subscribed while the model call is held: the approval's idle wait
    // starts only after it.
    client.send(&subscribe("c_sub", &id));
    until(&client, "the subscribe acknowledgement", |line| {
        acknowledges(line, "c_sub")
    });
    server.release();
    let asked = until(&client, "permission_requested", |line| {
        line["kind"] == "permission_requested"
    });
    let request = asked.last().unwrap()["payload"]["request_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let exited = until_exited(&client, &setup);
    assert_eq!(
        exited.last().unwrap()["payload"]["suspended_on"],
        request.as_str()
    );
    setup.idle(None);

    client.send(&allow("c_reply", &id, &request));
    let resumed = until(&client, "the resumed turn", |line| {
        line["kind"] == "turn_completed"
    });
    // The hub sent the subscription again; its acknowledgement is not
    // passed on, so the reply's is the only one.
    let acks: Vec<&Value> = resumed
        .iter()
        .filter(|line| line["kind"] == "command_accepted" || line["kind"] == "command_rejected")
        .collect();
    assert_eq!(acks.len(), 1, "{acks:?}");
    assert!(acknowledges(acks[0], "c_reply"), "{acks:?}");
    let raised = resumed
        .iter()
        .rev()
        .find(|line| line["kind"] == "permission_requested")
        .expect("the resumed session raised the request again");
    assert_eq!(raised["payload"]["request_id"], request.as_str());
    let resolved = resumed
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .expect("the reply resolved the request");
    assert_eq!(resolved["payload"]["request_id"], request.as_str());
    assert_eq!(resolved["payload"]["decided_by"], "person");
    let completed = resumed
        .iter()
        .rev()
        .find(|line| line["kind"] == "tool_call_completed")
        .expect("the allowed call ran");
    assert_eq!(completed["payload"]["status"], "completed");

    client.send(&close(&id));
    until_exited(&client, &setup);
    drop(client);
    assert_one_continued_log(&setup.log(&id), 2);
    hub.lock().unwrap().take().expect("the hub ran").finish();
}

#[test]
fn a_fresh_connection_answers_a_request_raised_again_on_resume() {
    let setup = Setup::new();
    let server = ProviderServer::start([echo_call(), hello()]).unwrap();
    setup.provider(&server);
    setup.standing_ask();
    setup.idle(Some(0));
    let id = doors::mint("s_");
    let (code, lines) = setup.run(&[
        "session",
        "--id",
        &id,
        "--workspace",
        &setup.workspace_text(),
        "--prompt",
        "run it",
    ]);
    assert_eq!(code, Some(0));
    let request = lines.last().unwrap()["payload"]["suspended_on"]
        .as_str()
        .expect("the session exited suspended on the approval")
        .to_owned();
    setup.idle(None);

    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub);
    client.send(&subscribe("c_sub", &id));
    let mut resumed = false;
    until(&client, "the re-raised permission_requested", |line| {
        if line["kind"] == "fiber_started" && line["payload"]["resumed"] == true {
            resumed = true;
        }
        resumed
            && line["kind"] == "permission_requested"
            && line["payload"]["request_id"] == request.as_str()
    });
    client.send(&allow("c_reply", &id, &request));
    let decided = until(&client, "turn_completed", |line| {
        line["kind"] == "turn_completed"
    });
    assert!(decided.iter().any(|line| acknowledges(line, "c_reply")));
    let resolved = decided
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .expect("the reply resolved the request");
    assert_eq!(resolved["payload"]["decided_by"], "person");
    assert!(
        decided
            .iter()
            .any(|line| line["kind"] == "text_completed" && line["payload"]["text"] == "Hello."),
        "{decided:?}"
    );

    client.send(&close(&id));
    until_exited(&client, &setup);
    drop(client);
    assert_one_continued_log(&setup.log(&id), 2);
    hub.lock().unwrap().take().expect("the hub ran").finish();
}

#[test]
fn a_subscribe_through_the_hub_to_an_exited_delegate_is_refused() {
    let setup = Setup::new();
    let id = "s_00000000000000d1";
    let log = log::Log::create(
        &setup.sessions(),
        SessionId(id.into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    log.append(
        &Event::SessionStarted(SessionStarted {
            workspace: fs::canonicalize(setup.workspace())
                .unwrap()
                .display()
                .to_string(),
            variables: Variables {
                path: String::new(),
                names: Vec::new(),
                source: VariablesSource::Inherited,
            },
            parent: Some(Parent {
                session_id: SessionId("s_1111111111111111".into()),
                delegate_id: JobId("j_1".into()),
            }),
            forked_from: None,
            rewind: None,
        }),
        None,
        None,
    )
    .unwrap();
    drop(log);

    let hub = Arc::new(Mutex::new(None));
    let client = connect_hub(&setup, &hub);
    client.send(&subscribe("c_sub", id));
    let rejected = client.next("the rejection", &[]);
    assert_eq!(rejected["kind"], "command_rejected", "{rejected}");
    assert_eq!(rejected["payload"]["code"], "session_not_found");
    assert_eq!(rejected["payload"]["command_id"], "c_sub");
    assert_eq!(
        rejected["payload"]["message"],
        "A delegate resumes only through its parent."
    );
    // No session process started: it would have bound its socket and
    // appended `fiber_started`.
    assert!(!setup.hub_log().contains("session_resumed"));
    assert!(!setup.home().join("run").join(id).exists());
    assert_eq!(setup.log(id).len(), 1);
    drop(client);
    hub.lock().unwrap().take().expect("the hub ran").finish();
}
