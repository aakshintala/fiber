//! Binary-level tests of a Lua extension's `after_tool` hook through
//! `fiber ask` (`docs/extensions.md`, "Hooks"; `docs/testing.md`, "Levels"):
//! the built `fiber` runs in its own process group with its own
//! `FIBER_HOME`, holding an ordinary provider whose base URL is the fake
//! server, and the extensions under test. No test here waits on a hook's
//! timeout; those run on the fake clock in `extensions`.

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

/// How long one `fiber` run may take.
const DEADLINE: Duration = Duration::from_secs(20);

/// The fixture extension's name.
const FIXTURE: &str = "fiber.test/lua-fixture";

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fh");
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

    /// Installs the extension in `source`.
    fn install(&self, source: PathBuf) {
        extensions::plan(
            &self.home(),
            &extensions::Request::Path(source),
            "0.1.0",
            &extensions::Origin::github(),
            &*fakes::clock::FakeClock::new(),
        )
        .unwrap()
        .commit()
        .unwrap();
    }

    /// Installs `fiber.test/<short>`, whose entry script is `init`.
    fn lua(&self, short: &str, init: &str) {
        let source = self.root.path().join("src").join(short);
        write(
            &source.join("extension.json"),
            &json!({"name": format!("fiber.test/{short}"), "version": "v2.0.0", "fiber": "0.1.0", "api": 1})
                .to_string(),
        );
        write(&source.join("init.lua"), init);
        self.install(source);
    }

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and writes configuration naming `fake/m` with `extra`.
    fn provider(&self, server: &ProviderServer, extra: &Value) {
        let source = self.root.path().join("src").join("fake");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}).to_string(),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url())}]
            })
            .to_string(),
        );
        self.install(source);
        let mut config = json!({"model": "fake/m"});
        for (key, value) in extra.as_object().into_iter().flatten() {
            config[key] = value.clone();
        }
        write(&self.home().join("config.json"), &config.to_string());
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

    /// Writes `text` to `note.txt` in the workspace, starts a server whose
    /// model reads it and then answers, and runs `fiber ask`.
    fn read_note(&self, text: &str, config: &Value) -> (Run, ProviderServer) {
        fs::write(self.workspace().join("note.txt"), text).unwrap();
        let server = ProviderServer::start([
            stream(&[function_call(
                "call_read",
                "read",
                &json!({"path": "note.txt"}),
            )]),
            hello(),
        ])
        .unwrap();
        self.provider(&server, config);
        let run = self.run(&["ask", "read the note"]);
        assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
        (run, server)
    }
}

fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

// debt: the runner, `spawn_watched` and `KillGroup` copy `tools.rs`'s, as
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

/// One finished run: its exit code, stdout's lines parsed, and stderr.
struct Run {
    code: Option<i32>,
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
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
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

    fn all(&self, kind: &str) -> Vec<&Value> {
        self.lines
            .iter()
            .filter(|line| line["kind"] == kind)
            .collect()
    }

    fn completed(&self) -> &Value {
        &self.first("tool_call_completed")["payload"]
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
            .join(self.lines[0]["session_id"].as_str().unwrap())
    }
}

/// Every file's bytes under `dir`, as text, joined.
fn on_disk(dir: &Path) -> String {
    let mut all = String::new();
    let mut dirs = vec![dir.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.is_file() {
                all.push_str(&String::from_utf8_lossy(&fs::read(&path).unwrap()));
            }
        }
    }
    all
}

/// The tool output the second request carries.
fn output_sent(server: &ProviderServer) -> String {
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap()["output"]
        .as_str()
        .unwrap()
        .to_owned()
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

/// An `openai-responses` stream answering `Hello.`.
fn hello() -> Response {
    stream(&[
        json!({"type": "response.output_text.delta", "delta": "Hello."}),
        json!({"type": "response.output_item.done", "item": {
            "type": "message", "content": [{"type": "output_text", "text": "Hello."}]
        }}),
    ])
}

/// The complete event kinds of a run whose model reads the note, then
/// answers: `loading` follows `extensions_loaded`, the notices loading
/// raised, and `hooked` precedes the read's `tool_call_completed`, the
/// notices its hooks raised.
fn read_kinds(loading: &[&'static str], hooked: &[&'static str]) -> Vec<&'static str> {
    [
        &["session_started", "fiber_started", "extensions_loaded"][..],
        loading,
        &[
            "preamble_built",
            "opening_message",
            "turn_started",
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
        ],
        hooked,
        &[
            "tool_call_completed",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
            "fiber_exited",
        ],
    ]
    .concat()
}

/// An `after_tool` hook of `phase` that appends `|<tag>`.
fn tagging(tag: &str, phase: &str) -> String {
    format!(
        "fiber.hook(\"after_tool\", {{ phase = \"{phase}\", on_failure = \"blocking\", timeout = 1000,\n\
           run = function(call) return {{ content = call.content .. \"|{tag}\" }} end }})\n"
    )
}

#[test]
fn an_installed_extension_is_loaded_before_the_first_request_and_its_hook_redacts_the_result() {
    let setup = Setup::new();
    setup.install(fakes::lua_fixture());
    let (run, server) = setup.read_note("the password is hunter2\n", &json!({}));

    // `extensions_loaded` follows `fiber_started`, before the first step.
    assert_eq!(run.kinds(), read_kinds(&[], &[]));
    assert_eq!(
        run.first("extensions_loaded")["payload"]["extensions"],
        json!([
            {"name": "fake", "version": "v0.0.0"},
            {"name": FIXTURE, "version": "v1.0.0"},
        ])
    );

    let completed = run.completed();
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": "the password is [redacted]\n"}])
    );
    assert_eq!(completed["changed_by"], json!([FIXTURE]));
    assert_eq!(output_sent(&server), "the password is [redacted]\n");
    let dir = run.session_dir(&setup);
    assert!(!on_disk(&dir).contains("hunter2"));
    assert!(on_disk(&dir).contains("[redacted]"));
}

#[test]
fn a_result_no_hook_changes_names_nobody() {
    let setup = Setup::new();
    setup.install(fakes::lua_fixture());
    let (run, server) = setup.read_note("nothing secret\n", &json!({}));
    assert_eq!(run.kinds(), read_kinds(&[], &[]));
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "nothing secret\n");
    assert!(completed.get("changed_by").is_none());
    assert_eq!(output_sent(&server), "nothing secret\n");
}

#[test]
fn the_artifact_holds_the_hooks_artifact_text() {
    let setup = Setup::new();
    setup.lua(
        "art",
        "fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"blocking\",\n\
           run = function(call) return { content = \"summary\", artifact = \"the whole log\" } end })\n",
    );
    let (run, server) = setup.read_note("raw build output\n", &json!({}));
    assert_eq!(run.kinds(), read_kinds(&[], &[]));
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "summary");
    assert_eq!(completed["changed_by"], json!(["fiber.test/art"]));
    let dir = run.session_dir(&setup);
    let artifact = completed["artifact"].as_str().unwrap();
    assert_eq!(
        fs::read_to_string(dir.join(artifact)).unwrap(),
        "the whole log"
    );
    assert_eq!(output_sent(&server), "summary");
    assert!(!on_disk(&dir).contains("raw build output"));
}

#[test]
fn hooks_that_do_not_register_or_fail_softly_leave_the_output_with_a_notice() {
    let setup = Setup::new();
    setup.lua(
        "bad",
        "local run = function(call) return { content = \"BAD\" } end\n\
         fiber.hook(\"after_tool\", { on_failure = \"blocking\", run = run })\n\
         fiber.hook(\"after_tool\", { timeout = 1000, run = run })\n",
    );
    setup.lua(
        "soft",
        "fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"non-blocking\",\n\
           run = function(call) error(\"soft failure\") end })\n",
    );
    let (run, server) = setup.read_note("original\n", &json!({}));
    assert_eq!(run.kinds(), read_kinds(&["notice", "notice"], &["notice"]));
    let notices = run.all("notice");
    let registered: Vec<(&str, &str, &str)> = notices
        .iter()
        .map(|n| {
            (
                n["payload"]["code"].as_str().unwrap(),
                n["payload"]["extension"].as_str().unwrap_or_default(),
                n["payload"]["message"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(
        registered.contains(&(
            "extension_failed",
            "fiber.test/bad",
            "`after_tool` hook not registered: missing `timeout`"
        )),
        "{registered:?}"
    );
    assert!(
        registered.contains(&(
            "extension_failed",
            "fiber.test/bad",
            "`after_tool` hook not registered: missing `on_failure`"
        )),
        "{registered:?}"
    );
    let soft = registered
        .iter()
        .find(|(code, _, _)| *code == "hook_failed")
        .expect("a hook_failed notice");
    assert_eq!(soft.1, "fiber.test/soft");
    assert!(soft.2.contains("soft failure"), "{}", soft.2);
    // Neither extension changed the result.
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "original\n");
    assert!(completed.get("changed_by").is_none());
    assert_eq!(output_sent(&server), "original\n");
}

#[test]
fn a_blocking_hook_that_fails_withholds_the_output_and_keeps_the_status() {
    let setup = Setup::new();
    setup.lua(
        "hard",
        "fiber.hook(\"after_tool\", { timeout = 1000, on_failure = \"blocking\",\n\
           run = function(call) error(\"hard failure\") end })\n",
    );
    let (run, server) = setup.read_note("secret text\n", &json!({}));
    assert_eq!(run.kinds(), read_kinds(&[], &[]));
    let withheld = "Output withheld: the `after_tool` hook of extension fiber.test/hard failed.";
    let completed = run.completed();
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": withheld}])
    );
    assert!(completed.get("artifact").is_none());
    assert_eq!(completed["changed_by"], json!(["fiber.test/hard"]));
    assert_eq!(output_sent(&server), withheld);
    let dir = run.session_dir(&setup);
    assert!(!on_disk(&dir).contains("secret text"));
    assert!(
        !dir.join("artifacts").exists()
            || fs::read_dir(dir.join("artifacts"))
                .unwrap()
                .next()
                .is_none()
    );
}

#[test]
fn hooks_run_by_phase_then_hooks_order_then_name() {
    let setup = Setup::new();
    setup.lua("a", &tagging("a", "transform"));
    setup.lua(
        "b",
        &(tagging("b-t", "transform") + &tagging("b-s", "sanitize")),
    );
    setup.lua("c", &tagging("c", "transform"));
    let (run, server) = setup.read_note(
        "x",
        &json!({"hooks": {"order": {"after_tool": ["fiber.test/c"]}}}),
    );
    assert_eq!(run.kinds(), read_kinds(&[], &[]));
    let completed = run.completed();
    assert_eq!(completed["content"][0]["text"], "x|b-s|c|a|b-t");
    assert_eq!(
        completed["changed_by"],
        json!(["fiber.test/b", "fiber.test/c", "fiber.test/a"])
    );
    assert_eq!(output_sent(&server), "x|b-s|c|a|b-t");
}
