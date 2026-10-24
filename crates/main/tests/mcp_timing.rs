//! Binary-level `mcp.connect` journeys (`docs/state.md`, "Diagnostic
//! logs"): the built `fiber` times one MCP connection attempt per actual
//! start through the installed timer. At debug an `ask` with a stdio or
//! HTTP server writes `mcp.connect` with only `op` and `ms`; at info it
//! writes no `op_timing` at all. Cached first use, resumed first use and
//! approved repository starts keep their context; skipped and required
//! failures behave. Every wait is the test's one deadline, and `ms` is
//! normalized, never asserted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{KillGroup, Run, Setup, function_call, hello, is_status, spawn_watched, stream};

impl Setup {
    /// Runs `fiber` with `args` in `dir` under `home` to exit under the
    /// test's deadline, asserting nothing it started is left behind.
    #[track_caller]
    fn run_as(&self, home: &Path, dir: &Path, args: &[&str]) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(dir)
            .env_clear()
            .envs(fakes::check_run())
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
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(
                self.deadline,
                group,
                &finished,
                &format!("`fiber {}` to exit", args.join(" ")),
            ),
        };
        assert!(!fakes::group_alive(group), "`fiber` left a process behind");
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        let stdout = String::from_utf8(output.stdout).unwrap();
        Run {
            code: output.status.code(),
            stderr: String::from_utf8(output.stderr).unwrap(),
            lines: stdout
                .lines()
                .filter(|l| !is_status(l))
                .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
                .collect(),
        }
    }

    /// Runs `fiber` with `args` in the workspace under this home.
    #[track_caller]
    fn run(&self, args: &[&str]) -> Run {
        let (home, workspace) = (self.home(), self.workspace());
        self.run_as(&home, &workspace, args)
    }
}

/// The fixture directory with `tools` and one result file per tool.
fn fixture(setup: &Setup, tools: &Value) -> PathBuf {
    let dir = setup.root.path().join("fx");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tools.json"), tools.to_string()).unwrap();
    fs::write(
        dir.join("call-echo.json"),
        r#"{"content":[{"type":"text","text":"hi"}]}"#,
    )
    .unwrap();
    dir
}

/// One read-only offline tool, so its calls take the permission fast path.
fn echo_tool() -> Value {
    json!({
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

/// Writes the home configuration with `servers` and the model, at debug
/// when asked: the shape `tests/mcp.rs` writes, with a sentinel env value
/// no timing line may ever carry.
fn configure(setup: &Setup, servers: Value, debug: bool) {
    let mut config = json!({"model": "fake/m", "mcp": {"servers": servers}});
    if debug {
        config["diagnostics"] = json!({"level": "debug"});
    }
    support::write_json(&setup.home().join("config.json"), &config);
}

fn stdio_server(dir: &Path, extra: Value) -> Value {
    let mut server = json!({
        "command": "/bin/bash",
        "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
        "env": {"MCP_TIMING_SENTINEL": "mcp-timing-sentinel"},
        "startup_timeout_ms": 600000,
    });
    if let (Some(object), Value::Object(fields)) = (server.as_object_mut(), extra) {
        for (key, value) in fields {
            object.insert(key, value);
        }
    }
    server
}

/// A provider that calls `tool` on the first request and says hello
/// after: answered from the conversation's state, so an offer or review
/// round trip never exhausts a script.
fn calling(tool: &str) -> ProviderServer {
    let tool = tool.to_owned();
    ProviderServer::start_responding(move |req: &fakes::Request| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let answered = body["input"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["type"] == "function_call_output")
        });
        if answered {
            hello()
        } else {
            stream(&[function_call("c1", &tool, &json!({}))])
        }
    })
    .unwrap()
}

/// A provider that calls `tool` again when the turn's latest item is not
/// already a call or its answer: a resumed turn carries the old answer in
/// its history, so only the last item tells whether this turn called.
fn calling_again(tool: &str) -> ProviderServer {
    let tool = tool.to_owned();
    ProviderServer::start_responding(move |req: &fakes::Request| {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let latest = body["input"].as_array().and_then(|items| items.last());
        let answered = latest.is_some_and(|item| {
            item["type"] == "function_call" || item["type"] == "function_call_output"
        });
        if answered {
            hello()
        } else {
            stream(&[function_call("c1", &tool, &json!({}))])
        }
    })
    .unwrap()
}

/// Every diagnostic line under the home's `logs/`, parsed: every file,
/// not only the post-attach one, since a required failure never attaches.
fn log_lines(home: &Path) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = home.join("logs");
    if !logs.exists() {
        return lines;
    }
    let mut files: Vec<PathBuf> = fs::read_dir(&logs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    for file in files {
        if !file.is_file() {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

/// Every `op_timing` line's `data`.
fn timings(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| line["code"] == "op_timing")
        .map(|line| &line["data"])
        .collect()
}

/// The `mcp.connect` lines' `data`.
fn connects(lines: &[Value]) -> Vec<&Value> {
    timings(lines)
        .into_iter()
        .filter(|data| data["op"] == "mcp.connect")
        .collect()
}

/// Every `op_timing` line carries only `op`, `ms` and `tool`, with `ms` a
/// whole number that is normalized away. A connection line carries only
/// `op` and `ms`: never a server name, a path, a URL or the env sentinel.
fn assert_timing_shape(lines: &[Value]) {
    let timings: Vec<&Value> = timings(lines);
    for data in &timings {
        let object = data.as_object().unwrap();
        for key in object.keys() {
            assert!(
                key == "op" || key == "ms" || key == "tool",
                "an op_timing field outside op, ms and tool: {data}"
            );
        }
        assert!(data["ms"].as_u64().is_some(), "ms is whole: {data}");
    }
    for data in connects(lines) {
        let flat = data.to_string();
        for sentinel in [
            "mcp-timing-sentinel",
            "mcp-fixture",
            "127.0.0.1",
            "fx",
            "remote",
        ] {
            assert!(
                !flat.contains(sentinel),
                "timing data carries {sentinel}: {data}"
            );
        }
    }
}

#[test]
fn debug_ask_times_stdio_connect() {
    let setup = Setup::new();
    let dir = fixture(&setup, &json!([echo_tool()]));
    let server = calling("mcp__fx__echo");
    setup.provider(&server);
    configure(&setup, json!({"fx": stdio_server(&dir, Value::Null)}), true);
    let run = setup.run(&["ask", "call the echo tool"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "the tool call lands and the turn ends"
    );
    let lines = log_lines(&setup.home());
    let connects = connects(&lines);
    assert!(!connects.is_empty(), "a connection is timed: {lines:?}");
    for data in &connects {
        assert_eq!(
            data.as_object().unwrap().len(),
            2,
            "a connection carries only op and ms: {data}"
        );
    }
    assert_timing_shape(&lines);
}

/// One fake remote MCP server: the handshake with one read-only `echo`
/// tool, answered from each request's id.
fn remote_mcp() -> ProviderServer {
    ProviderServer::start_responding(|req: &fakes::Request| {
        if req.method == "DELETE" {
            return fakes::Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let result =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => fakes::Response::status(
                200,
                result(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "1868a90c"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                fakes::Response::status(202, "")
            }
            Some("tools/list") => {
                fakes::Response::status(200, result(json!({"tools": [echo_tool()]})))
            }
            Some("tools/call") => fakes::Response::status(
                200,
                result(json!({"content": [{"type": "text", "text": "hi"}]})),
            ),
            Some(other) => panic!("unexpected method {other}"),
        }
    })
    .unwrap()
}

#[test]
fn debug_ask_times_http_connect() {
    let setup = Setup::new();
    let fake = remote_mcp();
    let url = format!("{}/mcp", fake.url());
    let provider = calling("mcp__remote__echo");
    setup.provider(&provider);
    configure(&setup, json!({"remote": {"url": url}}), true);
    let run = setup.run(&["ask", "call the echo tool"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        provider.await_requests(2, setup.deadline),
        "the tool call lands and the turn ends"
    );
    assert!(
        fake.await_requests(3, setup.deadline),
        "the remote server is dialed and listed"
    );
    let lines = log_lines(&setup.home());
    let connects = connects(&lines);
    assert_eq!(connects.len(), 1, "one connection is timed: {lines:?}");
    assert_timing_shape(&lines);
}

#[test]
fn info_ask_writes_no_op_timing() {
    let setup = Setup::new();
    let dir = fixture(&setup, &json!([echo_tool()]));
    let server = calling("mcp__fx__echo");
    setup.provider(&server);
    configure(
        &setup,
        json!({"fx": stdio_server(&dir, Value::Null)}),
        false,
    );
    let run = setup.run(&["ask", "call the echo tool"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(2, setup.deadline),
        "the tool call lands and the turn ends"
    );
    let lines = log_lines(&setup.home());
    assert!(
        timings(&lines).is_empty(),
        "at info nothing is timed: {lines:?}"
    );
}

/// Removes every diagnostic file, so the next run's lines stand alone.
fn wipe_logs(home: &Path) {
    let logs = home.join("logs");
    if logs.exists() {
        fs::remove_dir_all(&logs).unwrap();
    }
}

#[test]
fn cached_first_use_times_one() {
    let setup = Setup::new();
    let dir = fixture(&setup, &json!([echo_tool()]));
    let server = calling("mcp__fx__echo");
    setup.provider(&server);
    configure(
        &setup,
        json!({"fx": stdio_server(&dir, Value::Null)}),
        false,
    );
    // The info run populates the cache with no debug lines.
    let primed = setup.run(&["ask", "call the echo tool"]);
    assert_eq!(primed.code, Some(0), "stderr: {}", primed.stderr);
    // The debug run declares from the cache: only the first use starts.
    wipe_logs(&setup.home());
    let provider = calling("mcp__fx__echo");
    setup.provider(&provider);
    configure(&setup, json!({"fx": stdio_server(&dir, Value::Null)}), true);
    let run = setup.run(&["ask", "call the echo tool"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines = log_lines(&setup.home());
    let connects = connects(&lines);
    assert_eq!(connects.len(), 1, "the first use is timed: {lines:?}");
    assert_timing_shape(&lines);
}

/// The session id of an `ask` run's first line that names one.
fn session_id(run: &Run) -> String {
    run.lines
        .iter()
        .find_map(|line| {
            line.get("session_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| panic!("an ask names its session: {:?}", run.lines))
}

#[test]
fn resumed_first_use_times_one() {
    let setup = Setup::new();
    let dir = fixture(&setup, &json!([echo_tool()]));
    let server = calling("mcp__fx__echo");
    setup.provider(&server);
    configure(&setup, json!({"fx": stdio_server(&dir, Value::Null)}), true);
    let first = setup.run(&["ask", "call the echo tool"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = session_id(&first);
    // The resumed turn declares from the cache: only its first use
    // starts, through the resume path.
    wipe_logs(&setup.home());
    let provider = calling_again("mcp__fx__echo");
    setup.provider(&provider);
    configure(&setup, json!({"fx": stdio_server(&dir, Value::Null)}), true);
    let run = setup.run(&["ask", "--resume", &id, "call the echo tool again"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines = log_lines(&setup.home());
    let connects = connects(&lines);
    assert_eq!(
        connects.len(),
        1,
        "the resumed first use is timed: {lines:?}"
    );
    assert_timing_shape(&lines);
}

#[test]
fn approved_repository_start_times_connect_and_skipped_writes_none() {
    let setup = Setup::new();
    // The fixture server behind a pinned copy, as `mcp_commands` pins it.
    let dir = setup.root.path().join("fx");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("tools.json"), json!([echo_tool()]).to_string()).unwrap();
    fs::write(
        dir.join("call-echo.json"),
        r#"{"content":[{"type":"text","text":"hi"}]}"#,
    )
    .unwrap();
    let workspace = setup.workspace();
    let srv = workspace.join("srv");
    fs::create_dir_all(&srv).unwrap();
    fs::write(
        srv.join("run.sh"),
        format!(
            "#!/bin/bash\nexec /bin/bash \"{}\" \"$1\"\n",
            fakes::mcp_fixture().display(),
        ),
    )
    .unwrap();
    let add = setup.run(&[
        "mcp",
        "add",
        "--repo",
        "db",
        "--",
        "/bin/bash",
        "srv/run.sh",
        &dir.display().to_string(),
    ]);
    assert_eq!(add.code, Some(0), "{add:?}");
    // The approved declaration starts with the session: timed.
    let provider = calling("mcp__db__echo");
    setup.provider(&provider);
    configure_repo_debug(&setup);
    let run = setup.run(&["ask", "call the echo tool"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let lines = log_lines(&setup.home());
    assert_eq!(connects(&lines).len(), 1, "the approval starts timed");
    assert_timing_shape(&lines);
    // The same declaration in a fresh home has no approval: skipped
    // with no connection.
    let fresh = setup.root.path().join("h2");
    fs::create_dir_all(&fresh).unwrap();
    let second_source = setup.root.path().join("src2");
    install_provider(&fresh, &second_source, &provider);
    configure_repo_debug_home(&fresh);
    let ask = setup.run_as(&fresh, &workspace, &["ask", "call the echo tool"]);
    assert_eq!(ask.code, Some(0), "stderr: {}", ask.stderr);
    assert!(
        ask.lines.iter().any(|line| line["kind"] == "notice"
            && line["payload"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("db")),
        "the unapproved server is skipped: {ask:?}"
    );
    assert!(
        connects(&log_lines(&fresh)).is_empty(),
        "a skipped server connects never"
    );
}

/// Turns debug on in `home`, keeping the model the provider installed.
fn configure_repo_debug(setup: &Setup) {
    let home = setup.home();
    configure_repo_debug_home(&home);
}

fn configure_repo_debug_home(home: &Path) {
    let config: Value =
        serde_json::from_str(&fs::read_to_string(home.join("config.json")).unwrap()).unwrap();
    let mut config = config;
    config["diagnostics"] = json!({"level": "debug"});
    support::write_json(&home.join("config.json"), &config);
}

/// Installs the fake provider with model `fake/m` in `home`.
fn install_provider(home: &Path, root: &Path, server: &ProviderServer) {
    let source = root.join("src");
    support::write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
    );
    support::write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
        }),
    );
    extensions::plan(
        home,
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    support::write_json(&home.join("config.json"), &json!({"model": "fake/m"}));
}

#[test]
fn required_failure_writes_connect_before_attach() {
    let setup = Setup::new();
    let provider = ProviderServer::start([hello()]).unwrap();
    setup.provider(&provider);
    let dir = setup.root.path().join("fx");
    fs::create_dir_all(&dir).unwrap();
    configure(
        &setup,
        json!({"fx": stdio_server(&dir, json!({"required": true}))}),
        true,
    );
    // Nothing spawns: the command does not exist.
    let mut config: Value =
        serde_json::from_str(&fs::read_to_string(setup.home().join("config.json")).unwrap())
            .unwrap();
    config["mcp"]["servers"]["fx"]["command"] = json!("/nonexistent-fiber-server");
    config["mcp"]["servers"]["fx"]["args"] = json!([]);
    support::write_json(&setup.home().join("config.json"), &config);
    let run = setup.run(&["ask", "hi"]);
    assert_ne!(run.code, Some(0), "the required failure fails");
    // Every file is read: the session never attaches, so the line sits in
    // the pre-attach file with no session id.
    let lines = log_lines(&setup.home());
    let connects = connects(&lines);
    assert_eq!(connects.len(), 1, "the failed attempt is timed: {lines:?}");
    assert_timing_shape(&lines);
    for line in lines.iter().filter(|line| line["code"] == "op_timing") {
        assert!(
            line.get("session_id").is_none(),
            "nothing is attached yet: {line}"
        );
    }
}
