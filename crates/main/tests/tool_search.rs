//! Binary-level tests of `tool_search` through `fiber ask` and one
//! `fiber session` (`docs/testing.md`, "Levels"): the built `fiber` runs in
//! its own process group with its own `FIBER_HOME`, holding a provider on
//! `openai-responses` with `deferred_tools` on at the fake server unless the
//! case says otherwise, and a configured fixture server. Every wait runs
//! under the test's [`Deadline`] with the setup's [`Watchdog`] beside it.
//! Every case asserts the full ordered list of event kinds.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]
mod support;

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Watchdog};
use serde_json::{Value, json};
use support::{
    Deadline, HELLO_KINDS, KillGroup, Run, Socket, first_line, function_call, group_alive, hello,
    is_status, kinds, read_kinds, spawn_watched, stream, until, until_close,
};

/// A temporary root holding Fiber home and the workspace, removed on drop.
/// Its name is short: a session's socket path must fit in 103 bytes on
/// macOS.
struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
    /// Kills every process whose command line names the test's root,
    /// and their groups, when the setup drops: every fixture server runs
    /// with its directory under the root as its argument, so a hang or a
    /// failing exit still kills the server and its grandchild.
    #[allow(dead_code, reason = "held only for its drop")]
    guard: Watchdog,
}

impl Setup {
    fn new() -> Self {
        let deadline = Deadline::start();
        let root = fakes::TempDir::new("fa");
        // Armed before any start or run: the root is known here, while
        // no server pid is.
        let guard = Watchdog::matching(&root.path().display().to_string());
        fs::create_dir_all(root.path().join("h")).unwrap();
        fs::create_dir_all(root.path().join("w")).unwrap();
        Self {
            root,
            deadline,
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
    /// fake server, and makes `fake/m` the configured model. `deferred`
    /// decides whether the model's data sets `deferred_tools`.
    fn provider_with_deferral(&self, server: &ProviderServer, deferred: bool) {
        let mut model = json!({"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000});
        if deferred {
            model["deferred_tools"] = json!(true);
        }
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

    /// Installs the provider with `deferred_tools` on.
    fn provider(&self, server: &ProviderServer) {
        self.provider_with_deferral(server, true);
    }

    /// Adds `servers` under `mcp.servers` and `tools` under `tools` to the
    /// global configuration, keeping the configured model.
    fn configure_with(&self, servers: &Value, tools: &Value) {
        write(
            &self.home().join("config.json"),
            &json!({"model": "fake/m", "tools": tools, "mcp": {"servers": servers}}),
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

    /// One `fiber` invocation with `args`: the environment every test
    /// runs under. Stdio is piped; the caller decides how to wait.
    fn fiber(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(args)
            .current_dir(self.workspace())
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn session_socket(&self, id: &str) -> PathBuf {
        self.home().join("run").join(id)
    }

    /// Runs `fiber ask` in its own process group, waits for it under the
    /// test's [`Deadline`], and asserts that nothing it started is left in the
    /// group, after a timeout too (`docs/testing.md`, "Running tests").
    /// A watchdog beside it kills that group if this process dies first.
    /// The fixture server runs detached from Fiber's group; the setup's
    /// guard kills it and its grandchild with the setup when the test
    /// ends, after the asserts below.
    #[track_caller]
    fn run(&self, args: &[&str]) -> Run {
        let (child, watchdog) = spawn_watched(&mut self.fiber(args));
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

fn write(file: &Path, value: &Value) {
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

/// The echo tool: reads-only and offline, so its calls take the permission
/// fast path.
fn echo_tool() -> Value {
    json!({
        "name": "echo",
        "description": "Echoes.",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
        },
        "annotations": {"readOnlyHint": true, "openWorldHint": false},
    })
}

/// The one configured fixture server `fx` in `dir`.
fn fx_server(dir: &Path) -> Value {
    json!({
        "command": "/bin/bash",
        "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
        "startup_timeout_ms": 600000,
    })
}

/// Configures one fixture server `fx` in `dir` and returns nothing.
fn configure_fx(setup: &Setup, dir: &Path) {
    setup.configure_with(&json!({"fx": fx_server(dir)}), &json!({}));
}

/// A finished `tool_search_call` for `query`, as the provider's reply.
fn search_call(query: &str) -> Value {
    json!({"type": "response.output_item.done", "item": {
        "type": "tool_search_call", "id": "ts_1", "call_id": "call_1",
        "execution": "client", "status": "completed",
        "arguments": {"query": query}
    }})
}

/// The `tools` array of request `body`, parsed.
fn request_tools(body: &[u8]) -> Vec<Value> {
    let body: Value = serde_json::from_slice(body).unwrap();
    body["tools"].as_array().unwrap().clone()
}

/// Whether `tools` holds OpenAI's client-executed search declaration.
fn has_native_search(tools: &[Value]) -> bool {
    tools.iter().any(|tool| {
        tool["type"] == "tool_search"
            && tool.get("execution").and_then(Value::as_str) == Some("client")
    })
}

/// The `function` entry for `name` in `tools`.
fn function_entry<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["type"] == "function" && tool["name"] == name)
        .unwrap_or_else(|| panic!("no function entry for {name}: {tools:?}"))
}

/// The sent deferred flag of `name` in a `preamble_built` line.
fn sent_deferred(line: &Value, name: &str) -> bool {
    line["payload"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == name)
        .unwrap_or_else(|| panic!("no sent {name}"))
        .get("deferred")
        .and_then(Value::as_bool)
        .unwrap()
}

/// The search's `tool_call_completed`, by its `loaded` list.
fn search_completed(lines: &[Value]) -> &Value {
    lines
        .iter()
        .filter(|line| line["kind"] == "tool_call_completed")
        .find(|line| !line["payload"].get("loaded").is_none())
        .expect("one search tool_call_completed")
}

/// The event kinds of a turn whose first reply calls the search, whose
/// second calls one reads-only tool, and whose third is [`hello`]: neither
/// call takes the permission path.
fn search_then_call_kinds() -> Vec<&'static str> {
    let mut expected = read_kinds();
    expected.splice(
        13..13,
        [
            "step_started",
            "assistant_message_started",
            "tool_call_requested",
            "usage_recorded",
            "assistant_message_completed",
            "tool_call_started",
            "tool_call_completed",
        ],
    );
    expected
}

/// (a) Deferral on: request 1 declares the native search next to the
/// deferred echo; the search loads the echo, the model calls it, and the
/// echo's result reaches the third request.
#[test]
fn a_deferred_search_loads_before_the_call_it_loads() {
    let setup = Setup::new();
    let dir = setup.fixture(
        &json!([echo_tool()]),
        &[(
            "call-echo.json",
            r#"{"content":[{"type":"text","text":"hi"}]}"#,
        )],
    );
    let server = ProviderServer::start([
        stream(&[search_call("echo")]),
        stream(&[function_call(
            "call_echo",
            "mcp__fx__echo",
            &json!({"text": "hi"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir);

    let run = setup.run(&["ask", "echo hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), search_then_call_kinds());
    // Request 1 declares the native search alongside the deferred echo.
    let requests = server.requests();
    assert_eq!(requests.len(), 3);
    let first = request_tools(&requests[0].body);
    assert!(has_native_search(&first), "{first:?}");
    assert_eq!(
        function_entry(&first, "mcp__fx__echo")["defer_loading"],
        json!(true),
        "{first:?}"
    );
    let built = run
        .lines
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert!(sent_deferred(built, "mcp__fx__echo"));
    assert!(!sent_deferred(built, "tool_search"));
    // The search loads the echo before the model calls it.
    let completed = search_completed(&run.lines);
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["loaded"], json!(["mcp__fx__echo"]));
    // Request 2 carries the search call and its output holding the echo's
    // declared object; request 3 holds the echo call's output.
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let input = second["input"].as_array().unwrap();
    assert!(input.iter().any(|item| item["type"] == "tool_search_call"));
    let output = input
        .iter()
        .find(|item| item["type"] == "tool_search_output")
        .expect("a tool_search_output");
    let loaded = output["tools"].as_array().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0]["name"], "mcp__fx__echo");
    let third: Value = serde_json::from_slice(&requests[2].body).unwrap();
    let output = third["input"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["type"] == "function_call_output")
        .unwrap();
    assert_eq!(output["output"], "hi");
}

/// (b) `declare_in_full`: the server's tools are declared whole, so no
/// search is declared.
#[test]
fn a_declare_in_full_server_declares_whole_with_no_search() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    let mut fx = fx_server(&dir);
    fx["declare_in_full"] = json!(true);
    setup.configure_with(&json!({"fx": fx}), &json!({}));

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS.to_vec());
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let tools = request_tools(&requests[0].body);
    assert!(!has_native_search(&tools), "{tools:?}");
    assert!(
        function_entry(&tools, "mcp__fx__echo")
            .get("defer_loading")
            .is_none(),
        "{tools:?}"
    );
    let built = run
        .lines
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert!(!sent_deferred(built, "mcp__fx__echo"));
}

/// (c) Provider data without `deferred_tools`: every tool is declared in
/// full and no search is declared.
#[test]
fn without_deferred_tools_everything_is_full_with_no_search() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider_with_deferral(&server, false);
    configure_fx(&setup, &dir);

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS.to_vec());
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let tools = request_tools(&requests[0].body);
    assert!(!has_native_search(&tools), "{tools:?}");
    assert!(
        function_entry(&tools, "mcp__fx__echo")
            .get("defer_loading")
            .is_none(),
        "{tools:?}"
    );
}

/// (d) `tools."mcp__fx__echo".deferred: false`: echo is declared whole
/// next to the other builtins, so no search is declared.
#[test]
fn an_echo_deferral_key_sends_echo_whole_with_no_search() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.configure_with(
        &json!({"fx": fx_server(&dir)}),
        &json!({"mcp__fx__echo": {"deferred": false}}),
    );

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), HELLO_KINDS.to_vec());
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let tools = request_tools(&requests[0].body);
    assert!(!has_native_search(&tools), "{tools:?}");
    assert!(
        function_entry(&tools, "mcp__fx__echo")
            .get("defer_loading")
            .is_none(),
        "{tools:?}"
    );
    let built = run
        .lines
        .iter()
        .find(|line| line["kind"] == "preamble_built")
        .unwrap();
    assert!(!sent_deferred(built, "mcp__fx__echo"));
}

/// (e) `tools."tool_search".deferred` has no effect: the ask logs one
/// notice and still declares the search next to the deferred echo.
#[test]
fn a_tool_search_deferral_key_logs_one_notice_and_declares() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.configure_with(
        &json!({"fx": fx_server(&dir)}),
        &json!({"tool_search": {"deferred": false}}),
    );

    let run = setup.run(&["ask", "hi"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let mut expected = HELLO_KINDS.to_vec();
    expected.insert(3, "notice");
    assert_eq!(run.kinds(), expected);
    let notices: Vec<_> = run.lines.iter().filter(|l| l["kind"] == "notice").collect();
    assert_eq!(notices.len(), 1, "{:?}", run.kinds());
    assert_eq!(notices[0]["payload"]["code"], "config_key_ignored");
    assert_eq!(
        notices[0]["payload"]["message"],
        "`tools.\"tool_search\".deferred` has no effect: `tool_search` is never deferred."
    );
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let tools = request_tools(&requests[0].body);
    assert!(has_native_search(&tools), "{tools:?}");
    assert_eq!(
        function_entry(&tools, "mcp__fx__echo")["defer_loading"],
        json!(true),
        "{tools:?}"
    );
}

/// (f) `/reload` applies a scoped deferral key: the session starts
/// deferring, the config exempts echo, and after the reload the next turn
/// declares echo whole with no search and the rows keep echo's source.
#[test]
fn a_reload_applies_an_echo_deferral_key() {
    let setup = Setup::new();
    let dir = setup.fixture(&json!([echo_tool()]), &[]);
    let server = ProviderServer::start([hello(), hello()]).unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir);

    let mut session = Session::start(setup);
    session.first_hello_turn();
    assert!(
        server.await_requests(1, session.setup.deadline),
        "the first turn sent one request"
    );
    // The first turn starts deferring, with (a)'s request shape.
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let first = request_tools(&requests[0].body);
    assert!(has_native_search(&first), "{first:?}");
    assert_eq!(
        function_entry(&first, "mcp__fx__echo")["defer_loading"],
        json!(true),
        "{first:?}"
    );
    // The rows list echo under its server, deferred.
    let answer = session.tools_answer("c_tools");
    let rows = answer["payload"]["result"]["tools"].as_array().unwrap();
    let echo = rows
        .iter()
        .find(|tool| tool["name"] == "mcp__fx__echo")
        .expect("an echo row");
    assert_eq!(echo["source"], "mcp");
    assert_eq!(echo["server"], "fx");
    assert_eq!(echo["tool"], "echo");
    assert_eq!(echo["state"], "deferred");
    // The config exempts echo; the reload applies it.
    session.setup.configure_with(
        &json!({"fx": fx_server(&dir)}),
        &json!({"mcp__fx__echo": {"deferred": false}}),
    );
    session.reload();
    session.hello_turn();
    assert!(
        server.await_requests(2, session.setup.deadline),
        "the second turn sent one more request"
    );
    // The next turn declares echo whole with no search, and the rows keep
    // echo's source, now full, with no search row.
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let tools = request_tools(&requests[1].body);
    assert!(!has_native_search(&tools), "{tools:?}");
    assert!(
        function_entry(&tools, "mcp__fx__echo")
            .get("defer_loading")
            .is_none(),
        "{tools:?}"
    );
    let answer = session.tools_answer("c_tools2");
    let rows = answer["payload"]["result"]["tools"].as_array().unwrap();
    let echo = rows
        .iter()
        .find(|tool| tool["name"] == "mcp__fx__echo")
        .expect("an echo row");
    assert_eq!(echo["source"], "mcp");
    assert_eq!(echo["server"], "fx");
    assert_eq!(echo["tool"], "echo");
    assert_eq!(echo["state"], "full");
    assert!(
        rows.iter().all(|tool| tool["name"] != "tool_search"),
        "{rows:?}"
    );
    session.finish();
}

/// One `fiber session` under the test's [`Deadline`]: prompted, reloaded
/// and asked for its rows, then closed. The stream's kinds must run the
/// full expected list: startup, the first turn (a search, a call, hello),
/// the rows answer, the reload, the second turn (hello), the rows answer
/// again, and the close.
struct Session {
    child: Child,
    guard: KillGroup,
    watchdog: Watchdog,
    stderr: mpsc::Receiver<String>,
    client: Socket,
    setup: Setup,
    stream: Vec<Value>,
    expected: Vec<&'static str>,
}

impl Session {
    fn start(setup: Setup) -> Self {
        let id = doors::mint("s_");
        let workspace = setup.workspace();
        let (mut child, watchdog) = spawn_watched(&mut setup.fiber(&[
            "session",
            "--id",
            &id,
            "--workspace",
            workspace.to_str().unwrap(),
        ]));
        let guard = KillGroup(child.id());
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        let (tx, stderr) = mpsc::channel();
        thread::spawn(move || {
            let mut text = String::new();
            stderr_pipe
                .take(u64::MAX)
                .read_to_string(&mut text)
                .unwrap();
            tx.send(text).unwrap();
        });
        first_line(setup.deadline, &lines);
        let client = Socket::connect(setup.deadline, &setup.session_socket(&id));
        client.send(r#"{"id":"sub","command":"subscribe","args":{"level":"full"}}"#);
        // `clients` is written whenever a `full` connection attaches, with
        // no order against the startup events (`docs/events.md`): the wait
        // returns once both `clients` and `extensions_loaded` have arrived,
        // in any order, keeping every line in arrival order.
        let mut seen_clients = false;
        let mut seen_extensions = false;
        let stream = until(&client, "startup", |line| {
            if line["kind"] == "clients" {
                seen_clients = true;
            }
            if line["kind"] == "extensions_loaded" {
                seen_extensions = true;
            }
            seen_clients && seen_extensions
        });
        let clients = stream
            .iter()
            .filter(|line| line["kind"] == "clients")
            .count();
        assert_eq!(clients, 1, "{stream:?}");
        Self {
            child,
            guard,
            watchdog,
            stderr,
            client,
            setup,
            stream,
            expected: vec![
                "command_accepted",
                "session_started",
                "fiber_started",
                "extensions_loaded",
            ],
        }
    }

    fn prompt(&mut self) {
        self.client.send(
            &json!({"id":doors::mint("c_"),"command":"prompt","args":{"content":[{"type":"text","text":"hi"}]}})
                .to_string(),
        );
        self.stream
            .extend(until(&self.client, "turn completed", |line| {
                line["kind"] == "turn_completed"
            }));
    }

    /// A prompt answered by [`hello`]: the turn's kinds after an already
    /// built preamble.
    fn hello_turn(&mut self) {
        self.prompt();
        self.expected.extend([
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]);
    }

    /// The first prompt, answered by [`hello`]: the turn's kinds with the
    /// preamble and the opening message before them.
    fn first_hello_turn(&mut self) {
        self.prompt();
        self.expected
            .splice(4..4, ["preamble_built", "opening_message"]);
        self.expected.extend([
            "turn_started",
            "command_accepted",
            "step_started",
            "assistant_message_started",
            "assistant_message_delta",
            "assistant_message_delta",
            "text_completed",
            "usage_recorded",
            "assistant_message_completed",
            "turn_completed",
        ]);
    }

    fn reload(&mut self) {
        self.client
            .send(&json!({"id":doors::mint("c_"),"command":"reload"}).to_string());
        self.stream
            .extend(until(&self.client, "reload preamble", |line| {
                line["kind"] == "preamble_built" && line["payload"]["reason"] == "reload"
            }));
        self.expected.extend([
            "reloaded",
            "command_accepted",
            "extensions_loaded",
            "preamble_built",
        ]);
    }

    /// Asks for the `tools` answer as `id` and returns it.
    fn tools_answer(&mut self, id: &str) -> Value {
        self.client
            .send(&format!(r#"{{"id":"{id}","command":"tools"}}"#));
        let lines = until(&self.client, "the tools answer", |line| {
            line["payload"]["command_id"] == id
        });
        self.stream.extend(lines.clone());
        self.expected.push("command_accepted");
        let answer = lines.last().unwrap();
        assert_eq!(answer["kind"], "command_accepted", "{answer}");
        answer.clone()
    }

    fn finish(mut self) {
        self.client.send(r#"{"id":"close","command":"close"}"#);
        self.stream.extend(until_close(&self.client));
        let group = self.child.id();
        let (tx, done) = mpsc::channel();
        thread::spawn(move || {
            tx.send(self.child.wait()).unwrap();
        });
        let status = self.setup.deadline.recv(&done).unwrap().unwrap();
        let stderr = self.setup.deadline.recv(&self.stderr).unwrap();
        assert!(status.success(), "{stderr}");
        assert!(!group_alive(self.setup.deadline, group));
        std::mem::forget(self.guard);
        self.watchdog
            .stand_down(self.setup.deadline.cleanup_phase());
        self.expected.extend(["command_accepted", "fiber_exited"]);
        let stream: Vec<Value> = self
            .stream
            .into_iter()
            .filter(|line| line["kind"] != "clients")
            .collect();
        assert_eq!(kinds(&stream), self.expected, "{stream:?}");
    }
}
