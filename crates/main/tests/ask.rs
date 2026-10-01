//! Binary-level tests of `fiber ask` (`docs/testing.md`, "Levels"): the
//! built `fiber` runs in its own process group with its own `FIBER_HOME`,
//! holding an ordinary provider whose base URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: PathBuf,
}

impl Setup {
    fn new() -> Self {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fa{}-{n}", std::process::id()));
        fs::remove_dir_all(&root).unwrap_or(());
        fs::create_dir_all(root.join("h")).unwrap();
        fs::create_dir_all(root.join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("h")
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and makes `fake/m` the configured model.
    fn provider(&self, server: &ProviderServer) {
        let source = self.root.join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "fiber": "0.0.0", "api": 1}),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url())}]
            }),
        );
        extensions::install(&self.home(), &source, "0.0.0").unwrap();
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// Runs `fiber` with `args`, `stdin` piped in (closed when `None`) and
    /// `FIBER_HOME` set to `home`.
    fn fiber_with_home(&self, home: &str, args: &[&str], stdin: Option<&str>) -> Run {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fiber"))
            .args(args)
            .current_dir(self.root.join("w"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("FIBER_HOME", home)
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let group = child.id();
        let mut pipe = child.stdin.take().unwrap();
        if let Some(text) = stdin {
            pipe.write_all(text.as_bytes()).unwrap();
        }
        drop(pipe);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = finished
            .recv_timeout(DEADLINE)
            .unwrap_or_else(|_| {
                kill_group(group);
                panic!("waited {DEADLINE:?} for `fiber {}` to exit", args.join(" "))
            })
            .unwrap();
        assert!(
            !group_alive(group),
            "`fiber` left a process in its group behind"
        );
        Run::from(output)
    }

    fn fiber(&self, args: &[&str], stdin: Option<&str>) -> Run {
        self.fiber_with_home(self.home().to_str().unwrap(), args, stdin)
    }
}

impl Drop for Setup {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap_or(());
    }
}

fn write(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// Whether any process remains in process group `group`.
fn group_alive(group: u32) -> bool {
    Command::new("kill")
        .args(["-0", "--", &format!("-{group}")])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

fn kill_group(group: u32) {
    Command::new("kill")
        .args(["-KILL", "--", &format!("-{group}")])
        .status()
        .unwrap();
}

/// One finished run: its exit code, stdout's lines, as text and parsed, and
/// stderr.
struct Run {
    code: Option<i32>,
    raw: Vec<String>,
    lines: Vec<Value>,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let raw: Vec<String> = stdout.lines().map(str::to_owned).collect();
        let lines = raw
            .iter()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        Self {
            code: output.status.code(),
            raw,
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Run {
    fn kinds(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|l| l["kind"].as_str().unwrap())
            .collect()
    }

    fn last(&self) -> &Value {
        self.lines.last().expect("stdout has a line")
    }

    fn session_id(&self) -> &str {
        self.lines[0]["session_id"].as_str().unwrap()
    }

    /// The session's directory, from its id.
    fn session_dir(&self, setup: &Setup) -> PathBuf {
        let workspace = fs::canonicalize(setup.root.join("w")).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        setup
            .home()
            .join("projects")
            .join(key)
            .join("sessions")
            .join(self.session_id())
    }
}

/// An `openai-responses` stream answering `Hello.` in two fragments.
fn hello() -> Response {
    let events = [
        json!({"type": "response.output_text.delta", "delta": "Hel"}),
        json!({"type": "response.output_text.delta", "delta": "lo."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
        json!({"type": "response.completed", "response": {
            "id": "resp_1", "status": "completed",
            "usage": {"input_tokens": 10, "input_tokens_details": {"cached_tokens": 4}, "output_tokens": 3}
        }}),
    ];
    let body: String = events
        .iter()
        .map(|e| format!("event: {}\ndata: {e}\n\n", e["type"].as_str().unwrap()))
        .collect();
    Response::stream(body)
}

/// The text of the first message in the session's `turn_started`.
fn turn_input(run: &Run) -> &str {
    let started = run
        .lines
        .iter()
        .find(|l| l["kind"] == "turn_started")
        .unwrap();
    started["payload"]["input"][0]["content"][0]["text"]
        .as_str()
        .unwrap()
}

#[test]
fn a_prompt_as_an_argument_runs_one_turn_and_stdout_is_the_log() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "fiber_started",
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    assert_eq!(run.lines[0]["payload"]["resumed"], false);
    assert_eq!(turn_input(&run), "hi");
    let exited = &run.last()["payload"];
    assert_eq!(exited["exit_code"], 0);
    assert_eq!(exited["text"], "Hello.");
    let message = run
        .lines
        .iter()
        .find(|l| l["kind"] == "assistant_message_completed")
        .unwrap();
    assert_eq!(exited["final_action_id"], message["action_id"]);
    assert_eq!(exited["usage"]["tokens"]["output"], 3);
    assert_eq!(exited.get("error"), None);

    // Stdout filtered to this session's durable lines is the log, byte for
    // byte.
    let dir = run.session_dir(&setup);
    let durable: String = run
        .raw
        .iter()
        .zip(&run.lines)
        .filter(|(_, l)| l.get("seq").is_some() && l["session_id"] == run.session_id())
        .map(|(raw, _)| format!("{raw}\n"))
        .collect();
    assert_eq!(
        fs::read_to_string(dir.join("events.jsonl")).unwrap(),
        durable
    );

    // The socket was bound in `run/` and is gone after exit.
    assert!(setup.home().join("run").is_dir());
    assert!(!setup.home().join("run").join(run.session_id()).exists());

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/responses");
    assert_eq!(requests[0].header("authorization"), Some("<masked>"));
    assert!(String::from_utf8_lossy(&requests[0].body).contains("\"hi\""));
}

#[test]
fn a_prompt_on_stdin_runs_one_turn() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);

    let run = setup.fiber(&["ask"], Some("review the brief\n"));

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(turn_input(&run), "review the brief\n");
    assert_eq!(run.kinds().first(), Some(&"fiber_started"));
    assert_eq!(run.last()["payload"]["text"], "Hello.");
}

#[test]
fn a_failed_turn_exits_1_with_the_turns_error() {
    let setup = Setup::new();
    let server = ProviderServer::start([Response::status(503, "{}")]).unwrap();
    setup.provider(&server);

    let run = setup.fiber(&["ask", "hi"], None);

    assert_eq!(run.code, Some(1));
    assert_eq!(
        run.kinds(),
        [
            "fiber_started",
            "session_started",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ]
    );
    let turn = &run.lines[6]["payload"];
    assert_eq!(turn["outcome"], "failed");
    let exited = &run.last()["payload"];
    assert_eq!(exited["exit_code"], 1);
    assert_eq!(exited["error"], turn["error"]);
    assert_eq!(exited["error"]["code"], "provider_unavailable");
    assert_eq!(exited.get("text"), None);
    assert!(run.session_dir(&setup).join("events.jsonl").is_file());
}

/// The one line stdout holds when the process failed before any session.
fn assert_pre_session(run: &Run, exit: i32, code: &str) {
    assert_eq!(run.code, Some(exit));
    assert_eq!(run.kinds(), ["fiber_exited"]);
    let line = run.last();
    assert_eq!(line.get("session_id"), None);
    assert_eq!(line["payload"]["exit_code"], exit);
    assert_eq!(line["payload"]["error"]["code"], code);
    let message = line["payload"]["error"]["message"].as_str().unwrap();
    assert_eq!(run.stderr, format!("fiber: {message}\n"));
}

#[test]
fn two_prompts_or_none_is_a_usage_error() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);

    assert_pre_session(&setup.fiber(&["ask", "hi"], Some("and this")), 2, "usage");
    assert_pre_session(&setup.fiber(&["ask"], None), 2, "usage");
    assert_pre_session(&setup.fiber(&["ask", "a", "b"], None), 2, "usage");
    assert_pre_session(&setup.fiber(&["ask", "--model", "x"], None), 2, "usage");
    assert!(server.requests().is_empty());
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_failure_before_any_session_ends_stdout_with_fiber_exited_and_no_session_id() {
    let setup = Setup::new();

    assert_pre_session(&setup.fiber(&["ask", "hi"], None), 1, "no_model");
    assert_pre_session(&setup.fiber_with_home("", &["ask", "hi"], None), 2, "usage");
    assert_pre_session(
        &setup.fiber_with_home("rel", &["ask", "hi"], None),
        2,
        "usage",
    );
    assert!(!setup.home().join("projects").exists());
}

#[test]
fn a_missing_credential_fails_before_the_session() {
    let setup = Setup::new();
    let server = ProviderServer::start([]).unwrap();
    setup.provider(&server);
    let source = setup.home().join("extensions/fake/providers/fake.json");
    let text = fs::read_to_string(&source)
        .unwrap()
        .replace("FIBER_TEST_FAKE_KEY", "FIBER_TEST_UNSET_KEY");
    fs::write(&source, text).unwrap();

    assert_pre_session(&setup.fiber(&["ask", "hi"], None), 1, "credential_missing");
}

#[test]
fn fiber_without_ask_is_a_usage_error_naming_fiber_ask() {
    let setup = Setup::new();

    let run = setup.fiber(&[], None);

    assert_eq!(run.code, Some(2));
    assert!(run.stderr.contains("fiber ask"), "stderr: {}", run.stderr);
}
