//! A timed-out MCP call sends its error message as the tool result text,
//! on the Anthropic protocol (`docs/testing.md`, "Levels"): the built
//! `fiber` runs in its own process group with its own `FIBER_HOME`,
//! holding an ordinary provider whose base URL is the fake server, and a
//! configured fixture server whose tool never answers.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{KillGroup, Setup, StdoutRun, write_json};

impl Setup {
    /// Installs a provider `fake` with model `m` on `protocol` at the fake
    /// server, and makes `fake/m` the configured model.
    fn provider_on(&self, server: &ProviderServer, protocol: &str) {
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        let model = json!({"id": "m", "protocol": protocol,
            "base_url": format!("{}/v1", server.url()), "context_window": 100000,
            "input": ["text", "image"]});
        write_json(
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
        write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    /// Adds `servers` under `mcp.servers` to the global configuration,
    /// keeping the configured model.
    fn configure(&self, servers: &Value) {
        write_json(
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

    /// Runs `fiber` in its own process group, waits for it under the test's
    /// [`Deadline`], and asserts that nothing it started is left in the
    /// group, after a timeout too (`docs/testing.md`, "Running tests").
    /// A watchdog beside it kills that group if this process dies first.
    #[track_caller]
    fn run(&self, args: &[&str]) -> StdoutRun {
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
        let (child, watchdog) = support::spawn_watched(&mut command);
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
        StdoutRun::from(output)
    }
}

/// Configures one fixture server `fx` in `dir` and returns nothing.
fn configure_fx(setup: &Setup, dir: &Path, extra: Value) {
    let mut server = json!({
        "command": "/bin/bash",
        "args": [fakes::mcp_fixture().display().to_string(), dir.display().to_string()],
        "startup_timeout_ms": 600000,
    });
    if let (Some(object), Value::Object(fields)) = (server.as_object_mut(), extra) {
        for (key, value) in fields {
            object.insert(key, value);
        }
    }
    setup.configure(&json!({"fx": server}));
}

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

/// An `anthropic-messages` reply calling `mcp__fx__hang` with no arguments.
fn anthropic_hang() -> Response {
    anthropic(&[
        json!({"type": "content_block_start", "index": 0, "content_block": {
            "type": "tool_use", "id": "toolu_1", "name": "mcp__fx__hang", "input": {}}}),
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

/// The `tool_result` blocks the fake provider received, in arrival order.
fn tool_results(server: &ProviderServer) -> Vec<Value> {
    server
        .requests()
        .iter()
        .filter_map(|request| {
            let body: Value = serde_json::from_slice(&request.body).ok()?;
            body["messages"]
                .as_array()?
                .iter()
                .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
                .find(|block| block["type"] == "tool_result")
        })
        .collect()
}

/// Whether the error result `block` carries no text: what the provider
/// answers with HTTP 400 `content cannot be empty if is_error is true`.
fn is_empty_error(block: &Value) -> bool {
    block["is_error"] == json!(true)
        && (block["content"] == json!("") || block["content"] == json!([]))
}

#[test]
fn a_timed_out_call_sends_its_error_message_as_the_tool_result() {
    let setup = Setup::new_with_mcp_fixture();
    let _guard = Watchdog::matching(&setup.root.path().display().to_string());
    let dir = setup.fixture(
        &json!([{"name": "hang", "annotations": {"readOnlyHint": true, "openWorldHint": false}}]),
        &[("call-hang.json", "hang")],
    );
    let server = ProviderServer::start_responding(|request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let result = body["messages"]
            .as_array()
            .into_iter()
            .flat_map(|messages| messages.iter())
            .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
            .find(|block| block["type"] == "tool_result");
        match result {
            None => anthropic_hang(),
            Some(block) if is_empty_error(&block) => Response::status(
                400,
                json!({"type": "error", "error": {"type": "invalid_request_error",
                    "message": "messages.164.content.0.tool_result: content cannot be empty if is_error is true"}})
                    .to_string(),
            ),
            Some(_) => anthropic_hello(),
        }
    })
    .unwrap();
    setup.provider_on(&server, "anthropic-messages");
    configure_fx(&setup, &dir, json!({"timeout_ms": 200}));

    let run = setup.run(&["ask", "hang"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let result = tool_results(&server)
        .into_iter()
        .find(|block| block["is_error"] == json!(true))
        .expect("the timed-out result reached the provider");
    assert_eq!(result["is_error"], json!(true));
    let content = result["content"].as_str().unwrap();
    assert!(
        !content.is_empty(),
        "the timed-out result reached the provider empty: 400 messages.164.content.0.tool_result: content cannot be empty if is_error is true"
    );
    assert!(
        content.contains("did not answer"),
        "the timed-out result carries the timeout message, got: {content}"
    );
    let completed = run.first("tool_call_completed");
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "timeout");
}
