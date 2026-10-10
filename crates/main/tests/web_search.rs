//! Binary-level tests of Fiber's own `web_search` over an installed search
//! backend (`docs/tools.md`, "Fiber's own, over a backend";
//! `docs/testing.md`, "Levels"): the built `fiber` runs in its own process
//! group with its own `FIBER_HOME`, holding an ordinary provider whose base
//! URL is the fake server, and the backend extensions under test.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod extension_harness;
mod support;

use std::fs;
use std::path::PathBuf;
use std::process::Output;
use std::sync::mpsc;
use std::thread;

use extension_harness::Setup;
use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{function_call, hello_single_delta as hello, outputs_sent, stream};

/// One finished `fiber ask`: its exit code, its stdout lines parsed (less
/// `session_status`, which an observer thread writes), its raw stdout and
/// its stderr.
struct Run {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .filter(|line| !line.contains(r#""kind":"session_status""#))
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

    fn all(&self, kind: &str) -> Vec<&Value> {
        self.lines
            .iter()
            .filter(|line| line["kind"] == kind)
            .collect()
    }

    fn payload(&self, kind: &str) -> &Value {
        &self
            .lines
            .iter()
            .find(|line| line["kind"] == kind)
            .unwrap_or_else(|| panic!("no {kind} line"))["payload"]
    }

    /// `preamble_built`'s row for the tool `name`.
    fn sent_tool(&self, name: &str) -> &Value {
        self.payload("preamble_built")["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("no tool {name} in preamble_built"))
    }

    /// Each notice's code and message.
    fn notices(&self) -> Vec<(String, String)> {
        self.all("notice")
            .iter()
            .map(|line| {
                (
                    line["payload"]["code"].as_str().unwrap().to_owned(),
                    line["payload"]["message"].as_str().unwrap().to_owned(),
                )
            })
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

/// Runs `fiber` with `args` in the workspace, in its own process group,
/// and waits for it under the test's deadline.
fn run_fiber(setup: &Setup, args: &[&str]) -> Run {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let child = command.spawn().unwrap();
    let group = child.id();
    let watchdog = Watchdog::group(group);
    let (done, finished) = mpsc::channel();
    thread::spawn(move || done.send(child.wait_with_output()).unwrap());
    let output = match finished.recv_timeout(setup.deadline.left()) {
        Ok(output) => output.unwrap(),
        Err(_) => support::expired(setup.deadline, group, &finished, "`fiber` to exit"),
    };
    assert!(
        !support::group_alive(setup.deadline, group),
        "`fiber` left a process in its group behind"
    );
    watchdog.stand_down(setup.deadline.cleanup());
    Run::from(output)
}

/// Runs `fiber ask <prompt>` and asserts it exits 0.
fn ask(setup: &Setup, prompt: &str) -> Run {
    let run = run_fiber(setup, &["ask", prompt]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    run
}

/// A server whose model makes `calls` in one step, then answers `Hello.`.
fn calling(calls: &[Value]) -> ProviderServer {
    ProviderServer::start([stream(calls), hello()]).unwrap()
}

/// The request `index`'s `tools`.
fn sent_tools(server: &ProviderServer, index: usize) -> Vec<Value> {
    let request: Value = serde_json::from_slice(&server.requests()[index].body).unwrap();
    request["tools"].as_array().unwrap().clone()
}

/// A search backend fixture: `fiber.test/<short>` registers `backend`,
/// whose one result carries `prefix` in its title and echoes the call's
/// argument as its snippet.
fn echo_backend(backend: &str, prefix: &str) -> String {
    format!(
        "fiber.search_backend(\"{backend}\", {{ timeout = 10000, run = function(arg) \
           return {{ {{ title = \"{prefix} 1\", url = \"https://example.com/1\", \
           snippet = json.encode(arg) }} }} end }})\n"
    )
}

/// Installs one echo backend and allows `web_search` by standing rule, so
/// the network call runs unreviewed.
fn echo_setup(short: &str, backend: &str, prefix: &str, setup: &Setup) {
    setup.lua(short, &echo_backend(backend, prefix));
    allow_web_search(setup);
}

/// A standing allow for `web_search`, so the backend call runs without a
/// person to review it.
fn allow_web_search(setup: &Setup) {
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "web_search", "prefix": ""})
        ),
    )
    .unwrap();
}

/// Overwrites the session's `config.json` with `model` and `web_search`.
fn write_config(setup: &Setup, config: &Value) {
    fs::write(setup.home().join("config.json"), config.to_string()).unwrap();
}

/// The complete event kinds of a run whose model calls `web_search` once
/// (allowed by the standing rule, so `permission_resolved` names it) then
/// answers `Hello.`: one `assistant_message_delta`, as this file's `hello`
/// streams one text fragment.
fn search_call_kinds() -> Vec<&'static str> {
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
        "permission_resolved",
        "tool_call_started",
        "tool_call_completed",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]
}

/// The complete event kinds of a run whose only reply is `hello`, with
/// `extra` notices between `extensions_loaded` and `preamble_built`.
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
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]);
    kinds
}

/// The complete event kinds of a resumed run whose only reply is `hello`:
/// no `session_started` and no `opening_message`, as the log holds both.
fn resumed_hello_kinds() -> Vec<&'static str> {
    vec![
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
        "turn_started",
        "step_started",
        "assistant_message_started",
        "assistant_message_delta",
        "text_completed",
        "usage_recorded",
        "assistant_message_completed",
        "turn_completed",
        "fiber_exited",
    ]
}

/// Asserts two strings hold the same bytes, reporting only the lengths
/// and the first differing offset: both sides can be tens of kilobytes,
/// which `assert_eq!` would print whole into the CI log
/// (`docs/testing.md`, "What a test asserts").
fn assert_str_eq(expected: &str, actual: &str, what: &str) {
    let offset = expected
        .bytes()
        .zip(actual.bytes())
        .position(|(a, b)| a != b);
    let same = expected.len() == actual.len() && offset.is_none();
    assert!(
        same,
        "{what}: lengths {} vs {}, first difference at {}",
        expected.len(),
        actual.len(),
        offset.unwrap_or(expected.len().min(actual.len()))
    );
}

#[test]
fn one_backend_runs_the_search_and_writes_its_results() {
    let setup = Setup::new();
    let server = calling(&[function_call(
        "call_search",
        "web_search",
        &json!({"query": "rust", "allowed_domains": ["rust-lang.org"]}),
    )]);
    setup.provider(&server);
    echo_setup("searcher", "brave", "T", &setup);
    let run = ask(&setup, "search the web for rust");
    assert_eq!(run.kinds(), search_call_kinds());

    let tools = sent_tools(&server, 0);
    let declared = tools
        .iter()
        .find(|tool| tool["name"] == "web_search")
        .expect("a web_search function in the request");
    assert_eq!(declared["type"], "function");
    assert_eq!(
        declared["parameters"]["properties"]["query"]["minLength"],
        json!(1)
    );
    assert_eq!(
        declared["parameters"]["required"],
        json!(["query"]),
        "the backend schema"
    );
    assert!(
        tools.iter().all(|tool| tool["type"] != "web_search"),
        "no hosted search: {tools:?}"
    );
    let row = run.sent_tool("web_search");
    assert_eq!(row["registered_by"], "builtin");
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    let rendered = "1. T 1\nhttps://example.com/1\n{\"allowed_domains\":[\"rust-lang.org\"],\"query\":\"rust\"}";
    assert_eq!(
        completed["content"],
        json!([{"type": "text", "text": rendered}])
    );
    assert_eq!(outputs_sent(&server), [rendered]);
}

#[test]
fn a_hosted_search_stands_over_an_installed_backend() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    install_hosted_model(&setup, &server);
    setup.lua("searcher", &echo_backend("brave", "T"));
    let run = ask(&setup, "search the web for rust");
    assert_eq!(run.kinds(), hello_kinds(&[]));

    let tools = sent_tools(&server, 0);
    assert!(
        tools.contains(&json!({"type": "web_search"})),
        "the hosted search is sent: {tools:?}"
    );
    assert!(
        tools
            .iter()
            .all(|tool| tool.get("name").is_none_or(|name| name != "web_search")),
        "no web_search function: {tools:?}"
    );
    let row = run.sent_tool("web_search");
    assert_eq!(row["definition"], json!({"type": "web_search"}));
}

/// Installs a provider `fake` with `r` on `openai-responses` declaring the
/// hosted search `web_search`, and makes it the configured model.
fn install_hosted_model(setup: &Setup, server: &ProviderServer) {
    let source = setup.root.path().join("src");
    write_json(
        &source.join("extension.json"),
        &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1}),
    );
    write_json(
        &source.join("providers/fake.json"),
        &json!({
            "name": "fake",
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "r", "protocol": "openai-responses",
                        "base_url": format!("{}/v1", server.url()),
                        "context_window": 100000, "web_search": "web_search"}],
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
    write_json(
        &setup.home().join("config.json"),
        &json!({"model": "fake/r"}),
    );
}

fn write_json(file: &std::path::Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

#[test]
fn two_backends_with_no_setting_declare_nothing_and_name_the_setting() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.lua("one", &echo_backend("one", "one"));
    setup.lua("two", &echo_backend("two", "two"));
    let run = ask(&setup, "search the web for rust");
    assert_eq!(run.kinds(), hello_kinds(&["notice"]));

    let tools = sent_tools(&server, 0);
    assert!(
        tools
            .iter()
            .all(|tool| tool.get("name").is_none_or(|name| name != "web_search")),
        "no web_search is declared: {tools:?}"
    );
    let notices = run.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].0, "web_search_unavailable");
    assert!(
        notices[0].1.contains("web_search.backend"),
        "{}",
        notices[0].1
    );
}

#[test]
fn two_backends_with_the_setting_run_the_named_one() {
    let setup = Setup::new();
    let server = calling(&[function_call(
        "call_search",
        "web_search",
        &json!({"query": "rust"}),
    )]);
    setup.provider(&server);
    setup.lua("one", &echo_backend("one", "one"));
    setup.lua("two", &echo_backend("two", "two"));
    allow_web_search(&setup);
    write_config(
        &setup,
        &json!({"model": "fake/m", "web_search": {"backend": "two"}}),
    );
    let run = ask(&setup, "search the web for rust");
    assert_eq!(run.kinds(), search_call_kinds());

    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    let text = completed["content"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("1. two 1\n"),
        "the named backend ran: {text}"
    );
}

#[test]
fn a_setting_naming_a_missing_backend_declares_nothing_and_names_it() {
    let setup = Setup::new();
    let server = ProviderServer::start([hello()]).unwrap();
    setup.provider(&server);
    setup.lua("searcher", &echo_backend("brave", "T"));
    write_config(
        &setup,
        &json!({"model": "fake/m", "web_search": {"backend": "missing"}}),
    );
    let run = ask(&setup, "search the web for rust");
    assert_eq!(run.kinds(), hello_kinds(&["notice"]));

    let tools = sent_tools(&server, 0);
    assert!(
        tools
            .iter()
            .all(|tool| tool.get("name").is_none_or(|name| name != "web_search")),
        "no web_search is declared: {tools:?}"
    );
    let notices = run.notices();
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(notices[0].0, "web_search_unavailable");
    assert!(notices[0].1.contains("missing"), "{}", notices[0].1);
}

#[test]
fn a_backend_past_its_timeout_fails_and_the_turn_ends() {
    let setup = Setup::new();
    let server = calling(&[function_call(
        "call_search",
        "web_search",
        &json!({"query": "rust"}),
    )]);
    setup.provider(&server);
    setup.lua(
        "searcher",
        "fiber.search_backend(\"brave\", { timeout = 50, run = function() while true do end end })\n",
    );
    allow_web_search(&setup);
    let run = ask(&setup, "search the web for rust");
    assert_eq!(run.kinds(), search_call_kinds());

    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "failed");
    assert_eq!(completed["error"]["code"], "timeout");
}

#[test]
fn a_result_past_the_default_cap_is_cut_to_its_start() {
    let setup = Setup::new();
    let server = calling(&[function_call(
        "call_search",
        "web_search",
        &json!({"query": "rust"}),
    )]);
    setup.provider(&server);
    setup.lua(
        "searcher",
        "fiber.search_backend(\"brave\", { timeout = 10000, run = function() \
           local results = {} \
           for i = 1, 200 do \
             results[i] = { title = \"T \" .. i, url = \"https://example.com/\" .. i, \
             snippet = string.rep(\"s\", 200) } \
           end \
           return results end })\n",
    );
    allow_web_search(&setup);
    let run = ask(&setup, "search the web for rust");
    assert_eq!(run.kinds(), search_call_kinds());

    let mut full = String::new();
    for i in 1..=200 {
        if i > 1 {
            full.push_str("\n\n");
        }
        full.push_str(&format!(
            "{i}. T {i}\nhttps://example.com/{i}\n{}",
            "s".repeat(200)
        ));
    }
    assert!(full.len() > 16_384, "the result passes the default cap");
    let completed = run.payload("tool_call_completed");
    assert_eq!(completed["status"], "completed");
    let completed_line = run.all("tool_call_completed");
    assert_eq!(completed_line.len(), 1);
    let action = completed_line[0]["action_id"].as_str().unwrap();
    let artifact = format!("artifacts/{action}.txt");
    assert_eq!(completed["artifact"], artifact.as_str());
    let dir = run.session_dir(&setup);
    let path = dir.join(&artifact);
    assert_str_eq(
        &full,
        &fs::read_to_string(&path).unwrap(),
        "the artifact holds the whole result",
    );
    assert_str_eq(
        &format!(
            "{}\n[{} bytes cut. The full output is in {}; read it with `read`.]",
            &full[..16_384],
            full.len() - 16_384,
            path.display()
        ),
        completed["content"][0]["text"].as_str().unwrap(),
        "the completed content keeps the start",
    );
}

#[test]
fn a_resumed_session_lists_the_backend_search() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_search",
            "web_search",
            &json!({"query": "rust"}),
        )]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    echo_setup("searcher", "brave", "T", &setup);
    let first = ask(&setup, "search the web for rust");
    assert_eq!(first.kinds(), search_call_kinds());
    let id = first.session_id().to_owned();

    let second = run_fiber(&setup, &["ask", "--resume", &id, "two"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.kinds(), resumed_hello_kinds());
    let row = second.sent_tool("web_search");
    assert_eq!(row["registered_by"], "builtin");
}
