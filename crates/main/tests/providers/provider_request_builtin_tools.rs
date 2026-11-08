//! Gemini `openai-completions` bodies over the full built-in tool set
//! (`docs/testing.md`, "Levels"): the built binary against the fake provider
//! server, once with `compat.gemini` and once without, both over the live
//! registration — no literal list of tool names.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use crate::support;

use std::fs;
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response};
use serde_json::{Value, json};
use support::{KillGroup, Run, Setup, is_status, spawn_watched};

impl Setup {
    /// Installs a provider `fake` with a Gemini `gem` and a plain `plain`
    /// model, both `openai-completions` at the fake server, and makes
    /// `fake/gem` the configured model; the test picks per run with
    /// `--model`.
    fn provider_with_gemini(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write_file(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        write_file(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [
                    {"id": "gem", "protocol": "openai-completions",
                     "base_url": format!("{}/v1", server.url()),
                     "context_window": 100000, "compat": {"gemini": true}},
                    {"id": "plain", "protocol": "openai-completions",
                     "base_url": format!("{}/v1", server.url()),
                     "context_window": 100000},
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
        write_file(
            &self.home().join("config.json"),
            &json!({"model": "fake/gem"}),
        );
    }

    /// Runs `fiber ask --model <model> hi` in its own process group, waits
    /// for it under the test's [`Deadline`], and asserts that nothing it
    /// started is left in the group (`docs/testing.md`, "Running tests").
    #[track_caller]
    fn ask_model(&self, model: &str) -> Run {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fiber"));
        command
            .args(["ask", "--model", model, "hi"])
            .current_dir(self.root.path().join("w"))
            .env_clear()
            .envs(fakes::check_run())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.root.path())
            .env("FIBER_HOME", self.home())
            .env("FIBER_TEST_FAKE_KEY", "sk-test")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let (mut child, watchdog) = spawn_watched(&mut command);
        let group = child.id();
        let guard = KillGroup(group);
        drop(child.stdin.take());
        let (done, finished) = mpsc::channel();
        thread::spawn(move || done.send(child.wait_with_output()).unwrap());
        let output = match self.deadline.recv(&finished) {
            Ok(output) => output.unwrap(),
            Err(_) => support::expired(self.deadline, group, &finished, "`fiber ask` to exit"),
        };
        assert!(
            !fakes::group_alive(group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        run_from(output)
    }
}

fn write_file(file: &std::path::Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

/// One finished run: its exit code and stdout's lines, parsed.
fn run_from(output: Output) -> Run {
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// An `openai-completions` stream answering `Hello.` in two fragments.
fn completions_hello() -> Response {
    let chunks = [
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [
            {"index": 0, "delta": {"role": "assistant", "content": "Hel"}}]}),
        json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "choices": [
            {"index": 0, "delta": {"content": "lo."}, "finish_reason": "stop"}]}),
        json!({"id": "chatcmpl-1", "choices": [], "usage": {"prompt_tokens": 15,
            "completion_tokens": 9, "total_tokens": 24,
            "prompt_tokens_details": {"cached_tokens": 14}}}),
    ];
    let mut body: String = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    Response::stream(body)
}

/// Every remaining nullable `type` array in `value`, as dotted paths: an
/// object inside an `enum`, `const`, `default` or `examples` value is data,
/// not a schema, so the walk skips those values, mirroring the rewrite.
fn null_unions(value: &Value, path: &mut String, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if map
                .get("type")
                .and_then(Value::as_array)
                .is_some_and(|types| types.iter().any(|t| t == "null"))
            {
                found.push(path.clone());
            }
            for (key, child) in map {
                if matches!(key.as_str(), "enum" | "const" | "default" | "examples") {
                    continue;
                }
                let mut child_path = format!("{path}.{key}");
                null_unions(child, &mut child_path, found);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                let mut item_path = format!("{path}[{i}]");
                null_unions(item, &mut item_path, found);
            }
        }
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => {}
    }
}

#[test]
fn gemini_full_tool_set_body_is_rewritten_without_either_key() {
    let setup = Setup::new();
    let server = ProviderServer::start([completions_hello(), completions_hello()]).unwrap();
    setup.provider_with_gemini(&server);
    let gemini = setup.ask_model("fake/gem");
    assert_eq!(gemini.code, Some(0), "stderr: {}", gemini.stderr);
    let plain = setup.ask_model("fake/plain");
    assert_eq!(plain.code, Some(0), "stderr: {}", plain.stderr);
    let requests = server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let gemini_body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    let plain_body: Value = serde_json::from_slice(&requests[1].body).unwrap();

    assert_eq!(gemini_body.get("prompt_cache_key"), None, "{gemini_body}");
    assert_eq!(gemini_body.get("stream_options"), None, "{gemini_body}");
    let gemini_tools = gemini_body["tools"].as_array().unwrap();
    assert!(!gemini_tools.is_empty(), "{gemini_body}");
    // No `web_search` backend is installed and neither model declares a
    // hosted search, so the hosted search stays absent and every tool is a
    // local function; the set itself is the live registration, looked up by
    // name, never listed.
    let by_name = |tools: &[Value], name: &str| {
        tools
            .iter()
            .find(|tool| tool["function"]["name"] == name)
            .unwrap_or_else(|| panic!("no {name} tool: {tools:?}"))
            .clone()
    };
    for tool in gemini_tools {
        let name = tool["function"]["name"]
            .as_str()
            .unwrap_or_else(|| panic!("a local function: {tool}"));
        assert_ne!(name, "web_search");
    }
    // The `read`/`shell` entries exercise their real null-union shapes
    // through the live set.
    let read = by_name(gemini_tools, "read");
    assert_eq!(
        read["function"]["parameters"]["properties"]["offset"]["type"],
        "integer"
    );
    assert_eq!(
        read["function"]["parameters"]["properties"]["offset"]["nullable"],
        true
    );
    let shell = by_name(gemini_tools, "shell");
    assert_eq!(
        shell["function"]["parameters"]["properties"]["workdir"]["type"],
        "string"
    );
    assert_eq!(
        shell["function"]["parameters"]["properties"]["workdir"]["nullable"],
        true
    );
    let mut remaining = Vec::new();
    for tool in gemini_tools {
        let mut path = tool["function"]["name"]
            .as_str()
            .unwrap_or("hosted")
            .to_owned();
        null_unions(&tool["function"]["parameters"], &mut path, &mut remaining);
    }
    assert_eq!(remaining, Vec::<String>::new(), "unrewritten unions");

    // Without the flag the same live set goes over exactly as declared.
    assert!(plain_body.get("prompt_cache_key").is_some(), "{plain_body}");
    assert_eq!(plain_body["stream_options"], json!({"include_usage": true}));
    let plain_tools = plain_body["tools"].as_array().unwrap();
    assert!(!plain_tools.is_empty(), "{plain_body}");
    let plain_read = by_name(plain_tools, "read");
    assert_eq!(
        plain_read["function"]["parameters"]["properties"]["offset"]["type"],
        json!(["integer", "null"])
    );
    let mut kept = Vec::new();
    for tool in plain_tools {
        let mut path = tool["function"]["name"]
            .as_str()
            .unwrap_or("hosted")
            .to_owned();
        null_unions(&tool["function"]["parameters"], &mut path, &mut kept);
    }
    assert!(
        !kept.is_empty(),
        "the live set carries a null union: {plain_body}"
    );
}
