//! Binary-level tests of MCP stdio servers through `fiber ask`
//! (`docs/testing.md`, "Levels"): the built `fiber` runs in its own process
//! group with its own `FIBER_HOME`, holding an ordinary provider whose base
//! URL is the fake server, and a configured fixture server.

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

/// The built-in tool order, when no MCP server declares anything.
const TOOL_NAMES: [&str; 10] = [
    "ask_user",
    "delegate_spawn",
    "edit",
    "handoff",
    "jobs",
    "read",
    "session_search",
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
        let deadline = Deadline::start();
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
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
        );
        write(
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
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// Adds `servers` under `mcp.servers` to the global configuration,
    /// keeping the configured model.
    fn configure(&self, servers: &Value) {
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m", "mcp": {"servers": servers}}),
        );
    }

    /// Writes the fixture server's directory: `tools.json` and one result
    /// file per `(name, body)` pair. Returns the directory.
    fn fixture(&self, tools: &Value, calls: &[(&str, &str)]) -> PathBuf {
        let dir = self.root.path().join("fx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tools.json"), tools.to_string()).unwrap();
        for (name, body) in calls {
            fs::write(dir.join(name), body).unwrap();
        }
        dir
    }

    /// Runs `fiber` in its own process group, waits for it under the
    /// test's [`Deadline`], and asserts that nothing it started is left in the
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

/// One finished run: its exit code, stdout's lines, as text and parsed, and
/// stderr.
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
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
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

fn tool_names(body: &[u8]) -> Vec<String> {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// The event kinds of a turn whose first reply calls one reads-only tool
/// and whose second is [`hello`]: no `permission_` line is written, as for
/// a reads-only workspace call (`docs/permissions.md`, "Fast paths").
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

/// The event kinds of a turn whose only reply is [`hello`].
fn hello_kinds(extra: &[&'static str]) -> Vec<&'static str> {
    let mut kinds = vec!["session_started", "fiber_started", "extensions_loaded"];
    kinds.extend(extra.iter().copied());
    kinds.extend([
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
    ]);
    kinds
}

/// The echo tool: reads-only and offline, so its calls take the permission
/// fast path. Its schema names `zebra` before `apple`, so the request
/// proves the keys are sorted.
fn echo_tool() -> Value {
    json!({
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {
            "type": "object",
            "properties": {"zebra": {"type": "string"}, "apple": {"type": "string"}},
        },
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

/// Configures one fixture server `fx` in `dir` and returns nothing.
fn configure_fx(setup: &Setup, dir: &Path, extra: Value) {
    let mut server = json!({
        "command": "/bin/bash",
        "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
    });
    if let (Some(object), Value::Object(fields)) = (server.as_object_mut(), extra) {
        for (key, value) in fields {
            object.insert(key, value);
        }
    }
    setup.configure(&json!({"fx": server}));
}

#[test]
fn a_configured_server_declares_and_runs_its_tools() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([echo_tool()]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"hi"}]}"#,
        )],
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "echo hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    // The tools in one globally sorted order, with the server's name as
    // the MCP tool's registrar.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        tool_names(&requests[0].body),
        [
            "ask_user",
            "delegate_spawn",
            "edit",
            "handoff",
            "jobs",
            "mcp__fx__echo",
            "read",
            "session_search",
            "shell",
            "web_fetch",
            "write"
        ],
    );
    let preamble = run
        .lines
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
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
    assert!(sent.contains(&("mcp__fx__echo", "fx")));
    // Every schema's keys sorted: `apple` precedes `zebra` in the raw
    // request bytes.
    let raw = String::from_utf8_lossy(&requests[0].body);
    let (apple, zebra) = (
        raw.find("\"apple\"").unwrap(),
        raw.find("\"zebra\"").unwrap(),
    );
    assert!(apple < zebra, "schema keys are not sorted: {raw}");
    // The server saw the call with its arguments, and the result text
    // reaches the provider's next request.
    let log = fs::read_to_string(dir.join("requests.log")).unwrap();
    assert!(log.contains(r#""method":"tools/call""#), "log:\n{log}");
    assert!(log.contains(r#""name":"echo""#), "log:\n{log}");
    assert!(log.contains(r#""text":"hi""#), "log:\n{log}");
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], "hi");
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let output = second["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(output["output"], "hi");
}

#[test]
fn the_server_runs_in_the_workspace_and_stops_with_the_session() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&[]));
    let cwd = fs::read_to_string(dir.join("cwd.txt")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()),
        fs::canonicalize(setup.workspace()).unwrap().as_path(),
    );
    let pid: u32 = fs::read_to_string(dir.join("pid.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(pid > 1);
    // The server's pid is gone after `fiber` exits.
    assert!(
        fakes::pids_exit(&[pid], setup.deadline.left()),
        "waited until the deadline for pid {pid} to exit after `fiber`"
    );
}

#[test]
fn an_error_result_fails_the_call_with_tool_error() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([echo_tool()]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"no such thing"}],"isError":true}"#,
        )],
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "echo hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "tool_error");
    assert_eq!(completed["payload"]["content"][0]["text"], "no such thing");
}

#[test]
fn a_result_over_16_kib_is_cut_and_kept_in_an_artifact() {
    let setup = Setup::new();
    let big = "x".repeat(20_000);
    let dir = setup.fixture(
        &json!([echo_tool()]),
        &[(
            "call-echo.json",
            &json!({"content": [{"type": "text", "text": big}]}).to_string(),
        )],
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "echo hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    let artifact = completed["payload"]["artifact"].as_str().unwrap();
    let kept = fs::read(run.session_dir(&setup).join(artifact)).unwrap();
    assert_eq!(kept, big.as_bytes());
    let shown: String = completed["payload"]["content"][0]["text"]
        .as_str()
        .unwrap()
        .into();
    assert!(shown.len() < big.len(), "the result was not cut");
}

#[test]
fn a_call_that_times_out_fails_with_timeout() {
    let setup = Setup::new();
    // Reads-only and offline, so the call runs without review and the
    // test observes the timeout itself rather than a denial.
    let dir = setup.fixture(
        &json!([{"name": "hang", "annotations": {"readOnlyHint": true, "openWorldHint": false}}]),
        &[("call-hang.json", "hang")],
    );
    let server = ProviderServer::start([
        stream(&[function_call("call_hang", "mcp__fx__hang", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, json!({"timeout_ms": 200}));

    let run = setup.run(&["ask", "hang"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "timeout");
}

#[test]
fn disabled_hides_a_tool() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([echo_tool(), {"name": "hidden"}]),
        &[("call-echo.json", r#"{"content":[]}"#)],
    );
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, json!({"tools": {"disabled": ["hidden"]}}));

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&[]));
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        tool_names(&requests[0].body),
        [
            "ask_user",
            "delegate_spawn",
            "edit",
            "handoff",
            "jobs",
            "mcp__fx__echo",
            "read",
            "session_search",
            "shell",
            "web_fetch",
            "write"
        ]
    );
}

#[test]
fn a_persons_hints_override_changes_the_declared_effects() {
    let setup = Setup::new();
    // The server says destructive, which would send the call to review
    // with no `tool_call_started`; the person's override says reads-only
    // and offline, so the call runs and `tool_call_started` carries the
    // override's effects.
    let dir = setup.fixture(
        &json!([{"name": "echo", "annotations": {"destructiveHint": true}}]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"hi"}]}"#,
        )],
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(
        &setup,
        &dir,
        json!({"tools": {"echo": {"hints": {"readOnlyHint": true, "openWorldHint": false}}}}),
    );

    let run = setup.run(&["ask", "echo hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    let started = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started")
        .unwrap();
    // The override's effects, not the server's destructive ones.
    assert_eq!(started["payload"]["effects"], json!(["reads"]));
    assert_eq!(started["payload"]["reversible"], true);
}

#[test]
fn a_repositorys_hints_do_not_change_the_declared_effects() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([echo_tool()]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"hi"}]}"#,
        )],
    );
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);
    fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    fs::write(
        setup.workspace().join(".fiber/config.json"),
        json!({"mcp": {"servers": {"fx": {"tools": {"echo": {"hints": {"destructiveHint": true}}}}}}})
            .to_string(),
    )
    .unwrap();

    let run = setup.run(&["ask", "echo hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let started = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_started")
        .unwrap();
    // The server's own hints stand: reads, and no network.
    assert_eq!(started["payload"]["effects"], json!(["reads"]));
    assert_eq!(started["payload"]["reversible"], true);
}

#[test]
fn a_repositorys_server_is_not_started() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    fs::write(
        setup.workspace().join(".fiber/config.json"),
        json!({"mcp": {"servers": {"repo": {
            "command": "/bin/bash",
            "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
        }}}})
        .to_string(),
    )
    .unwrap();

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&["notice"]));
    let notice = run
        .lines
        .iter()
        .find(|line| line["kind"] == "notice")
        .unwrap();
    assert_eq!(notice["payload"]["code"], "repository_code_skipped");
    assert!(
        notice["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("`repo`")
    );
    let requests = server.requests();
    assert_eq!(tool_names(&requests[0].body), TOOL_NAMES);
}

#[test]
fn a_persons_server_with_repository_args_is_not_started() {
    let setup = Setup::new();
    setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.configure(&json!({"fx": {"command": "/bin/bash"}}));
    fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    fs::write(
        setup.workspace().join(".fiber/config.json"),
        json!({"mcp": {"servers": {"fx": {"args": ["evil.sh"]}}}}).to_string(),
    )
    .unwrap();

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&["notice"]));
    let notice = run
        .lines
        .iter()
        .find(|line| line["kind"] == "notice")
        .unwrap();
    assert_eq!(notice["payload"]["code"], "repository_code_skipped");
    assert!(
        notice["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("`fx`")
    );
}

#[test]
fn a_persons_empty_env_does_not_mask_a_repositorys_entry() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let marker = setup.root.path().join("pwned");
    let evil = setup.root.path().join("evil.sh");
    fs::write(&evil, format!("touch \"{}\"", marker.display())).unwrap();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.configure(&json!({"fx": {
        "command": "/bin/bash",
        "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
        "env": {},
    }}));
    fs::create_dir_all(setup.workspace().join(".fiber")).unwrap();
    fs::write(
        setup.workspace().join(".fiber/config.json"),
        json!({"mcp": {"servers": {"fx": {"env": {"BASH_ENV": evil.display().to_string()}}}}})
            .to_string(),
    )
    .unwrap();

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&["notice"]));
    let notice = run
        .lines
        .iter()
        .find(|line| line["kind"] == "notice")
        .unwrap();
    assert_eq!(notice["payload"]["code"], "repository_code_skipped");
    assert!(
        notice["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("`fx`")
    );
    let requests = server.requests();
    assert_eq!(tool_names(&requests[0].body), TOOL_NAMES);
    assert!(
        !marker.exists(),
        "the repository's BASH_ENV ran despite the skip"
    );
}

#[test]
fn a_server_that_fails_to_start_leaves_the_session_running() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.configure(&json!({"bad": {"command": "/no/such/command"}}));

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&["mcp_server_failed"]));
    let failed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "mcp_server_failed")
        .unwrap();
    assert_eq!(failed["payload"]["server"], "bad");
    assert_eq!(failed["payload"]["reason"], "start_failed");
    assert_eq!(failed["payload"]["will_restart"], false);
    assert_eq!(failed["payload"]["error"]["code"], "mcp_server_unavailable");
}

#[test]
fn a_second_session_declares_cached_tools_without_starting() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([echo_tool()]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"hi"}]}"#,
        )],
    );
    let server = ProviderServer::start([
        hello(),
        hello(),
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    // The first session misses the cache and starts the server.
    let first = setup.run(&["ask", "hi"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert!(
        dir.join("pid.txt").exists(),
        "the first session starts the server"
    );
    fs::remove_file(dir.join("pid.txt")).unwrap();

    // The second session declares the tools from the cache: the first
    // model request already lists them, and nothing spawns.
    let second = setup.run(&["ask", "hi"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.kinds(), hello_kinds(&[]));
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        tool_names(&requests[1].body).contains(&"mcp__fx__echo".to_owned()),
        "the cached tools are declared on the first request"
    );
    assert!(
        !dir.join("pid.txt").exists(),
        "a session that never calls starts nothing"
    );

    // The third session calls the tool: the first call starts the server
    // and answers with the fixture's result.
    let third = setup.run(&["ask", "echo hi"]);
    assert_eq!(third.code, Some(0), "stderr: {}", third.stderr);
    assert!(
        dir.join("pid.txt").exists(),
        "the first call starts the server"
    );
    let completed = third
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], "hi");
}

#[test]
fn a_required_server_that_fails_to_start_exits_before_the_session() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start(Vec::<Response>::new()).unwrap();
    setup.provider(&server);
    // A healthy server beside the failing required one: the failure stops
    // what started before failing the session.
    setup.configure(&json!({
        "fx": {
            "command": "/bin/bash",
            "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
        },
        "bad": {"command": "/no/such/command", "required": true},
    }));

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(
        server.requests().is_empty(),
        "the session never starts, so the model is never called"
    );
    let line = run.lines.first().unwrap();
    assert_eq!(
        line["payload"]["error"]["code"],
        "mcp_required_server_failed"
    );
    assert!(
        line["payload"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("The required MCP server `bad` failed to start"),
        "message: {}",
        line["payload"]["error"]["message"]
    );
    assert!(
        run.stderr
            .contains("The required MCP server `bad` failed to start"),
        "stderr: {}",
        run.stderr
    );
    let pid: u32 = fs::read_to_string(dir.join("pid.txt"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The healthy server started, then stopped with the failed session.
    assert!(
        fakes::pids_exit(&[pid], setup.deadline.left()),
        "waited until the deadline for pid {pid} to exit after the required failure"
    );
}

#[test]
fn a_cached_server_that_fails_on_the_call_fails_the_call() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([echo_tool()]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"hi"}]}"#,
        )],
    );
    let server = ProviderServer::start([
        hello(),
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    // The first session populates the cache.
    let first = setup.run(&["ask", "hi"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    // The server's tool list breaks under the same declaration, so the
    // next session declares from the cache and fails on the call.
    fs::write(dir.join("tools.json"), "null").unwrap();

    let run = setup.run(&["ask", "echo hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        tool_names(&requests[1].body).contains(&"mcp__fx__echo".to_owned()),
        "the cached tools are declared on the first request"
    );
    // `read_kinds` with the failed start's record before the completion.
    let mut kinds = read_kinds();
    kinds.insert(
        kinds
            .iter()
            .position(|kind| *kind == "tool_call_completed")
            .unwrap(),
        "mcp_server_failed",
    );
    assert_eq!(run.kinds(), kinds);
    let failed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "mcp_server_failed")
        .unwrap();
    assert_eq!(failed["payload"]["server"], "fx");
    assert_eq!(failed["payload"]["reason"], "start_failed");
    // A failed start on the call is the server's one death: the next call
    // would restart it.
    assert_eq!(failed["payload"]["will_restart"], true);
    assert_eq!(failed["payload"]["error"]["code"], "mcp_server_unavailable");
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(
        completed["payload"]["error"]["code"],
        "mcp_server_unavailable"
    );
}

#[test]
fn a_call_to_a_tool_the_server_removed_fails_with_mcp_tool_removed() {
    let setup = Setup::new();
    let gone =
        json!({"name": "gone", "annotations": {"readOnlyHint": true, "openWorldHint": false}});
    let dir = setup.fixture(
        &json!([echo_tool(), gone]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"hi"}]}"#,
        )],
    );
    let server = ProviderServer::start([
        hello(),
        stream(&[function_call("call_gone", "mcp__fx__gone", &json!({}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    // The first session caches both tools.
    let first = setup.run(&["ask", "hi"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    // The server removes the tool under the same declaration.
    fs::write(dir.join("tools.json"), json!([echo_tool()]).to_string()).unwrap();

    let run = setup.run(&["ask", "call gone"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), read_kinds());
    assert!(
        run.lines
            .iter()
            .all(|line| line["kind"] != "mcp_server_failed"),
        "a removed tool is no failed start"
    );
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "mcp_tool_removed");
    // The cache now holds the live list, for the next session.
    let cache: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("cache/mcp/fx.json")).unwrap())
            .unwrap();
    let names: Vec<&str> = cache["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["echo"]);
}

/// A reads-only, offline fixture tool named `name`: its calls take the
/// permission fast path.
fn quiet_tool(name: &str) -> Value {
    json!({"name": name, "annotations": {"readOnlyHint": true, "openWorldHint": false}})
}

/// One reply per call, in order, each calling `mcp__fx__<name>`, then
/// [`hello`].
fn replies(names: &[&str]) -> Vec<Response> {
    let mut replies: Vec<Response> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            stream(&[function_call(
                &format!("call_{index}"),
                &format!("mcp__fx__{name}"),
                &json!({}),
            )])
        })
        .collect();
    replies.push(hello());
    replies
}

impl Run {
    /// Each call's lines, in call order: the lines under the action of
    /// each `tool_call_completed` after its start, ending with that
    /// completion.
    fn calls(&self) -> Vec<Vec<&Value>> {
        self.lines
            .iter()
            .filter(|line| line["kind"] == "tool_call_completed")
            .map(|completed| {
                self.lines
                    .iter()
                    .filter(|line| {
                        line["action_id"] == completed["action_id"]
                            && line["kind"] != "tool_call_requested"
                            && line["kind"] != "tool_call_started"
                    })
                    .collect()
            })
            .collect()
    }
}

/// One call's lines as words: each server line's kind, with its reason and
/// `will_restart` for a failure, then the completion's status and code.
fn call_words(lines: &[&Value]) -> Vec<String> {
    lines
        .iter()
        .map(|line| match line["kind"].as_str().unwrap() {
            "mcp_server_failed" => {
                assert_eq!(line["payload"]["server"], "fx");
                assert_eq!(line["payload"]["error"]["code"], "mcp_server_unavailable");
                format!(
                    "failed {} {}",
                    line["payload"]["reason"].as_str().unwrap(),
                    line["payload"]["will_restart"]
                )
            }
            "mcp_server_ready" => {
                assert_eq!(line["payload"]["server"], "fx");
                "ready".to_owned()
            }
            "tool_call_completed" => match line["payload"]["error"]["code"].as_str() {
                Some(code) => format!("{} {code}", line["payload"]["status"].as_str().unwrap()),
                None => line["payload"]["status"].as_str().unwrap().to_owned(),
            },
            other => other.to_owned(),
        })
        .collect()
}

/// The event kinds of a turn whose replies call one reads-only tool each,
/// then [`hello`]: each call's server lines, from `servers`, fall between
/// its `tool_call_started` and its `tool_call_completed`.
fn call_kinds(servers: &[&[&'static str]]) -> Vec<&'static str> {
    let read = read_kinds();
    let start = read
        .iter()
        .position(|kind| *kind == "step_started")
        .unwrap();
    let completed = read
        .iter()
        .position(|kind| *kind == "tool_call_completed")
        .unwrap();
    let mut kinds = read[..start].to_vec();
    for lines in servers {
        kinds.extend_from_slice(&read[start..completed]);
        kinds.extend_from_slice(lines);
        kinds.push("tool_call_completed");
    }
    kinds.extend_from_slice(&read[completed + 1..]);
    kinds
}

fn words(run: &Run) -> Vec<Vec<String>> {
    run.calls().iter().map(|lines| call_words(lines)).collect()
}

fn die_and_echo(setup: &Setup) -> PathBuf {
    setup.fixture(
        &json!([quiet_tool("die"), quiet_tool("echo")]),
        &[
            ("call-die.json", "exit"),
            (
                "call-echo.json",
                r#"{"content":[{"type":"text","text":"hi"}]}"#,
            ),
        ],
    )
}

#[test]
fn a_server_that_dies_mid_session_restarts_on_the_next_call() {
    let setup = Setup::new();
    let dir = die_and_echo(&setup);
    let server = ProviderServer::start(replies(&["die", "echo"])).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "die then echo"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        call_kinds(&[&["mcp_server_failed"], &["mcp_server_ready"]]),
    );
    assert_eq!(
        words(&run),
        [
            vec!["failed died true", "failed mcp_server_unavailable"],
            vec!["ready", "completed"],
        ],
    );
    let failed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "mcp_server_failed")
        .unwrap();
    assert_eq!(
        failed["payload"]["error"]["message"],
        "The MCP server `fx` exited; Fiber restarts it on the next call."
    );
}

#[test]
fn a_server_that_dies_twice_stays_dead_with_its_tools_declared() {
    let setup = Setup::new();
    let dir = die_and_echo(&setup);
    let server = ProviderServer::start(replies(&["die", "echo", "die", "echo"])).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "die twice"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        call_kinds(&[
            &["mcp_server_failed"],
            &["mcp_server_ready"],
            &["mcp_server_failed"],
            &[],
        ]),
    );
    assert_eq!(
        words(&run),
        [
            vec!["failed died true", "failed mcp_server_unavailable"],
            vec!["ready", "completed"],
            vec!["failed died false", "failed mcp_server_unavailable"],
            vec!["failed mcp_server_unavailable"],
        ],
    );
    // The dead server's tools stay declared on every request, so the
    // prompt cache holds.
    let requests = server.requests();
    assert_eq!(requests.len(), 5);
    for request in &requests {
        let names = tool_names(&request.body);
        assert!(names.contains(&"mcp__fx__die".to_owned()), "{names:?}");
        assert!(names.contains(&"mcp__fx__echo".to_owned()), "{names:?}");
    }
}

#[test]
fn a_list_changed_notice_changes_nothing_until_the_next_session() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([quiet_tool("echo"), quiet_tool("notify")]),
        &[(
            "call-notify.json",
            r#"{"content":[{"type":"text","text":"noted"}]}"#,
        )],
    );
    let mut script = vec![hello()];
    script.extend(replies(&["notify"]));
    let server = ProviderServer::start(script).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    // The first session caches `echo` and `notify`.
    let first = setup.run(&["ask", "hi"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(first.kinds(), hello_kinds(&[]));
    // The server gains a tool, and says so during the next call.
    fs::write(
        dir.join("tools.json"),
        json!([
            quiet_tool("echo"),
            quiet_tool("notify"),
            quiet_tool("extra")
        ])
        .to_string(),
    )
    .unwrap();
    fs::write(dir.join("notify-notify"), "").unwrap();
    fs::remove_file(dir.join("requests.log")).unwrap();

    let run = setup.run(&["ask", "notify"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    // The call completed, so the notice sent before its answer was read.
    assert_eq!(run.kinds(), read_kinds());
    assert_eq!(words(&run), [vec!["completed"]]);
    let log = fs::read_to_string(dir.join("requests.log")).unwrap();
    assert_eq!(
        log.matches(r#""method":"tools/list""#).count(),
        1,
        "the notice fetched no list: {log}",
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let declared = tool_names(&requests[1].body);
    assert!(declared.contains(&"mcp__fx__notify".to_owned()));
    assert!(!declared.contains(&"mcp__fx__extra".to_owned()));
    assert_eq!(tool_names(&requests[2].body), declared);
}

/// The fixture with an `echo` tool and a `greet` prompt taking `who`
/// (required) and `tone`: `prompt` files hold one `prompts/get` result
/// each, written as extra `(name, body)` pairs.
fn prompt_fixture(extra: &[(&str, &str)]) -> (Setup, PathBuf) {
    let setup = Setup::new();
    let mut calls = vec![
        (
            "prompts.json",
            r#"[{"name":"greet","description":"Greets someone.","arguments":[{"name":"who","required":true},{"name":"tone"}]}]"#,
        ),
        (
            "prompt-greet.json",
            r#"{"messages":[{"role":"user","content":{"type":"text","text":"Say hello to Ada, warmly."}}]}"#,
        ),
    ];
    calls.extend(extra.iter().copied());
    let dir = setup.fixture(&json!([{"name": "echo"}]), &calls);
    (setup, dir)
}

#[test]
fn fiber_ask_runs_a_servers_prompt_with_its_arguments() {
    let (setup, dir) = prompt_fixture(&[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "/greet Ada warm"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&[]));
    let started = run
        .lines
        .iter()
        .find(|line| line["kind"] == "turn_started")
        .expect("the prompt started a turn");
    assert_eq!(
        started["payload"]["input"][0]["content"][0]["text"],
        "Say hello to Ada, warmly."
    );
    let log = fs::read_to_string(dir.join("requests.log")).unwrap();
    assert!(log.contains(r#""method":"prompts/get""#), "log:\n{log}");
    assert!(
        log.contains(r#""arguments":{"tone":"warm","who":"Ada"}"#),
        "the get sends the named arguments: {log}",
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        String::from_utf8_lossy(&requests[0].body).contains("Say hello to Ada, warmly."),
        "the provider's request holds the prompt's text",
    );
}

#[test]
fn a_cached_servers_prompt_starts_it_when_run() {
    let (setup, dir) = prompt_fixture(&[]);
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let first = setup.run(&["ask", "/greet Ada warm"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    assert_eq!(first.kinds(), hello_kinds(&[]));
    fs::remove_file(dir.join("pid.txt")).expect("pid.txt");
    let run = setup.run(&["ask", "/greet Ada warm"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&[]));
    assert!(
        dir.join("pid.txt").exists(),
        "running the prompt starts the cached server",
    );
    let log = fs::read_to_string(dir.join("requests.log")).unwrap();
    assert_eq!(log.matches(r#""method":"initialize""#).count(), 2);
    assert_eq!(log.matches(r#""method":"prompts/get""#).count(), 2);
    let second_init = log.rfind(r#""method":"initialize""#).unwrap();
    let second_get = log.rfind(r#""method":"prompts/get""#).unwrap();
    assert!(
        second_init < second_get,
        "the second session starts before its get: {log}",
    );
    let cache = fs::read_to_string(setup.home().join("cache/mcp/fx.json")).unwrap();
    assert!(
        cache.contains(r#""prompts":[{"#) && cache.contains(r#""name":"greet""#),
        "the cache holds the prompt list: {cache}",
    );
}

#[test]
fn fiber_ask_with_a_missing_required_argument_exits_1_without_calling_the_model() {
    let (setup, dir) = prompt_fixture(&[]);
    let server = ProviderServer::start(Vec::<Response>::new()).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "/greet"]);

    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ],
    );
    let exited = run.lines.last().expect("fiber_exited is last");
    assert_eq!(exited["payload"]["error"]["code"], "invalid_arguments");
    assert!(
        run.stderr.contains("needs <who>"),
        "stderr names what the prompt needs: {}",
        run.stderr,
    );
    assert!(server.requests().is_empty(), "the model is never called",);
    let log = fs::read_to_string(dir.join("requests.log")).unwrap();
    assert!(
        !log.contains("prompts/get"),
        "the server is never asked: {log}",
    );
}

#[test]
fn fiber_ask_whose_prompt_the_server_refuses_exits_1() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([{"name": "echo"}]),
        &[(
            "prompts.json",
            r#"[{"name":"greet","description":"Greets someone.","arguments":[{"name":"who","required":true}]}]"#,
        )],
    );
    // No `prompt-greet.json`: the fixture refuses with -32602.
    let server = ProviderServer::start(Vec::<Response>::new()).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "/greet Ada"]);

    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        [
            "session_started",
            "fiber_started",
            "extensions_loaded",
            "fiber_exited",
        ],
    );
    let exited = run.lines.last().expect("fiber_exited is last");
    assert_eq!(exited["kind"], "fiber_exited");
    assert_eq!(exited["payload"]["error"]["code"], "mcp_prompt_failed");
    assert!(
        run.stderr.contains("refused the prompt `/greet`"),
        "stderr names the refusal: {}",
        run.stderr,
    );
    assert!(server.requests().is_empty(), "the model is never called",);
}

#[test]
fn a_slash_name_neither_a_skill_nor_a_prompt_is_sent_as_written_with_a_server_configured() {
    let (setup, dir) = prompt_fixture(&[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, Value::Null);

    let run = setup.run(&["ask", "/nope x"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&[]));
    let started = run
        .lines
        .iter()
        .find(|line| line["kind"] == "turn_started")
        .expect("the prompt started a turn");
    assert_eq!(
        started["payload"]["input"][0]["content"][0]["text"],
        "/nope x"
    );
}
