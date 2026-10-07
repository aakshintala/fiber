//! Binary-level tests of the first-party `memory` extension with its shipped
//! manifest (`docs/memory.md`, "Testing"; `docs/testing.md`, "Levels"): the
//! built `fiber` runs in its own process group with its own `FIBER_HOME`,
//! holding a path install of the repo's `extensions/memory` and an ordinary
//! provider whose base URL is the fake server.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};

/// How long one `fiber` run may take. A test runs `fiber` at most twice,
/// each with an exit wait and a watchdog wait: 48 s of deadlines, under
/// half the 120 s nextest timeout (`docs/testing.md`, "Waits and timeouts").
const DEADLINE: Duration = Duration::from_secs(12);

/// The shipped extension's full name.
const MEMORY: &str = "github.com/aakshintala/fiber/extensions/memory";

/// Its directory name in Fiber home: its short name (`docs/state.md`,
/// "What each part holds").
const SLUG: &str = "memory";

/// The event kinds of a turn answered by [`hello`]: one text fragment in
/// two deltas.
const HELLO_KINDS: [&str; 15] = [
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
];

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fm");
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("h")
    }

    fn workspace(&self) -> PathBuf {
        self.root.path().join("w")
    }

    /// Installs the repo's shipped `extensions/memory` package from its
    /// path.
    fn memory(&self) {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/memory");
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
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url())}]
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
        write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// The project's key: the canonical workspace with every `/` as `-`.
    fn key(&self) -> String {
        fs::canonicalize(self.workspace())
            .unwrap()
            .to_string_lossy()
            .replace('/', "-")
    }

    /// Writes `content` to the memory extension's machine data directory.
    fn machine_file(&self, name: &str, content: &str) -> PathBuf {
        let path = self.home().join("data").join(SLUG).join(name);
        write_text(&path, content);
        path
    }

    /// Writes `content` to the memory extension's project data directory.
    fn project_file(&self, name: &str, content: &str) -> PathBuf {
        let path = self
            .home()
            .join("projects")
            .join(self.key())
            .join("data")
            .join(SLUG)
            .join(name);
        write_text(&path, content);
        path
    }

    /// Runs `fiber` in its own process group, waits for it under
    /// [`DEADLINE`], and asserts that nothing it started is left in the
    /// group. A watchdog beside it kills that group if this process dies
    /// first.
    fn run(&self, args: &[&str]) -> Run {
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
        let (child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match finished.recv_timeout(DEADLINE) {
            Ok(output) => output.unwrap(),
            Err(_) => {
                fakes::kill_group(group, "KILL").unwrap();
                let reaped = finished.recv_timeout(DEADLINE).is_ok();
                panic!(
                    "waited {DEADLINE:?} for `fiber {}` to exit (reaped after the kill: {reaped})",
                    args.join(" ")
                );
            }
        };
        assert!(
            !fakes::kill_group(group, "0").unwrap(),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(DEADLINE);
        Run::from(output)
    }

    /// Starts a server answering `responses`, installs the provider, and
    /// runs `fiber ask`.
    fn ask(&self, responses: Vec<Response>) -> (Run, ProviderServer) {
        let server = ProviderServer::start(responses).unwrap();
        self.provider(&server);
        let run = self.run(&["ask", "hi"]);
        assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
        (run, server)
    }
}

fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

fn write_text(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

// debt: the runner, `spawn_watched` and `KillGroup` copy `opening.rs`'s, as
// the six other binary test files in this crate each do; move all seven
// copies into `fakes` together when a change to one has to be made in all.

/// Spawns `command` in a new process group, then a watchdog in its own
/// group, which kills the group if this process dies first.
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
        match fakes::kill_group(self.0, "KILL") {
            Ok(_) | Err(_) => {}
        }
    }
}

/// One finished run: its exit code, stdout as text and parsed lines, and
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

    fn first(&self, kind: &str) -> &Value {
        self.lines
            .iter()
            .find(|line| line["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} line"))
    }
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

/// Whether `extensions_loaded` lists the memory extension.
fn loads_memory(run: &Run) -> bool {
    run.first("extensions_loaded")["payload"]["extensions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["name"] == MEMORY)
}

#[test]
fn a_fresh_install_lists_memory_enabled_and_starts_no_vm() {
    let setup = Setup::new();
    setup.memory();

    let listed = setup.run(&["extension", "list"]);
    assert_eq!(listed.code, Some(0), "stderr: {}", listed.stderr);
    assert!(
        listed
            .stdout
            .lines()
            .any(|line| line == format!("{MEMORY} 0.0.0 local")),
        "list output: {}",
        listed.stdout
    );

    // The shipped package holds no code: no entry script, no TUI scripts,
    // and no process program in its manifest.
    let installed = setup.home().join("extensions").join(SLUG);
    assert!(!installed.join("init.lua").exists());
    assert!(!installed.join("tui").exists());
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(installed.join("extension.json")).unwrap())
            .unwrap();
    assert!(manifest.get("process").is_none());

    let (run, _) = setup.ask(vec![hello()]);
    // The plain HELLO sequence: no `extension_failed`, no review.
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(loads_memory(&run));
    // An empty store adds nothing to the opening message.
    assert!(
        run.first("opening_message")["payload"]
            .get("extension_sections")
            .is_none()
    );
}

#[test]
fn a_scripted_page_write_takes_the_fast_path() {
    let setup = Setup::new();
    setup.memory();
    let page = setup
        .home()
        .join("data")
        .join(SLUG)
        .join("page.md")
        .display()
        .to_string();
    let server = ProviderServer::start([
        stream(&[function_call(
            "write_page",
            "write",
            &json!({"path": page, "content": "first\n"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    let run = setup.run(&["ask", "keep notes"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let expected = vec![
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
        "assistant_message_delta",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ];
    assert_eq!(run.kinds(), expected);
    assert_eq!(
        run.first("tool_call_completed")["payload"]["status"],
        "completed"
    );
    assert_eq!(
        fs::read_to_string(setup.home().join("data").join(SLUG).join("page.md")).unwrap(),
        "first\n"
    );
    // The fast path writes no `permission_` line and asks no reviewer: the
    // provider server saw exactly the session's two replies.
    assert!(
        run.lines
            .iter()
            .all(|line| !line["kind"].as_str().unwrap().starts_with("permission_"))
    );
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn an_over_budget_store_ends_the_section_with_the_prune_line() {
    let setup = Setup::new();
    setup.memory();
    let machine = "m".repeat(15_000);
    let project = "p".repeat(15_000);
    let machine_path = setup.machine_file("index.md", &machine);
    let project_path = setup.project_file("index.md", &project);
    let (run, server) = setup.ask(vec![hello()]);
    assert_eq!(run.kinds(), HELLO_KINDS);

    // `opening_message` records both files, machine first, with the budget.
    assert_eq!(
        run.first("opening_message")["payload"]["extension_sections"],
        json!([{
            "extension": MEMORY,
            "files": [
                {"path": machine_path.display().to_string(), "content": machine},
                {"path": project_path.display().to_string(), "content": project},
            ],
            "budget_bytes": 25000,
        }])
    );
    let body = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert!(
        body.contains(
            "Fiber: these files are 30000 bytes, over their budget of 25000 bytes. Prune them."
        ),
        "{body}"
    );
}

#[test]
fn after_remove_memory_the_opening_message_has_no_memory_section() {
    let setup = Setup::new();
    setup.memory();
    setup.machine_file("index.md", "- [[notes]] — what matters\n");
    let project = setup.project_file("index.md", "- [[here]] — this project\n");
    let settings = setup.home().join("config/memory.json");
    write_text(&settings, "{}");

    // Without a terminal the remove goes ahead without asking.
    let removed = setup.run(&["extension", "remove", "memory"]);
    assert_eq!(removed.code, Some(0), "stderr: {}", removed.stderr);
    assert!(!setup.home().join("extensions").join(SLUG).exists());
    assert!(!setup.home().join("data").join(SLUG).exists());
    assert!(!project.parent().unwrap().exists());
    assert!(!settings.exists());

    let (run, server) = setup.ask(vec![hello()]);
    assert_eq!(run.kinds(), HELLO_KINDS);
    assert!(
        run.first("opening_message")["payload"]
            .get("extension_sections")
            .is_none()
    );
    let body = String::from_utf8(server.requests()[0].body.clone()).unwrap();
    assert!(!body.contains("extensions/memory"), "{body}");
}

#[test]
fn disabling_memory_by_its_short_name_stops_it_loading_and_keeps_its_store() {
    let setup = Setup::new();
    setup.memory();
    let page = setup.machine_file("index.md", "- [[notes]] — what matters\n");
    let server = ProviderServer::start(vec![hello()]).unwrap();
    setup.provider(&server);
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/m", "extensions": {"memory": {"enabled": false}}}),
    );
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(!loads_memory(&run));
    assert!(
        run.first("opening_message")["payload"]
            .get("extension_sections")
            .is_none()
    );
    assert!(page.is_file());
}
