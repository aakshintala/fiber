//! Binary-level tests of remote MCP servers over streamable HTTP through
//! `fiber ask` (`docs/testing.md`, "Levels"): the built `fiber` runs in
//! its own process group with its own `FIBER_HOME`, holding an ordinary
//! provider whose base URL is one fake server, and a configured `url`
//! server on another, answering by method. The default 5 s startup
//! deadline runs on the real process clock, which is the binary-test rule
//! (`docs/testing.md`, "Waits and timeouts").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Request, Response, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, KillGroup, Run, function_call, hello, is_status, read_kinds, spawn_watched, stream,
};

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

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
    /// Kills every process whose command line names the test's root,
    /// and their groups, when the setup drops.
    #[allow(dead_code, reason = "held only for its drop")]
    guard: Watchdog,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fa");
        let guard = Watchdog::matching(&root.path().display().to_string());
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self {
            deadline,
            root,
            guard,
        }
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
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
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

    /// Installs providers `fake` with models `m` and `r`, makes `fake/m`
    /// the configured model and `fake/r` its reviewer, so reviewed calls
    /// (every remote call declares `network`) are allowed.
    fn provider_with_reviewer(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [
                    {"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000},
                    {"id": "r", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000},
                ]
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
            &json!({"model": "fake/m", "reviewer": {"model": "fake/r"}}),
        );
    }

    /// Adds `servers` under `mcp.servers` to the global configuration,
    /// keeping the configured model and reviewer.
    fn configure(&self, servers: &Value) {
        let path = self.home().join("config.json");
        let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        config["mcp"] = json!({"servers": servers});
        write(&path, &config);
    }

    /// Runs `fiber` in its own process group, waits for it under the
    /// test's [`Deadline`], and asserts that nothing it started is left in the
    /// group, after a timeout too (`docs/testing.md`, "Running tests").
    #[track_caller]
    fn run(&self, args: &[&str]) -> Run {
        let home = self.home();
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
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
        assert!(
            fakes::group_empties(group, self.deadline),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Run::new(output)
    }
}

fn write(file: &std::path::Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

impl Run {
    fn new(output: Output) -> Self {
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

/// Like [`call_kinds`], for reviewed calls: the reviewer's usage and its
/// `permission_resolved` fall between the turn's own lines and each
/// `tool_call_started`.
fn reviewed_kinds(servers: &[&[&'static str]]) -> Vec<&'static str> {
    let read = read_kinds();
    let start = read
        .iter()
        .position(|kind| *kind == "step_started")
        .unwrap();
    let started = read
        .iter()
        .position(|kind| *kind == "tool_call_started")
        .unwrap();
    let completed = read
        .iter()
        .position(|kind| *kind == "tool_call_completed")
        .unwrap();
    let mut kinds = read[..start].to_vec();
    for lines in servers {
        kinds.extend_from_slice(&read[start..started]);
        kinds.extend(["usage_recorded", "permission_resolved"]);
        kinds.extend_from_slice(&read[started..completed]);
        kinds.extend_from_slice(lines);
        kinds.push("tool_call_completed");
    }
    kinds.extend_from_slice(&read[completed + 1..]);
    kinds
}

/// A fake remote MCP server answering the handshake, `tools` from
/// `tools/list`, and every other call through `call`, keyed on the
/// request's JSON-RPC method (and `GET`/`DELETE`).
fn mcp_server(
    tools: Value,
    call: impl Fn(&Request, &Value) -> Response + Send + Sync + 'static,
) -> ProviderServer {
    ProviderServer::start_responding(move |req: &Request| {
        if req.method == "GET" {
            return Response::status(404, "no stream here");
        }
        if req.method == "DELETE" {
            return Response::status(200, "");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let id = body.get("id").cloned().unwrap_or(Value::Null);
        let answer =
            |result: Value| json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
        match body.get("method").and_then(Value::as_str) {
            Some("initialize") => Response::status(
                200,
                answer(json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "remote", "version": "1"},
                })),
            )
            .header("mcp-session-id", "remote-1"),
            Some("notifications/initialized") | Some("notifications/cancelled") | None => {
                Response::status(202, "")
            }
            Some("tools/list") => Response::status(200, answer(json!({"tools": tools}))),
            Some("tools/call") => call(req, &id),
            Some(other) => panic!("unexpected MCP method {other}"),
        }
    })
    .unwrap()
}

/// The `echo` tool the fake lists: reads-only, but every remote call
/// still declares `network`, so it runs through permission review.
fn echo_tool() -> Value {
    json!({
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": true},
    })
}

/// A reviewer verdict allowing the call.
fn allow_text() -> Response {
    stream(&[json!({"type": "response.output_item.done", "item": {
        "type": "message", "content": [{"type": "output_text", "text": "allow"}]}})])
}

/// An `echo` answer with `text`.
fn echo_answer(id: &Value, text: &str) -> Response {
    Response::status(
        200,
        json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": text}]}})
            .to_string(),
    )
}

/// The JSON-RPC method of a recorded fake request, `""` for `GET`/`DELETE`.
fn request_method(req: &Request) -> String {
    serde_json::from_slice::<Value>(&req.body)
        .ok()
        .and_then(|body| {
            body.get("method")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

#[test]
fn a_remote_server_answering_401_fails_to_start_naming_the_login() {
    for required in [false, true] {
        let setup = Setup::new();
        let model = ProviderServer::start([hello()]).unwrap();
        setup.provider(&model);
        let mcp = ProviderServer::start([Response::status(401, "secret")]).unwrap();
        setup.configure(
            &json!({"remote":{"url":format!("{}/mcp", mcp.url()), "required":required}}),
        );
        let run = setup.run(&["ask", "hi"]);
        let message = if required {
            assert_eq!(run.code, Some(1), "{}", run.stderr);
            assert_eq!(run.kinds(), ["fiber_exited"]);
            let error = &run.first("fiber_exited")["payload"]["error"];
            assert_eq!(error["code"], "mcp_required_server_failed");
            error["message"].as_str().unwrap()
        } else {
            assert_eq!(run.code, Some(0), "{}", run.stderr);
            assert_eq!(run.kinds(), hello_kinds(&["mcp_server_failed"]));
            let failed = &run.first("mcp_server_failed")["payload"];
            assert_eq!(failed["reason"], "not_logged_in");
            failed["error"]["message"].as_str().unwrap()
        };
        assert!(message.contains("fiber mcp login remote"));
        assert!(!message.contains("`url`"));
        assert_eq!(mcp.requests().len(), 1);
    }
}
#[test]
fn a_stored_token_is_sent_to_its_url() {
    let setup = Setup::new();
    let model = ProviderServer::start([
        stream(&[function_call("echo", "mcp__remote__echo", &json!({}))]),
        allow_text(),
        hello(),
    ])
    .unwrap();
    setup.provider_with_reviewer(&model);
    let mcp = mcp_server(json!([echo_tool()]), |_request, id| echo_answer(id, "hi"));
    let url = format!("{}/mcp", mcp.url());
    let token = mcp::Token {
        access_token: contract::Secret::new("at-1".into()),
        refresh_token: None,
        expires_at: None,
        token_endpoint: "https://auth/token".into(),
        client_id: "fiber".into(),
    };
    mcp::store_token(&config::McpTokenFiles::new(setup.home()), &url, &token).unwrap();
    setup.configure(&json!({"remote":{"url":url}}));
    let run = setup.run(&["ask", "echo"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.kinds(), reviewed_kinds(&[&[]]));
    assert_eq!(
        run.first("tool_call_completed")["payload"]["status"],
        "completed"
    );
    assert_eq!(
        mcp.requests()[0].header("authorization"),
        Some(fakes::fingerprint("Bearer at-1").as_str())
    );
}
#[test]
fn a_required_server_url_with_a_password_is_a_config_error() {
    let setup = Setup::new();
    let model = ProviderServer::start([hello()]).unwrap();
    setup.provider(&model);
    let mcp = ProviderServer::start([]).unwrap();
    let url = mcp.url().replacen("http://", "http://u:pw-594@", 1);
    setup.configure(&json!({"remote":{"url":format!("{url}/mcp"), "required":true}}));
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(1), "{}", run.stderr);
    assert_eq!(run.kinds(), ["fiber_exited"]);
    let error = &run.first("fiber_exited")["payload"]["error"];
    assert_eq!(error["code"], "config_invalid");
    assert!(error["message"].as_str().unwrap().contains("remote"));
    for line in &run.lines {
        assert!(!line.to_string().contains("pw-594"));
    }
    assert!(!run.stderr.contains("pw-594"));
    assert!(mcp.requests().is_empty());
}
#[test]
fn a_read_of_a_stored_mcp_token_is_refused() {
    for shell in [false, true] {
        let setup = Setup::new();
        mcp::store_token(
            &config::McpTokenFiles::new(setup.home()),
            "https://h/mcp",
            &mcp::Token {
                access_token: contract::Secret::new("protected-mcp-token-594".into()),
                refresh_token: None,
                expires_at: None,
                token_endpoint: "https://auth/token".into(),
                client_id: "fiber".into(),
            },
        )
        .unwrap();
        let path = fs::read_dir(setup.home().join("credentials/mcp"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_none())
            .unwrap();
        let (name, arguments) = if shell {
            (
                "shell",
                json!({"command":format!("cat '{}'", path.display())}),
            )
        } else {
            ("read", json!({"path":path}))
        };
        let model =
            ProviderServer::start([stream(&[function_call("deny", name, &arguments)]), hello()])
                .unwrap();
        setup.provider(&model);
        let run = setup.run(&["ask", "read the credential"]);
        assert_eq!(run.code, Some(0), "{}", run.stderr);
        let mut expected = read_kinds();
        let at = expected
            .iter()
            .position(|kind| *kind == "tool_call_started")
            .unwrap();
        expected[at] = "permission_resolved";
        assert_eq!(run.kinds(), expected);
        assert_eq!(
            run.first("permission_resolved")["payload"]["decided_by"],
            "credential_deny"
        );
        let completed = &run.first("tool_call_completed")["payload"];
        assert_eq!(completed["status"], "denied");
        assert_eq!(completed["reason"], "credentials");
        for line in &run.lines {
            assert!(!line.to_string().contains("protected-mcp-token-594"));
        }
        assert!(!run.stderr.contains("protected-mcp-token-594"));
        let workspace = fs::canonicalize(setup.workspace()).unwrap();
        let project = workspace.to_string_lossy().replace('/', "-");
        let dir = setup
            .home()
            .join("projects")
            .join(project)
            .join("sessions")
            .join(run.session_id());
        assert!(!support::on_disk(&dir).contains("protected-mcp-token-594"));
    }
}

#[test]
fn a_configured_url_server_declares_and_runs_its_tools() {
    let setup = Setup::new();
    let model = ProviderServer::start([
        stream(&[function_call(
            "call_echo",
            "mcp__remote__echo",
            &json!({"text": "hi"}),
        )]),
        allow_text(),
        hello(),
    ])
    .unwrap();
    setup.provider_with_reviewer(&model);
    let mcp = mcp_server(json!([echo_tool()]), |_req, id| echo_answer(id, "hi"));
    setup.configure(&json!({"remote": {"url": format!("{}/mcp", mcp.url())}}));

    let run = setup.run(&["ask", "echo hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), reviewed_kinds(&[&[]]));
    // Every tool of a remote server declares `network`.
    let started = run.first("tool_call_started");
    assert!(
        started["payload"]["effects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|effect| effect == "network"),
        "effects: {}",
        started["payload"]["effects"]
    );
    // The call's result reaches the model's second request.
    let completed = run.first("tool_call_completed");
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"][0]["text"], "hi");
    let requests = model.requests();
    let output = requests
        .iter()
        .find_map(|req| {
            serde_json::from_slice::<Value>(&req.body)
                .ok()?
                .get("input")?
                .as_array()?
                .iter()
                .find(|item| item["type"] == "function_call_output")
                .cloned()
        })
        .expect("the call's result reaches a later model request");
    assert_eq!(output["output"], "hi");
    // The session lists and runs over the wire, and says goodbye at exit.
    let methods: Vec<String> = mcp
        .requests()
        .iter()
        .map(|req| format!("{} {}", req.method, request_method(req)))
        .collect();
    assert_eq!(
        methods,
        [
            "POST initialize",
            "POST notifications/initialized",
            "POST tools/list",
            "POST tools/call",
            "DELETE ",
        ]
    );
    let recorded = mcp.requests();
    let call = recorded
        .iter()
        .find(|req| request_method(req) == "tools/call")
        .unwrap();
    assert_eq!(call.header("mcp-session-id"), Some("remote-1"));
}

#[test]
fn a_legacy_server_fails_to_start_saying_so() {
    let setup = Setup::new();
    let model = ProviderServer::start([hello()]).unwrap();
    setup.provider(&model);
    let mcp = ProviderServer::start([
        Response::status(405, "method not allowed"),
        Response::stream("event: endpoint\ndata: /messages?sessionId=abc\n\n"),
    ])
    .unwrap();
    setup.configure(&json!({"remote": {"url": format!("{}/mcp", mcp.url())}}));

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&["mcp_server_failed"]));
    let failed = run.first("mcp_server_failed");
    assert_eq!(failed["payload"]["server"], "remote");
    assert_eq!(failed["payload"]["reason"], "start_failed");
    assert_eq!(failed["payload"]["will_restart"], false);
    let message = failed["payload"]["error"]["message"].as_str().unwrap();
    assert!(message.contains("HTTP+SSE"), "{message}");
    assert!(message.contains("`url`"), "{message}");
}

#[test]
fn a_second_session_declares_cached_remote_tools_without_starting() {
    let setup = Setup::new();
    let model = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&model);
    let mcp = mcp_server(json!([echo_tool()]), |_req, id| echo_answer(id, "hi"));
    setup.configure(&json!({"remote": {"url": format!("{}/mcp", mcp.url())}}));

    // The first session populates the cache with a session start.
    let first = setup.run(&["ask", "hi"]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let before = mcp.requests().len();
    assert!(before >= 3, "the first session lists: {before}");

    // The second session declares from the cache: the model calls
    // nothing, and the MCP server records no request.
    let run = setup.run(&["ask", "hi"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), hello_kinds(&[]));
    assert_eq!(
        mcp.requests().len(),
        before,
        "a cached server starts on its first call only"
    );
    let preamble = run.first("preamble_built");
    let sent: Vec<_> = preamble["payload"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(sent.contains(&"mcp__remote__echo"), "{sent:?}");
}

#[test]
fn a_remote_server_that_drops_reconnects_on_the_next_call() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let model = ProviderServer::start([
        stream(&[function_call(
            "call_first",
            "mcp__remote__echo",
            &json!({"text": "hi"}),
        )]),
        allow_text(),
        stream(&[function_call(
            "call_second",
            "mcp__remote__echo",
            &json!({"text": "hi"}),
        )]),
        allow_text(),
        hello(),
    ])
    .unwrap();
    setup.provider_with_reviewer(&model);
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let mcp = mcp_server(json!([echo_tool()]), move |_req, id| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Response::drop_connection()
        } else {
            echo_answer(id, "hi")
        }
    });
    setup.configure(&json!({"remote": {"url": format!("{}/mcp", mcp.url())}}));

    let run = setup.run(&["ask", "echo twice"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(
        run.kinds(),
        reviewed_kinds(&[&["mcp_server_failed"], &["mcp_server_ready"]]),
    );
    let failed = run.first("mcp_server_failed");
    assert_eq!(
        failed["payload"]["error"]["message"],
        "The MCP server `remote` stopped answering; Fiber reconnects on the next call."
    );
    assert_eq!(failed["payload"]["will_restart"], true);
    assert_eq!(run.all("mcp_server_ready").len(), 1);
    let methods: Vec<String> = mcp
        .requests()
        .iter()
        .map(|req| format!("{} {}", req.method, request_method(req)))
        .collect();
    assert_eq!(
        methods
            .iter()
            .filter(|method| method.as_str() == "POST initialize")
            .count(),
        2,
        "the next call re-initializes: {methods:?}"
    );
}

#[test]
fn an_expired_remote_session_renews_without_a_record() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let setup = Setup::new();
    let model = ProviderServer::start([
        stream(&[function_call(
            "call_first",
            "mcp__remote__echo",
            &json!({"text": "hi"}),
        )]),
        allow_text(),
        stream(&[function_call(
            "call_second",
            "mcp__remote__echo",
            &json!({"text": "hi"}),
        )]),
        allow_text(),
        hello(),
    ])
    .unwrap();
    setup.provider_with_reviewer(&model);
    let calls = std::sync::Arc::new(AtomicUsize::new(0));
    let mcp = mcp_server(json!([echo_tool()]), move |_req, id| {
        if calls.fetch_add(1, Ordering::SeqCst) == 0 {
            Response::status(404, "ended")
        } else {
            echo_answer(id, "hi")
        }
    });
    setup.configure(&json!({"remote": {"url": format!("{}/mcp", mcp.url())}}));

    let run = setup.run(&["ask", "echo twice"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), reviewed_kinds(&[&[], &[]]));
    let completed: Vec<&Value> = run.all("tool_call_completed");
    assert_eq!(completed.len(), 2);
    assert_eq!(completed[0]["payload"]["status"], "failed");
    assert_eq!(
        completed[0]["payload"]["error"]["code"],
        "mcp_server_unavailable"
    );
    assert_eq!(
        completed[0]["payload"]["error"]["message"],
        "The MCP server `remote` ended its session; Fiber starts a new one on the next call."
    );
    assert_eq!(completed[1]["payload"]["status"], "completed");
    assert!(
        run.lines
            .iter()
            .all(|line| line["kind"] != "mcp_server_failed"),
        "an expiry records nothing"
    );
    assert!(
        run.lines
            .iter()
            .all(|line| line["kind"] != "mcp_server_ready"),
        "a renewal records no ready"
    );
    let methods: Vec<String> = mcp
        .requests()
        .iter()
        .map(|req| format!("{} {}", req.method, request_method(req)))
        .collect();
    assert_eq!(
        methods
            .iter()
            .filter(|method| method.as_str() == "POST initialize")
            .count(),
        2,
        "the next call starts a new session: {methods:?}"
    );
}

#[test]
fn a_server_with_both_command_and_url_is_never_started() {
    // Required: the session fails before its log exists, and the model
    // is never called.
    let setup = Setup::new();
    let model = ProviderServer::start([hello()]).unwrap();
    setup.provider(&model);
    let mcp = mcp_server(json!([echo_tool()]), |_req, id| echo_answer(id, "hi"));
    setup.configure(&json!({"fx": {
        "command": "/bin/bash",
        "url": format!("{}/mcp", mcp.url()),
        "required": true,
    }}));

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(1), "stderr: {}", run.stderr);
    assert!(
        model.requests().is_empty(),
        "the session never starts, so the model is never called"
    );
    let line = run.lines.first().unwrap();
    assert_eq!(line["payload"]["error"]["code"], "config_invalid");
    assert!(
        line["payload"]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("The required MCP server `fx` declares both `command` and `url`"),
        "message: {}",
        line["payload"]["error"]["message"]
    );

    // Not required: one notice, and the session answers.
    let setup = Setup::new();
    let model = ProviderServer::start([hello()]).unwrap();
    setup.provider(&model);
    setup.configure(&json!({"fx": {
        "command": "/bin/bash",
        "url": format!("{}/mcp", mcp.url()),
    }}));

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let notices: Vec<&Value> = run
        .lines
        .iter()
        .filter(|line| line["kind"] == "notice" && line["payload"]["code"] == "config_invalid")
        .collect();
    assert_eq!(notices.len(), 1);
    assert!(
        notices[0]["payload"]["message"]
            .as_str()
            .unwrap()
            .contains("The MCP server `fx` declares both `command` and `url`"),
        "message: {}",
        notices[0]["payload"]["message"]
    );
}

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    let mut files: Vec<_> = fs::read_dir(&logs)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

#[test]
fn a_debug_ask_writes_one_mcp_line_per_request() {
    let setup = Setup::new();
    let model = ProviderServer::start([
        stream(&[function_call("echo", "mcp__remote__echo", &json!({}))]),
        allow_text(),
        hello(),
    ])
    .unwrap();
    setup.provider_with_reviewer(&model);
    let mcp = mcp_server(json!([echo_tool()]), |_request, id| echo_answer(id, "hi"));
    let url = format!("{}/mcp", mcp.url());
    setup.configure(&json!({"remote": {"url": url}}));
    // Diagnostics at debug, keeping the model, reviewer and servers.
    let path = setup.home().join("config.json");
    let mut config: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    config["diagnostics"] = json!({"level": "debug"});
    write(&path, &config);
    let run = setup.run(&["ask", "echo"]);
    assert_eq!(run.code, Some(0), "{}", run.stderr);
    assert_eq!(run.kinds(), reviewed_kinds(&[&[]]));
    assert_eq!(
        run.first("tool_call_completed")["payload"]["status"],
        "completed"
    );
    let requests = mcp.requests();
    assert!(
        requests
            .iter()
            .any(|request| request_method(request) == "tools/list"),
        "the turn lists tools"
    );
    assert!(
        requests
            .iter()
            .any(|request| request_method(request) == "tools/call"),
        "the turn calls the tool"
    );
    let all = log_lines(&setup);
    let lines: Vec<&Value> = all
        .iter()
        .filter(|line| line["code"] == "http_request" && line["data"]["purpose"] == "mcp")
        .collect();
    // Every fake request, handshake included, writes exactly one line
    // (docs/state.md, "Diagnostic logs": one line per POST the transport
    // sends, and one per GET or DELETE it sends).
    assert_eq!(lines.len(), requests.len(), "{lines:?}");
    for line in &lines {
        assert_eq!(line["process"], "ask");
        assert_eq!(line["message"], "An HTTP request ended.");
        let data = &line["data"];
        assert!(data.get("provider").is_none(), "{data}");
        assert!(data.get("extension").is_none(), "{data}");
        assert!(data.get("model").is_none(), "{data}");
        assert!(data.get("host").is_none(), "{data}");
        assert!(data.get("path").is_none(), "{data}");
        assert_eq!(data["attempt"], 1);
        assert!(data["status"].is_u64(), "every request answered: {data}");
    }
    // The sent lengths match the recorded bodies, however they interleave.
    let mut sent: Vec<u64> = lines
        .iter()
        .map(|line| line["data"]["request_bytes"].as_u64().unwrap())
        .collect();
    sent.sort_unstable();
    let mut recorded: Vec<u64> = requests
        .iter()
        .map(|request| request.body.len() as u64)
        .collect();
    recorded.sort_unstable();
    assert_eq!(sent, recorded, "one line per request body");
    // The tool call's line carries the 200 and its answer's bytes.
    let call = requests
        .iter()
        .find(|request| request_method(request) == "tools/call")
        .unwrap();
    let call_line = lines
        .iter()
        .find(|line| line["data"]["request_bytes"] == call.body.len() as u64)
        .expect("the call's line");
    assert_eq!(call_line["data"]["status"], 200);
    assert!(
        call_line["data"]["response_bytes"].as_u64().unwrap() > 0,
        "{call_line}"
    );
    // The call runs inside the turn, after the session attaches: its
    // line carries the session id. The handshake predates the attach.
    assert_eq!(call_line["session_id"], run.session_id());
}
