//! Binary-level tests of the built-in tools `fiber ask` registers
//! (`docs/testing.md`, "Levels"): the built `fiber` runs in its own process
//! group with its own `FIBER_HOME`, holding an ordinary provider whose base
//! URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::Deadline;

/// The request's tool order: the loop keys tools by name, so this is name
/// order, whatever order `main` pushes them in.
const TOOL_NAMES: [&str; 8] = [
    "ask_user",
    "edit",
    "handoff",
    "jobs",
    "read",
    "shell",
    "web_fetch",
    "write",
];

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
}

impl Setup {
    fn new() -> Self {
        Self::within(Deadline::start())
    }

    fn within(deadline: Deadline) -> Self {
        let root = fakes::TempDir::new("fa");
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

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
    fn provider(&self, server: &ProviderServer) {
        self.provider_on_cost(server, "openai-responses", None);
    }

    /// [`Setup::provider`] with `cost` as model `m`'s declared prices.
    fn provider_priced(&self, server: &ProviderServer, cost: Value) {
        self.provider_on_cost(server, "openai-responses", Some(cost));
    }

    /// [`Setup::provider`] on `protocol`.
    fn provider_on(&self, server: &ProviderServer, protocol: &str) {
        self.provider_on_input(server, protocol, Some(vec!["text", "image"]));
    }

    /// [`Setup::provider_on`], with `input` as the model's declared input
    /// kinds, or none when it declares none.
    fn provider_on_input(&self, server: &ProviderServer, protocol: &str, input: Option<Vec<&str>>) {
        self.provider_on_cost_input(server, protocol, None, input);
    }

    /// [`Setup::provider_on_input`], with `cost` as the model's declared
    /// prices, or none when it declares none.
    fn provider_on_cost(&self, server: &ProviderServer, protocol: &str, cost: Option<Value>) {
        self.provider_on_cost_input(server, protocol, cost, Some(vec!["text", "image"]));
    }

    /// The shared body behind [`Setup::provider`] and
    /// [`Setup::provider_priced`]: `cost` is written only when present.
    fn provider_on_cost_input(
        &self,
        server: &ProviderServer,
        protocol: &str,
        cost: Option<Value>,
        input: Option<Vec<&str>>,
    ) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        let mut model = json!({"id": "m", "protocol": protocol,
            "base_url": format!("{}/v1", server.url()), "context_window": 100000});
        if let Some(input) = input {
            model["input"] = json!(input);
        }
        if let Some(cost) = cost {
            model["cost"] = cost;
        }
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [model]
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
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// Runs `fiber` in its own process group, waits for it under the test's
    /// [`Deadline`], and asserts that nothing it started is left in the
    /// group, after a timeout too (`docs/testing.md`, "Running tests").
    /// A watchdog beside it kills that group if this process dies first.
    fn run(&self, args: &[&str]) -> Run {
        let home = self.home();
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", home)
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(self.deadline.left()) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(
            fakes::group_empties(group, self.deadline.left()),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
        Run::from(output)
    }

    /// Runs `fiber` as [`Setup::run`] does, reading stdout as it is
    /// written, and calls `act` once the lines so far satisfy `when`: two
    /// waits under the test's [`Deadline`]: for the lines `when` needs, then
    /// for the exit.
    fn run_then(
        &self,
        args: &[&str],
        when: impl Fn(&[Value]) -> bool + Send + 'static,
        act: impl FnOnce(),
    ) -> Run {
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
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let (err_tx, err_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            match std::io::Read::read_to_string(&mut stderr, &mut text) {
                Ok(_) | Err(_) => {}
            }
            match err_tx.send(text) {
                Ok(()) | Err(_) => {}
            }
        });
        let text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let text_collector = std::sync::Arc::clone(&text);
        let (matched_tx, matched) = mpsc::channel();
        let (finished_tx, finished) = mpsc::channel();
        thread::spawn(move || {
            let mut lines = Vec::new();
            let mut sent = false;
            for line in std::io::BufRead::lines(std::io::BufReader::new(stdout)) {
                let line = line.unwrap();
                if is_status(&line) {
                    continue;
                }
                text_collector.lock().unwrap().push_str(&line);
                text_collector.lock().unwrap().push('\n');
                lines.push(serde_json::from_str(&line).unwrap_or(Value::Null));
                if !sent && when(&lines) {
                    sent = true;
                    if matched_tx.send(()).is_err() {
                        return;
                    }
                }
            }
            let status = child.wait().unwrap();
            let stderr = err_rx.recv().unwrap_or_default();
            let stdout = text_collector.lock().unwrap().clone();
            match finished_tx.send((status, stdout, lines, stderr)) {
                Ok(()) | Err(_) => {}
            }
        });
        match matched.recv_timeout(self.deadline.left()) {
            Ok(()) => act(),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "`fiber {}` exited before the lines run_then waits for",
                    args.join(" ")
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let so_far = text.lock().unwrap().clone();
                support::expired(
                    self.deadline,
                    group,
                    &finished,
                    &format!(
                        "`fiber {}` to write the lines run_then waits for; so far: {so_far}",
                        args.join(" ")
                    ),
                )
            }
        }
        let (status, stdout, lines, stderr) = match finished.recv_timeout(self.deadline.left()) {
            Ok(done) => done,
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit after the act", args.join(" ")),
            ),
        };
        assert!(
            fakes::group_empties(group, self.deadline.left()),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup());
        Run {
            code: status.code(),
            stdout,
            lines,
            stderr,
        }
    }
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Spawns `command` in a new process group, then a watchdog in its own
/// group. The watchdog's stdin is a pipe only this process holds: a newline
/// means the child is reaped, and EOF means this process died, so the
/// watchdog kills the group. The watchdog is started immediately after the
/// child; a kill in the gap between the two spawns can still orphan it.
fn spawn_watched(command: &mut Command) -> (Child, Watchdog) {
    let child = command.process_group(0).spawn().unwrap();
    let group = child.id();
    let guard = KillGroup(group);
    let watchdog = Watchdog::group(group);
    std::mem::forget(guard);
    (child, watchdog)
}

/// Kills process group `group` on drop. After the child is reaped and the
/// group is empty, [`std::mem::forget`] skips that kill.
struct KillGroup(u32);

impl Drop for KillGroup {
    fn drop(&mut self) {
        support::kill_group_detached(self.0, "KILL");
    }
}

/// Makes the FIFO `path` with `mkfifo`, run to its exit under the test's
/// [`Deadline`].
fn mkfifo(setup: &Setup, path: &Path) {
    let mut command = Command::new("mkfifo");
    command
        .arg(path)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let made = support::run_to_exit(setup.deadline, "mkfifo", command);
    assert!(made.status.success(), "mkfifo {}: {made:?}", path.display());
}

/// One finished run: its exit code, stdout's lines, as text and parsed, and
/// stderr.
struct Run {
    code: Option<i32>,
    stdout: String,
    lines: Vec<Value>,
    stderr: String,
}

/// A `session_status` line: ephemeral, and written by an observer thread, so
/// where it falls among the loop's own lines is not what these tests pin.
/// `tests/socket.rs` reads it.
fn is_status(line: &str) -> bool {
    line.contains(r#""kind":"session_status""#)
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            stdout,
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Run {
    fn kinds(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|line| line["kind"].as_str().unwrap())
            .collect()
    }

    fn session_id(&self) -> &str {
        self.lines[0]["session_id"].as_str().unwrap()
    }

    /// The session's directory, from its id.
    fn session_dir(&self, setup: &Setup) -> PathBuf {
        let workspace = fs::canonicalize(setup.workspace()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        setup
            .home()
            .join("projects")
            .join(key)
            .join("sessions")
            .join(self.session_id())
    }
}

/// `preamble_built` for `reason`, with the five built-in tools and nothing
/// replaced. A resume builds the same set again; it does not replace one.
fn assert_preamble(run: &Run, reason: &str) {
    let preamble = run
        .lines
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert_eq!(preamble["payload"]["reason"], reason);
    assert!(preamble["payload"].get("replaced").is_none());
    let sent: Vec<_> = preamble["payload"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| {
            (
                tool["name"].as_str().unwrap(),
                tool["registered_by"].as_str().unwrap(),
            )
        })
        .collect();
    let expected: Vec<_> = TOOL_NAMES.iter().map(|name| (*name, "builtin")).collect();
    assert_eq!(sent, expected);
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

/// An `openai-responses` stream answering `Hello.` in two fragments.
fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// An `openai-responses` stream answering `text`: what a scripted
/// reviewer verdict reads as. Reviewer replies are never streamed to
/// watchers, so no delta is needed, only the finished message.
fn text_reply(text: &str) -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": text}]
    }})])
}

fn tool_names(body: &[u8]) -> Vec<String> {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The event kinds of a turn whose first reply calls one tool and whose
/// second is [`hello`]. A reads-only workspace call is a fast path, so no
/// `permission_` line is written (`docs/permissions.md`, "Fast paths").
fn read_kinds() -> Vec<&'static str> {
    let mut kinds = vec![
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
        "step_started",
        "assistant_message_started",
    ];
    kinds.extend(["assistant_message_delta"; 2]);
    kinds.extend([
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

#[test]
fn a_new_session_offers_the_builtin_tools_and_a_read_completes() {
    let setup = Setup::new();
    let note = "alpha line\n";
    fs::write(setup.workspace().join("note.txt"), note).unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "read the note"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(tool_names(&requests[0].body), TOOL_NAMES);
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], note);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let output = second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(output["output"], note);
}

#[test]
fn a_resumed_session_offers_the_same_tools_in_the_same_order() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);

    let first = setup.run(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();

    let second = setup.run(&["ask", "--resume", &id, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let first_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let second_body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(first_body["tools"], second_body["tools"]);
    assert_eq!(tool_names(&requests[0].body), TOOL_NAMES);
    let turn = [
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ];
    // A resume writes no `session_started` and no `opening_message`: the log
    // already holds both.
    assert_eq!(
        first.kinds(),
        [
            &[
                "session_started",
                "fiber_started",
                "extensions_loaded",
                "preamble_built",
                "opening_message",
                "turn_started",
            ][..],
            &turn,
        ]
        .concat()
    );
    assert_eq!(
        second.kinds(),
        [
            &[
                "fiber_started",
                "extensions_loaded",
                "preamble_built",
                "turn_started"
            ][..],
            &turn
        ]
        .concat()
    );
    assert_preamble(&first, "start");
    assert_preamble(&second, "resume");
}

/// The credential file's text. It is not a path fragment and not a word the
/// denial sentence uses, so a hit in an output can only be the file's bytes.
const MARKER: &str = "xylophone-quorum-91";

/// The event kinds of a turn whose first reply makes `calls` denials and
/// whose second is [`hello`]. Every denial is decided before any completion.
fn denied_kinds(calls: usize) -> Vec<&'static str> {
    let mut kinds = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
    ];
    kinds.extend(std::iter::repeat_n("tool_call_requested", calls));
    kinds.extend(["usage_recorded", "assistant_message_completed"]);
    kinds.extend(std::iter::repeat_n("permission_resolved", calls));
    kinds.extend(std::iter::repeat_n("tool_call_completed", calls));
    kinds.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

fn holds_marker(bytes: &[u8], marker: &str) -> bool {
    let marker = marker.as_bytes();
    bytes.windows(marker.len()).any(|window| window == marker)
}

fn assert_no_marker(label: &str, bytes: &[u8]) {
    assert!(
        !holds_marker(bytes, MARKER),
        "{label} holds the credential file's text"
    );
}

/// Every file under `dir`, symlink targets included. `events.jsonl` is one
/// of them; `artifacts/` is entered even when a call wrote nothing there.
fn assert_session_has_no_marker(dir: &Path) {
    assert!(dir.join("events.jsonl").is_file());
    assert!(dir.join("artifacts").is_dir());
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        let meta = fs::symlink_metadata(&path).unwrap();
        if meta.file_type().is_dir() {
            for entry in fs::read_dir(&path).unwrap() {
                pending.push(entry.unwrap().path());
            }
            continue;
        }
        assert_no_marker(&path.display().to_string(), &fs::read(&path).unwrap());
    }
}

#[test]
fn a_credential_path_is_denied_for_a_read_and_a_shell_cat_under_every_spelling() {
    let setup = Setup::new();
    let secret = setup.home().join("credentials").join("secret.txt");
    fs::create_dir_all(secret.parent().unwrap()).unwrap();
    fs::write(&secret, format!("{MARKER}\n")).unwrap();
    let real = fs::canonicalize(&secret).unwrap();
    let absolute = real.to_str().unwrap().to_owned();
    std::os::unix::fs::symlink(&real, setup.workspace().join("via")).unwrap();
    // Through directories that exist: `..` over a missing directory would
    // end `tool_error` and prove nothing about the deny.
    let dotted = "../h/credentials/secret.txt";
    assert_eq!(
        fs::canonicalize(setup.workspace().join("via")).unwrap(),
        real
    );
    assert_eq!(
        fs::canonicalize(setup.workspace().join(dotted)).unwrap(),
        real
    );

    let spellings = [absolute.as_str(), "via", dotted];
    for spelling in spellings {
        assert!(!spelling.contains(MARKER), "{spelling}");
        assert!(!spelling.contains(char::is_whitespace), "{spelling}");
    }
    let mut events = Vec::new();
    for (index, path) in spellings.iter().enumerate() {
        events.push(function_call(
            &format!("read_{index}"),
            "read",
            &json!({"path": path}),
        ));
    }
    for (index, path) in spellings.iter().enumerate() {
        events.push(function_call(
            &format!("shell_{index}"),
            "shell",
            &json!({"command": format!("cat {path}")}),
        ));
    }
    let script = serde_json::to_string(&events).unwrap();
    assert!(
        !script.contains(MARKER),
        "the scripted arguments contain the credential file's text"
    );
    assert!(holds_marker(&fs::read(&secret).unwrap(), MARKER));

    let server = ProviderServer::start([stream(&events), hello()]).unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "show the secret"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), denied_kinds(spellings.len() * 2));
    let requested: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_requested")
        .collect();
    let resolved: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "permission_resolved")
        .collect();
    let completed: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(requested.len(), spellings.len() * 2);
    for (request, (decision, done)) in requested.iter().zip(resolved.iter().zip(&completed)) {
        let action = &request["action_id"];
        assert_eq!(&decision["action_id"], action);
        assert_eq!(&done["action_id"], action);
        assert_eq!(decision["payload"]["decided_by"], "credential_deny");
        assert_eq!(done["payload"]["status"], "denied");
        assert_eq!(done["payload"]["reason"], "credentials");
        assert!(
            !run.lines
                .iter()
                .any(|line| line["kind"] == "tool_call_started" && &line["action_id"] == action)
        );
    }
    assert_no_marker("stdout", run.stdout.as_bytes());
    assert_no_marker("stderr", run.stderr.as_bytes());
    for (index, request) in server.requests().iter().enumerate() {
        assert_no_marker(&format!("request {index}"), &request.body);
    }
    assert_session_has_no_marker(&run.session_dir(&setup));
}

#[test]
fn a_configured_file_credential_source_is_denied_for_a_read() {
    // A `file` source outside Fiber home and outside the workspace, named
    // by the global configuration: the model reads it, and the credential
    // deny refuses the read before it runs.
    let setup = Setup::new();
    let key = setup.root.path().join("keys").join("openrouter");
    fs::create_dir_all(key.parent().unwrap()).unwrap();
    fs::write(&key, format!("{MARKER}\n")).unwrap();
    let path = fs::canonicalize(&key).unwrap().to_str().unwrap().to_owned();
    assert!(!path.contains(MARKER), "{path}");
    assert!(!path.contains(char::is_whitespace), "{path}");

    let events = vec![function_call("read_0", "read", &json!({"path": path}))];
    let script = serde_json::to_string(&events).unwrap();
    assert!(
        !script.contains(MARKER),
        "the scripted arguments contain the credential file's text"
    );
    assert!(holds_marker(&fs::read(&key).unwrap(), MARKER));

    let server = ProviderServer::start([stream(&events), hello()]).unwrap();
    setup.provider(&server);
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m",
            "providers": {"fake": {"credentials": {"work": {"file": path}}}}}),
    );

    let run = setup.run(&["ask", "show the secret"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), denied_kinds(1));
    let requested: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_requested")
        .collect();
    let resolved: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "permission_resolved")
        .collect();
    let completed: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(requested.len(), 1);
    assert_eq!(resolved.len(), 1);
    assert_eq!(completed.len(), 1);
    assert_eq!(&resolved[0]["action_id"], &requested[0]["action_id"]);
    assert_eq!(&completed[0]["action_id"], &requested[0]["action_id"]);
    assert_eq!(resolved[0]["payload"]["decided_by"], "credential_deny");
    assert_eq!(
        resolved[0]["payload"]["reason"],
        "The call touches a configured credential file."
    );
    assert_eq!(completed[0]["payload"]["status"], "denied");
    assert_eq!(completed[0]["payload"]["reason"], "credentials");
    assert!(
        !run.lines
            .iter()
            .any(|line| line["kind"] == "tool_call_started"),
        "a denied call never starts"
    );
    assert_no_marker("stdout", run.stdout.as_bytes());
    assert_no_marker("stderr", run.stderr.as_bytes());
    for (index, request) in server.requests().iter().enumerate() {
        assert_no_marker(&format!("request {index}"), &request.body);
    }
    assert_session_has_no_marker(&run.session_dir(&setup));
}

#[test]
fn a_grep_that_follows_a_link_below_its_operand_into_the_credentials_is_reviewed() {
    let setup = Setup::new();
    let credentials = setup.home().join("credentials");
    fs::create_dir_all(&credentials).unwrap();
    fs::write(credentials.join("secret.txt"), format!("{MARKER}\n")).unwrap();
    // The declared path is `ws`, which neither is nor contains
    // `credentials/`; only the link below it leads there.
    let ws = setup.workspace().join("ws");
    fs::create_dir_all(&ws).unwrap();
    std::os::unix::fs::symlink(fs::canonicalize(&credentials).unwrap(), ws.join("link")).unwrap();
    let events = [function_call(
        "call_grep",
        "shell",
        &json!({"command": "grep -R quorum ws"}),
    )];
    let server = ProviderServer::start([stream(&events), hello()]).unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "search the workspace"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_no_marker("stdout", run.stdout.as_bytes());
    assert!(
        !run.kinds().contains(&"tool_call_started"),
        "a link-following grep ran without review"
    );
    let resolved = run
        .lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(resolved["payload"]["decided_by"], "no_reviewer");
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert!(resolved["payload"].get("reviewer").is_none());
    assert_eq!(completed_call(&run)["payload"]["status"], "denied");
    for (index, request) in server.requests().iter().enumerate() {
        assert_no_marker(&format!("request {index}"), &request.body);
    }
    assert_session_has_no_marker(&run.session_dir(&setup));
}

/// A 1x1 PNG, 69 bytes: within every cap, so the image child stores it byte
/// for byte.
const PIXEL: [u8; 69] = [
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0xc9, 0xfe, 0x92, 0xef, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e,
    0x44, 0xae, 0x42, 0x60, 0x82,
];

/// [`PIXEL`] as base64.
const PIXEL_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC";

/// An `anthropic-messages` stream of `events`, then its end.
fn anthropic(events: &[Value]) -> Response {
    let mut all = vec![json!({"type": "message_start", "message": {"id": "msg_1"}})];
    all.extend(events.iter().cloned());
    all.push(
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
        "usage": {"input_tokens": 10, "output_tokens": 3}}),
    );
    all.push(json!({"type": "message_stop"}));
    Response::stream(
        all.iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>(),
    )
}

/// An `anthropic-messages` reply that calls `read` on `path`.
fn anthropic_read(path: &str) -> Response {
    anthropic(&[
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "input_json_delta", "partial_json": json!({"path": path}).to_string()}}),
        json!({"type": "content_block_stop", "index": 0}),
    ])
}

/// An `anthropic-messages` reply of `Hello.`.
fn anthropic_hello() -> Response {
    anthropic(&[
        json!({"type": "content_block_start", "index": 0,
            "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0,
            "delta": {"type": "text_delta", "text": "Hello."}}),
        json!({"type": "content_block_stop", "index": 0}),
    ])
}

/// The `tool_result` block of a request body's messages.
fn tool_result(body: &[u8]) -> Value {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .find(|block| block["type"] == "tool_result")
        .unwrap()
}

/// [`read_kinds`] on the Anthropic stream: the call's arguments arrive as a
/// delta, and the second reply is one text delta.
fn anthropic_read_kinds() -> Vec<&'static str> {
    let mut kinds = read_kinds();
    kinds.retain(|kind| *kind != "assistant_message_delta");
    kinds.insert(
        kinds
            .iter()
            .position(|kind| *kind == "tool_call_requested")
            .unwrap(),
        "tool_call_arguments_delta",
    );
    let text = kinds
        .iter()
        .position(|kind| *kind == "text_completed")
        .unwrap();
    kinds.insert(text, "assistant_message_delta");
    kinds
}

fn completed_line(run: &Run) -> &Value {
    run.lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap()
}

#[test]
fn a_tool_call_with_no_arguments_is_logged_and_sent_back_as_an_empty_object() {
    // Anthropic streams a call with no arguments as `input: {}` and no
    // deltas; the log and the next request carried `""`, an HTTP 400 (#1295).
    let setup = Setup::new();
    let server = ProviderServer::start([
        anthropic(&[
            json!({"type": "content_block_start", "index": 0, "content_block": {
                "type": "tool_use", "id": "toolu_1", "name": "read", "input": {}}}),
            json!({"type": "content_block_stop", "index": 0}),
        ]),
        anthropic_hello(),
        anthropic_hello(),
    ])
    .unwrap();
    setup.provider_on(&server, "anthropic-messages");

    let run = setup.run(&["ask", "call it"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // The call streams no argument text, and `read` without a path fails
    // before it starts.
    let mut kinds = anthropic_read_kinds();
    kinds.retain(|kind| !matches!(*kind, "tool_call_arguments_delta" | "tool_call_started"));
    assert_eq!(run.kinds(), kinds);
    let requested = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested")
        .unwrap();
    assert_eq!(requested["payload"]["arguments"], json!({}));
    let body: Value = serde_json::from_slice(&server.requests()[1].body).unwrap();
    let sent = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .find(|block| block["type"] == "tool_use")
        .unwrap();
    assert_eq!(sent["input"], json!({}));
}

#[test]
fn an_image_is_stored_logged_by_path_and_sent_inside_the_tool_result_on_every_request() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("pic.png"), PIXEL).unwrap();
    let server = ProviderServer::start([
        anthropic_read("pic.png"),
        anthropic_hello(),
        anthropic_hello(),
    ])
    .unwrap();
    setup.provider_on(&server, "anthropic-messages");

    let run = setup.run(&["ask", "look at the picture"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), anthropic_read_kinds());
    let completed = completed_line(&run);
    assert_eq!(completed["payload"]["status"], "completed");
    let content = completed["payload"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["text"], "Image: 1x1 image/png.\n");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mime_type"], "image/png");
    assert_eq!(
        (content[1]["width"].clone(), content[1]["height"].clone()),
        (json!(1), json!(1))
    );
    let path = content[1]["path"].as_str().unwrap();
    assert!(
        path.starts_with("artifacts/i_") && path.ends_with(".png"),
        "{path}"
    );
    let session = run.session_dir(&setup);
    // The artifact is the processed file: here, the input byte for byte.
    assert_eq!(fs::read(session.join(path)).unwrap(), PIXEL);
    // The log names the file and never holds its bytes.
    let log = fs::read(session.join("events.jsonl")).unwrap();
    assert!(
        !holds_marker(&log, PIXEL_BASE64),
        "the log holds the image's base64"
    );
    assert!(
        !log.windows(PIXEL.len()).any(|window| window == PIXEL),
        "the log holds the image's bytes"
    );

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_result(&requests[1].body)["content"],
        json!([
            {"type": "text", "text": "Image: 1x1 image/png.\n"},
            {"type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": PIXEL_BASE64}},
        ])
    );

    let id = run.session_id().to_owned();
    let resumed = setup.run(&["ask", "--resume", &id, "again"]);
    assert_eq!(resumed.code, Some(0), "stderr: {}", resumed.stderr);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        tool_result(&requests[2].body),
        tool_result(&requests[1].body),
        "a resume sends the same bytes"
    );
}

#[test]
fn a_model_without_an_image_input_gets_no_image_part_and_the_result_says_so() {
    // No `input` at all, and an `input` that lists only text.
    let deadline = Deadline::start();
    for input in [None, Some(vec!["text"])] {
        let setup = Setup::within(deadline);
        fs::write(setup.workspace().join("pic.png"), PIXEL).unwrap();
        let server = ProviderServer::start([
            anthropic_read("pic.png"),
            anthropic_hello(),
            anthropic_hello(),
        ])
        .unwrap();
        setup.provider_on_input(&server, "anthropic-messages", input);

        let run = setup.run(&["ask", "look at the picture"]);

        assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
        let requests = server.requests();
        assert_eq!(requests.len(), 2);
        let content = tool_result(&requests[1].body)["content"].clone();
        let text = content.as_str().expect("the content stays a plain string");
        assert!(
            text.starts_with("Image: 1x1 image/png.\n[Image artifacts/i_"),
            "{text}"
        );
        assert!(
            text.ends_with(".png left out: this model does not take images.]"),
            "{text}"
        );
        assert!(
            !String::from_utf8_lossy(&requests[1].body).contains(PIXEL_BASE64),
            "no image part reaches the request"
        );
    }
}

/// A PNG that holds a signature, an IHDR of `width` x `height` and the start
/// of an IDAT, and no pixels.
fn header_only_png(width: u32, height: u32) -> Vec<u8> {
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFF_u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(data.len()).unwrap().to_be_bytes().to_vec();
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32(&body).to_be_bytes());
        out
    }
    let mut ihdr = width.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend(chunk(b"IHDR", &ihdr));
    out.extend(chunk(b"IDAT", &[0x78, 0x9C, 0x00]));
    out
}

#[test]
fn an_image_over_50_megapixels_fails_unsupported_file_with_the_pixel_count() {
    let setup = Setup::new();
    fs::write(
        setup.workspace().join("big.png"),
        header_only_png(8000, 7000),
    )
    .unwrap();
    let server = ProviderServer::start([anthropic_read("big.png"), anthropic_hello()]).unwrap();
    setup.provider_on(&server, "anthropic-messages");

    let run = setup.run(&["ask", "look at the big picture"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), anthropic_read_kinds());
    let completed = completed_line(&run);
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "unsupported_file");
    let message = completed["payload"]["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("8000x7000 is 56000000 pixels; the limit is 50000000"),
        "{message}"
    );
    let images = fs::read_dir(run.session_dir(&setup).join("artifacts"))
        .unwrap()
        .count();
    assert_eq!(images, 0, "a refused image stores nothing");
    let result = tool_result(&server.requests()[1].body);
    assert_eq!(result["is_error"], true);
    assert!(result["content"].is_string());
}

/// Configuration with a handoff trigger of five tokens: any reply's prompt
/// passes it.
fn tiny_trigger(setup: &Setup) {
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "handoff": {"tokens": 5}}),
    );
}

/// A turn whose first reply reads a file, then hands off on the five-token
/// trigger and answers from the new context, to the end of the process.
const HANDOFF_TURN: &[&str] = &[
    "turn_started",
    "step_started",
    "assistant_message_started",
    "tool_call_requested",
    "usage_recorded",
    "assistant_message_completed",
    "tool_call_started",
    "tool_call_completed",
    "step_started",
    "handoff_started",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "handoff_completed",
    "opening_message",
    "assistant_message_started",
    "assistant_message_delta",
    "assistant_message_delta",
    "text_completed",
    "usage_recorded",
    "assistant_message_completed",
    "turn_completed",
    "fiber_exited",
];

/// The lines of `run` of `kind`.
fn of_kind<'a>(run: &'a Run, kind: &str) -> Vec<&'a Value> {
    run.lines
        .iter()
        .filter(|line| line["kind"] == kind)
        .collect()
}

#[test]
fn handoff_configuration_reaches_a_new_session() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    tiny_trigger(&setup);

    let run = setup.run(&["ask", "read the note"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            &[
                "session_started",
                "fiber_started",
                "extensions_loaded",
                "preamble_built",
                "opening_message",
            ][..],
            HANDOFF_TURN,
        ]
        .concat()
    );
    assert_eq!(
        of_kind(&run, "preamble_built")[0]["payload"]["trigger_at"],
        5
    );
    assert_eq!(of_kind(&run, "handoff_started").len(), 1);
    assert_eq!(
        of_kind(&run, "handoff_completed")[0]["payload"]["outcome"],
        "completed"
    );
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn handoff_configuration_reaches_a_resumed_session() {
    let setup = Setup::new();
    fs::write(setup.workspace().join("note.txt"), "alpha line\n").unwrap();
    let server = ProviderServer::start([
        hello(),
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "note.txt"}),
        )]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    tiny_trigger(&setup);
    let first = setup.run(&["ask", "one"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    // One reply, and no second step: nothing handed off yet.
    assert_eq!(
        first.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let id = first.session_id().to_owned();

    let second = setup.run(&["ask", "--resume", &id, "read the note"]);

    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(
        second.kinds(),
        [
            &["fiber_started", "extensions_loaded", "preamble_built"][..],
            HANDOFF_TURN,
        ]
        .concat()
    );
    assert_eq!(
        of_kind(&second, "preamble_built")[0]["payload"]["trigger_at"],
        5
    );
    assert_eq!(of_kind(&second, "handoff_started").len(), 1);
    assert_eq!(server.requests().len(), 4);
}

#[test]
fn a_background_shell_job_is_waited_for_before_ask_exits() {
    let setup = Setup::new();
    let ready = setup.workspace().join("ready");
    mkfifo(&setup, &ready);
    // Held read-write, the FIFO always has a writer: neither this open nor
    // the job's blocks, and the job's `read` waits for the line `act` writes.
    let release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&ready)
        .unwrap();
    // The job waits for a line on the FIFO: past the receipt, the
    // final reply and the ending notice. One plain command, so a standing
    // rule can match it.
    let script = setup.workspace().join("wait.sh");
    fs::write(&script, "read -r _ < ready\necho done\n").unwrap();
    // The job runs in its own session and group, outside fiber's: this
    // guard kills it if the test fails, and if this process dies before
    // the job opens the FIFO, which then has no writer.
    let job_guard = Watchdog::matching(script.to_str().unwrap());
    let command = format!("sh {}", script.display());
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_bg",
            "shell",
            &json!({"command": command, "run_in_background": true}),
        )]),
        hello(),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    // A shell call is reviewed; a global standing rule allows this one.
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": command})
        ),
    )
    .unwrap();

    let run = setup.run_then(
        &["ask", "start the job"],
        |lines| {
            lines
                .iter()
                .position(|line| line["kind"] == "jobs_pending_notified")
                .is_some_and(|at| {
                    lines[at..]
                        .iter()
                        .any(|line| line["kind"] == "turn_completed")
                })
        },
        || std::io::Write::write_all(&mut &release, b"\n").unwrap(),
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // A reply that is text alone: one step, then the turn's end.
    let reply = [
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ];
    let expected = [
        &[
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
            "permission_resolved",
            "tool_call_started",
            "job_started",
            "tool_call_completed",
            "step_started",
        ][..],
        &reply,
        &["turn_started", "step_started", "jobs_pending_notified"],
        &reply,
        &["turn_started", "step_started", "job_completed"],
        &reply,
        &["fiber_exited"],
    ]
    .concat();
    // `job_delta` is ephemeral, and where it lands depends on when the
    // job writes, so the comparison leaves it out.
    let lines: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| line["kind"] != "job_delta")
        .collect();
    let kinds: Vec<&str> = lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, expected);
    let job_id = lines[13]["payload"]["job_id"].as_str().unwrap();
    // The ending notice's turn starts with Fiber's own message.
    let notice = &lines[23]["payload"]["input"];
    assert_eq!(notice[0]["source"], "fiber");
    assert!(notice[0].get("command_id").is_none());
    assert!(
        notice[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(job_id)
    );
    assert_eq!(lines[25]["payload"]["job_ids"], json!([job_id]));
    let completed = &lines[35];
    assert_eq!(completed["payload"]["job_id"], job_id);
    assert_eq!(completed["payload"]["status"], "completed");
    job_guard.stand_down(setup.deadline.cleanup());
    assert!(completed.get("action_id").is_none_or(Value::is_null));
    assert_eq!(server.requests().len(), 4);
}

#[test]
fn a_monitors_lines_reach_the_log_before_its_end_and_ask_exits() {
    let setup = Setup::new();
    let ready = setup.workspace().join("ready");
    mkfifo(&setup, &ready);
    // Held read-write, the FIFO always has a writer: neither this open nor
    // the job's blocks, and the job's `read` waits for the line `act` writes.
    let release = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&ready)
        .unwrap();
    // The monitor prints once the test writes a line: past the receipt,
    // the final reply and the ending notice. Both lines go out in one
    // write, so they are one batch.
    let script = setup.workspace().join("watch.sh");
    fs::write(&script, "read -r _ < ready\nprintf 'one\\ntwo\\n'\n").unwrap();
    // The monitor runs in its own session and group, outside fiber's: this
    // guard kills it if the test fails, and if this process dies before
    // the monitor opens the FIFO, which then has no writer.
    let job_guard = Watchdog::matching(script.to_str().unwrap());
    let command = format!("sh {}", script.display());
    // How many turns the lines and the end take depends on when they
    // arrive, so every later request is answered with text.
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_monitor",
            "shell",
            &json!({"command": command, "monitor": true}),
        )]),
        hello(),
        hello(),
        hello(),
        hello(),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": command})
        ),
    )
    .unwrap();

    let run = setup.run_then(
        &["ask", "start the monitor"],
        |lines| {
            lines
                .iter()
                .position(|line| line["kind"] == "jobs_pending_notified")
                .is_some_and(|at| {
                    lines[at..]
                        .iter()
                        .any(|line| line["kind"] == "turn_completed")
                })
        },
        || std::io::Write::write_all(&mut &release, b"\n").unwrap(),
    );

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // A reply that is text alone: one step, then the turn's end.
    let reply = [
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
    ];
    let opening = [
        &[
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
            "permission_resolved",
            "tool_call_started",
            "job_started",
            "tool_call_completed",
            "step_started",
        ][..],
        &reply,
        &["turn_started", "step_started", "jobs_pending_notified"],
        &reply,
    ]
    .concat();
    // The lines wake the model. How they split into deliveries, and so how
    // many turns and steps follow, depends on when each arrives (one write
    // can arrive as one batch or two). So the rest of the run is any number
    // of turns, each of steps that take a reply, with the end delivered at
    // one step's start; the checks below place the `job_line` deliveries.
    let step = &reply[..reply.len() - 1];
    let rest = &opening.len();
    // `job_delta` is ephemeral, and where it lands depends on when the
    // job writes, so the comparison leaves it out.
    let lines: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| line["kind"] != "job_delta")
        .collect();
    let all_kinds: Vec<&str> = lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let kinds: Vec<&str> = all_kinds
        .iter()
        .copied()
        .filter(|kind| *kind != "job_line")
        .collect();
    let tail = &kinds[*rest..kinds.len() - 1];
    assert_eq!(kinds.last(), Some(&"fiber_exited"), "{all_kinds:?}");
    assert_eq!(&kinds[..*rest], &opening[..], "{all_kinds:?}");
    let mut at = 0;
    let mut ends = 0;
    while at < tail.len() {
        assert_eq!(
            tail[at..].get(..2),
            Some(&["turn_started", "step_started"][..])
        );
        at += 2;
        loop {
            if tail.get(at) == Some(&"job_completed") {
                ends += 1;
                at += 1;
            }
            assert_eq!(tail[at..].get(..step.len()), Some(step), "{all_kinds:?}");
            at += step.len();
            if tail.get(at) == Some(&"turn_completed") {
                at += 1;
                break;
            }
            assert_eq!(tail.get(at), Some(&"step_started"), "{all_kinds:?}");
            at += 1;
        }
    }
    assert_eq!(ends, 1, "{all_kinds:?}");
    let job_id = lines[13]["payload"]["job_id"].as_str().unwrap();
    let receipt = lines[14]["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(receipt.starts_with("Started a monitor.\n"), "{receipt}");
    // Each delivery lands after the notice and before the end; together they
    // are exactly what the monitor printed.
    let notified = all_kinds
        .iter()
        .position(|kind| *kind == "jobs_pending_notified")
        .unwrap();
    let ended_at = all_kinds
        .iter()
        .position(|kind| *kind == "job_completed")
        .unwrap();
    let mut delivered = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line["kind"] == "job_line" {
            assert!(notified < index && index < ended_at, "{all_kinds:?}");
            assert_eq!(line["payload"]["job_id"], job_id);
            assert!(line["payload"].get("suppressed").is_none());
            assert!(line.get("action_id").is_none_or(Value::is_null));
            delivered.push(line["payload"]["lines"].as_str().unwrap());
        }
    }
    assert!(!delivered.is_empty(), "{all_kinds:?}");
    assert_eq!(delivered.join("\n"), "one\ntwo", "{all_kinds:?}");
    let completed = &lines[ended_at];
    assert_eq!(completed["payload"]["job_id"], job_id);
    assert_eq!(completed["payload"]["status"], "completed");
    job_guard.stand_down(setup.deadline.cleanup());
    // The deltas left out above: all of this job's, inside its life, and
    // together exactly what it printed, so a missing or duplicated one fails.
    let all: Vec<&str> = run
        .lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    let started = all.iter().position(|kind| *kind == "job_started").unwrap();
    let ended = all
        .iter()
        .position(|kind| *kind == "job_completed")
        .unwrap();
    let mut printed = String::new();
    for (index, line) in run.lines.iter().enumerate() {
        if line["kind"] == "job_delta" {
            assert!(started < index && index < ended, "{all:?}");
            assert_eq!(line["payload"]["job_id"], job_id);
            printed.push_str(line["payload"]["text"].as_str().unwrap());
        }
    }
    assert_eq!(printed, "one\ntwo\n");
}

/// A page server with one 200 answer of `content_type` and `body`.
fn page(content_type: &str, body: &str) -> ProviderServer {
    ProviderServer::start([Response {
        status: 200,
        headers: vec![("content-type".to_owned(), content_type.to_owned())],
        body: body.as_bytes().to_vec(),
        drop_connection: false,
        stall: false,
    }])
    .unwrap()
}

/// A global standing rule that allows `web_fetch` of every URL at `server`.
fn allow_fetch_of(setup: &Setup, server: &ProviderServer) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "web_fetch", "prefix": format!("{}/", server.url())})
        ),
    )
    .unwrap();
}

fn completed_call(run: &Run) -> &Value {
    run.lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap()
}

#[test]
fn a_web_fetch_allowed_by_a_standing_rule_returns_the_pages_first_line() {
    let setup = Setup::new();
    let site = page(
        "text/html; charset=utf-8",
        "<h1>Hello</h1><p>from the page</p>",
    );
    let url = format!("{}/doc", site.url());
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_fetch",
            "web_fetch",
            &json!({"url": url}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    allow_fetch_of(&setup, &site);

    let run = setup.run(&["ask", "fetch the page"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
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
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let resolved = run
        .lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(resolved["payload"]["decided_by"], "standing_rule");
    let completed = completed_call(&run);
    assert_eq!(completed["payload"]["status"], "completed");
    let got = completed["payload"]["content"][0]["text"].as_str().unwrap();
    let prefix = format!("{url} 200 text/html; charset=utf-8; raw page at ");
    assert!(got.starts_with(&prefix), "{got}");
    assert!(got.ends_with("\n\n# Hello\n\nfrom the page\n"), "{got}");
    assert_eq!(site.requests().len(), 1);
    assert_eq!(site.requests()[0].path, "/doc");
}

#[test]
fn a_fetched_page_over_16_kib_is_cut_and_its_whole_markdown_is_in_the_artifact() {
    let setup = Setup::new();
    let paragraph = "<p>0123456789 0123456789 0123456789 0123456789</p>";
    let html = paragraph.repeat(20_000 / paragraph.len() + 1);
    let markdown =
        "0123456789 0123456789 0123456789 0123456789\n\n".repeat(20_000 / paragraph.len() + 1);
    let markdown = format!("{}\n", markdown.trim_end());
    let site = page("text/html", &html);
    let url = format!("{}/long", site.url());
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_fetch",
            "web_fetch",
            &json!({"url": url}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    allow_fetch_of(&setup, &site);

    let run = setup.run(&["ask", "fetch the long page"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
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
            "permission_resolved",
            "tool_call_started",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let completed = completed_call(&run);
    assert_eq!(completed["payload"]["status"], "completed");
    let artifact = completed["payload"]["artifact"].as_str().unwrap();
    let kept = fs::read_to_string(run.session_dir(&setup).join(artifact)).unwrap();
    let prefix = format!("{url} 200 text/html; raw page at ");
    assert!(kept.starts_with(&prefix), "{kept}");
    assert!(kept.ends_with(&format!("\n\n{markdown}")), "{kept}");
    let shown = completed["payload"]["content"][0]["text"].as_str().unwrap();
    assert!(shown.starts_with(&prefix), "{shown}");
    assert!(shown.len() < kept.len(), "the result was not cut");
}

#[test]
fn a_web_fetch_with_no_rule_is_judged_at_step_7() {
    let setup = Setup::new();
    let site = page("text/plain", "never fetched");
    let url = format!("{}/doc", site.url());
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_fetch",
            "web_fetch",
            &json!({"url": url}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "fetch the page"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
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
            "notice",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let resolved = run
        .lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(resolved["payload"]["decided_by"], "no_reviewer");
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert!(resolved["payload"].get("reviewer").is_none());
    assert_eq!(completed_call(&run)["payload"]["status"], "denied");
    assert!(site.requests().is_empty(), "a denied call sends nothing");
}

#[test]
fn markdown_writes_in_extension_data_directories_take_the_fast_path() {
    let setup = Setup::new();
    // Absolute paths: the tools join relative paths to the workspace.
    let machine = setup.home().join("data/notes/a.md").display().to_string();
    // The project key, as `Run::session_dir` derives it.
    let key = fs::canonicalize(setup.workspace())
        .unwrap()
        .to_string_lossy()
        .replace('/', "-");
    let project_file = setup
        .home()
        .join("projects")
        .join(key)
        .join("data/notes/b.md");
    let project = project_file.display().to_string();
    let edit = |path: &str| json!({"path": path, "edits": [{"old_text": "first\n", "new_text": "edited\n"}]});
    let server = ProviderServer::start([
        stream(&[function_call(
            "write_a",
            "write",
            &json!({"path": machine, "content": "first\n"}),
        )]),
        stream(&[function_call("edit_a", "edit", &edit(&machine))]),
        stream(&[function_call(
            "write_b",
            "write",
            &json!({"path": project, "content": "first\n"}),
        )]),
        stream(&[function_call("edit_b", "edit", &edit(&project))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "keep notes"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let mut expected = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
    ];
    for _ in 0..4 {
        expected.extend([
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
        ]);
    }
    expected.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    assert_eq!(run.kinds(), expected);
    let completed: Vec<_> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), 4);
    for done in completed {
        assert_eq!(done["payload"]["status"], "completed");
    }
    assert_eq!(
        fs::read_to_string(setup.home().join("data/notes/a.md")).unwrap(),
        "edited\n"
    );
    assert_eq!(fs::read_to_string(&project_file).unwrap(), "edited\n");
    // The fast path writes no `permission_` line and asks no reviewer: the
    // provider server saw exactly the five scripted replies.
    assert!(
        run.lines
            .iter()
            .all(|line| !line["kind"].as_str().unwrap().starts_with("permission_"))
    );
    assert_eq!(server.requests().len(), 5);
}

/// The event kinds of a turn whose first reply makes one reviewed write,
/// which the reviewer blocks, and whose second is [`hello`]. Headless, no
/// person can answer, so the block writes `permission_resolved` with no
/// `permission_requested`; each reviewer stage still records its usage.
fn blocked_write_kinds() -> Vec<&'static str> {
    vec![
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
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]
}

/// A run whose one reviewed call the reviewer blocked (`docs/testing.md`, "What
/// a test asserts"): the call is refused, it never starts, and the target
/// bytes are unchanged. The `permission_resolved` line names the reviewer
/// as decider, and the provider server saw the reviewer's two stages
/// beside the session's two replies.
fn assert_blocked_by_reviewer(run: &Run, server: &ProviderServer) {
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), blocked_write_kinds());
    let requested = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested")
        .unwrap();
    let action = &requested["action_id"];
    let resolved = run
        .lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(&resolved["action_id"], action);
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert_eq!(resolved["payload"]["decided_by"], "reviewer");
    assert_eq!(
        resolved["payload"]["reviewer"],
        json!({"model": "fake/m", "stage": 2})
    );
    let done = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(&done["action_id"], action);
    assert_eq!(done["payload"]["status"], "denied");
    assert_eq!(done["payload"]["reason"], "reviewer");
    assert!(
        !run.lines
            .iter()
            .any(|line| line["kind"] == "tool_call_started" && &line["action_id"] == action)
    );
    assert_eq!(server.requests().len(), 4);
}

#[test]
fn a_command_credential_runs_once_per_process_with_a_same_provider_reviewer() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let counter = setup.home().join("counter");
    let probe = format!("echo run >> '{}'; echo k", counter.display());
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "reviewer": {"model": "fake/m"},
            "providers": {"fake": {"credentials": {"default": {"command": ["sh", "-c", probe]}}}}}),
    );

    let run = setup.run(&["ask", "hello"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let runs = fs::read_to_string(&counter)
        .unwrap_or_default()
        .lines()
        .count();
    assert_eq!(runs, 1, "the command ran {runs} times");
}

/// Points the session's reviewer at the fake server, beside the session's
/// own model.
fn with_reviewer(setup: &Setup) {
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "reviewer": {"model": "fake/m"}}),
    );
}

#[test]
fn a_lua_write_in_a_data_directory_is_reviewed_and_blocked() {
    let setup = Setup::new();
    let lua = setup.home().join("data/notes/x.lua").display().to_string();
    let server = ProviderServer::start([
        stream(&[function_call(
            "write_lua",
            "write",
            &json!({"path": lua, "content": "return {}\n"}),
        )]),
        text_reply("check"),
        text_reply("block Lua files may run as code"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    with_reviewer(&setup);

    let run = setup.run(&["ask", "save the snippet"]);

    assert_blocked_by_reviewer(&run, &server);
    assert!(!setup.home().join("data/notes/x.lua").exists());
}

#[test]
fn a_reviewed_call_at_the_spending_budget_is_denied_by_the_budget() {
    let setup = Setup::new();
    let lua = setup.home().join("data/notes/x.lua").display().to_string();
    let server = ProviderServer::start([
        stream(&[function_call(
            "write_lua",
            "write",
            &json!({"path": lua, "content": "return {}\n"}),
        )]),
        hello(),
    ])
    .unwrap();
    // The first reply's 3 output tokens alone cost $0.003, so the reviewed
    // call is denied at the budget before any reviewer request is sent.
    setup.provider_priced(&server, json!({"input": 1000.0, "output": 1000.0}));
    write(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "reviewer": {"model": "fake/m"}, "budget": {"usd": 0.001}}),
    );

    let run = setup.run(&["ask", "save the snippet"]);

    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
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
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert!(
        !run.lines
            .iter()
            .any(|line| line["kind"] == "permission_requested")
    );
    let requested = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested")
        .unwrap();
    let action = &requested["action_id"];
    let resolved = run
        .lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(&resolved["action_id"], action);
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert_eq!(resolved["payload"]["decided_by"], "budget");
    assert_eq!(
        resolved["payload"]["reason"],
        "The session reached its spending budget."
    );
    assert!(resolved["payload"].get("reviewer").is_none());
    assert!(resolved["payload"].get("request_id").is_none());
    let done = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(&done["action_id"], action);
    assert_eq!(done["payload"]["status"], "denied");
    assert_eq!(done["payload"]["reason"], "budget_exceeded");
    let end = run
        .lines
        .iter()
        .find(|line| line["kind"] == "turn_completed")
        .unwrap();
    assert_eq!(end["payload"]["outcome"], "failed");
    assert_eq!(end["payload"]["error"]["code"], "budget_exceeded");
    // No reviewer request was sent: the only request is the session's own
    // first reply.
    assert_eq!(server.requests().len(), 1);
    assert!(!setup.home().join("data/notes/x.lua").exists());
}

#[test]
fn a_failed_reviewer_at_stage_1_denies_naming_the_reviewer() {
    let setup = Setup::new();
    let lua = setup.home().join("data/notes/x.lua").display().to_string();
    let server = ProviderServer::start([
        stream(&[function_call(
            "write_lua",
            "write",
            &json!({"path": lua, "content": "return {}\n"}),
        )]),
        Response::status(400, "{}"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    with_reviewer(&setup);

    let run = setup.run(&["ask", "save the snippet"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        !run.lines
            .iter()
            .any(|line| line["kind"] == "permission_requested")
    );
    let requested = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested")
        .unwrap();
    let action = &requested["action_id"];
    let resolved = run
        .lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(&resolved["action_id"], action);
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert_eq!(resolved["payload"]["decided_by"], "reviewer");
    assert_eq!(
        resolved["payload"]["reviewer"],
        json!({"model": "fake/m", "stage": 1})
    );
    assert!(resolved["payload"].get("request_id").is_none());
    let done = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(&done["action_id"], action);
    assert_eq!(done["payload"]["status"], "denied");
    assert_eq!(done["payload"]["reason"], "reviewer");
    assert_eq!(
        run.kinds(),
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
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(server.requests().len(), 3);
    assert!(!setup.home().join("data/notes/x.lua").exists());
}

#[test]
fn a_failed_reviewer_at_stage_2_denies_naming_the_reviewer() {
    let setup = Setup::new();
    let lua = setup.home().join("data/notes/x.lua").display().to_string();
    let server = ProviderServer::start([
        stream(&[function_call(
            "write_lua",
            "write",
            &json!({"path": lua, "content": "return {}\n"}),
        )]),
        text_reply("check"),
        Response::status(400, "{}"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    with_reviewer(&setup);

    let run = setup.run(&["ask", "save the snippet"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        !run.lines
            .iter()
            .any(|line| line["kind"] == "permission_requested")
    );
    let requested = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_requested")
        .unwrap();
    let action = &requested["action_id"];
    let resolved = run
        .lines
        .iter()
        .find(|line| line["kind"] == "permission_resolved")
        .unwrap();
    assert_eq!(&resolved["action_id"], action);
    assert_eq!(resolved["payload"]["decision"], "deny");
    assert_eq!(resolved["payload"]["decided_by"], "reviewer");
    assert_eq!(
        resolved["payload"]["reviewer"],
        json!({"model": "fake/m", "stage": 2})
    );
    assert!(resolved["payload"].get("request_id").is_none());
    let done = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(&done["action_id"], action);
    assert_eq!(done["payload"]["status"], "denied");
    assert_eq!(done["payload"]["reason"], "reviewer");
    assert_eq!(
        run.kinds(),
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
            "usage_recorded",
            "permission_resolved",
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(server.requests().len(), 4);
    assert!(!setup.home().join("data/notes/x.lua").exists());
}

#[test]
fn a_markdown_write_escaping_its_data_directory_through_a_link_is_reviewed_and_blocked() {
    let setup = Setup::new();
    fs::create_dir_all(setup.home().join("../outside")).unwrap();
    let outside = fs::canonicalize(setup.home().join("../outside")).unwrap();
    let target = outside.join("kept.md");
    fs::write(&target, "away\n").unwrap();
    let notes = setup.home().join("data/notes");
    fs::create_dir_all(&notes).unwrap();
    std::os::unix::fs::symlink(&target, notes.join("out.md")).unwrap();
    let link = notes.join("out.md").display().to_string();
    let server = ProviderServer::start([
        stream(&[function_call(
            "write_link",
            "write",
            &json!({"path": link, "content": "changed\n"}),
        )]),
        text_reply("check"),
        text_reply("block the link leaves its data directory"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    with_reviewer(&setup);

    let run = setup.run(&["ask", "save the note"]);

    assert_blocked_by_reviewer(&run, &server);
    assert_eq!(fs::read(&target).unwrap(), b"away\n");
    assert!(
        fs::symlink_metadata(notes.join("out.md"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_shell_read_of_proc_environ_is_reviewed_and_blocked() {
    // The provider's key comes from `FIBER_TEST_FAKE_KEY`, an `env` source,
    // so the session's own environment holds it (`docs/configuration.md`,
    // "Secrets").
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call(
            "cat_environ",
            "shell",
            &json!({"command": "cat /proc/self/environ"}),
        )]),
        text_reply("check"),
        text_reply("block it reads the environment"),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    with_reviewer(&setup);

    let run = setup.run(&["ask", "show the environment"]);

    assert_blocked_by_reviewer(&run, &server);
    for (index, request) in server.requests().iter().enumerate() {
        assert!(
            !holds_marker(&request.body, "FIBER_TEST_FAKE_KEY="),
            "request {index} holds the environment"
        );
    }
}

/// The result of an `ask_user` call whose questions went to the driver.
const SENT: &str = "The questions went to the driver. The answers arrive as the next prompt.";

/// The request's tool named `name`.
fn tool_in<'a>(body: &'a Value, name: &str) -> &'a Value {
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap()
}

#[test]
fn ask_user_ends_the_run_with_its_questions_and_a_resume_answers_them() {
    let setup = Setup::new();
    let questions = json!([
        {"header": "Base", "question": "Which branch?", "options": [
            {"label": "main (Recommended)"},
            {"label": "dev", "description": "The development branch"}
        ]},
        {"header": "Name", "question": "What name?"}
    ]);
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_ask",
            "ask_user",
            &json!({"questions": questions}),
        )]),
        text_reply("Done."),
    ])
    .unwrap();
    setup.provider(&server);

    let first = setup.run(&["ask", "start"]);

    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let mut expected = read_kinds();
    let cut = expected
        .iter()
        .position(|kind| *kind == "tool_call_completed")
        .unwrap();
    expected.truncate(cut + 1);
    expected.extend(["turn_completed", "fiber_exited"]);
    assert_eq!(first.kinds(), expected);
    let completed = first
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], SENT);
    let ended = &first.lines[first.lines.len() - 2];
    assert_eq!(ended["kind"], "turn_completed");
    assert_eq!(
        ended["payload"],
        json!({"outcome": "completed", "questions": questions})
    );
    let exited = first.lines.last().unwrap();
    assert_eq!(exited["payload"]["exit_code"], 0);
    assert_eq!(exited["payload"]["questions"], questions);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(tool_in(&body, "ask_user")["strict"], false);
    assert_eq!(tool_in(&body, "handoff")["strict"], true);

    let id = first.session_id().to_owned();
    let second = setup.run(&["ask", "--resume", &id, "main; call it fiber"]);

    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    // No `session_started`: the session keeps its first line. No
    // `opening_message` either: the log already holds one. The resumed
    // reply says "Done." as one finished message, with no deltas.
    assert_eq!(
        second.kinds(),
        [
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
        ]
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let body: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let input = body["input"].as_array().unwrap();
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(output["call_id"], "call_ask");
    assert_eq!(output["output"], SENT);
    let last = input.last().unwrap();
    assert_eq!(last["role"], "user");
    assert!(last.to_string().contains("main; call it fiber"), "{last}");
    let exited = second.lines.last().unwrap();
    assert_eq!(exited["kind"], "fiber_exited");
    assert!(exited["payload"].get("questions").is_none(), "{exited}");
}

#[test]
fn an_ask_user_call_outside_its_limits_fails_before_it_starts() {
    let setup = Setup::new();
    let question = |header: &str, options: Value| json!({"header": header, "question": "Which?", "options": options});
    let two = json!([{"label": "a"}, {"label": "b"}]);
    let free = json!({"header": "h", "question": "q"});
    let calls = [
        json!({"questions": []}),
        json!({"questions": [free, free, free, free, free]}),
        json!({"questions": [question("thirteen char", two.clone())]}),
        json!({"questions": [question("h", json!([{"label": "a"}]))]}),
        json!({"questions": [question("h", json!([
            {"label": "a"}, {"label": "b"}, {"label": "c"}, {"label": "d"}, {"label": "e"}
        ]))]}),
        json!({"questions": [{"header": "h", "question": "q", "preview": "p"}]}),
    ];
    let events: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(n, arguments)| function_call(&format!("call_{n}"), "ask_user", arguments))
        .collect();
    let server = ProviderServer::start([stream(&events), hello()]).unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "ask me"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // Every invalid call fails before it starts: no `tool_call_started`
    // is written, and the turn continues on the [`hello`] reply.
    let mut expected = vec![
        "session_started",
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "opening_message",
        "turn_started",
        "step_started",
        "assistant_message_started",
    ];
    expected.extend(["tool_call_requested"; 6]);
    expected.extend(["usage_recorded", "assistant_message_completed"]);
    expected.extend(["tool_call_completed"; 6]);
    expected.extend([
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    assert_eq!(run.kinds(), expected);
    let completed: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), calls.len());
    for line in completed {
        assert_eq!(line["payload"]["status"], "failed", "{line}");
        assert_eq!(
            line["payload"]["error"]["code"], "invalid_arguments",
            "{line}"
        );
    }
    let ended = run
        .lines
        .iter()
        .find(|line| line["kind"] == "turn_completed")
        .unwrap();
    assert_eq!(ended["payload"], json!({"outcome": "completed"}));
    assert_eq!(server.requests().len(), 2);
}
