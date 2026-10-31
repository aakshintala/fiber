//! A timed-out MCP call sends its error message as the tool result text.

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
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::{ProviderServer, Response, Watchdog};
use serde_json::{Value, json};
use support::{Deadline, function_call, hello, stream};

struct Setup {
    root: fakes::TempDir,
    deadline: Deadline,
    #[allow(dead_code, reason = "held only for its drop")]
    guard: Watchdog,
}

impl Setup {
    fn new_with_mcp_fixture() -> Self {
        fakes::mcp_fixture();
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

    fn provider(&self, server: &ProviderServer) {
        let source = self.root.path().join("src");
        write_json(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}),
        );
        write_json(
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
        write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m"}),
        );
    }

    fn configure(&self, servers: &Value) {
        write_json(
            &self.home().join("config.json"),
            &json!({"model": "fake/m", "mcp": {"servers": servers}}),
        );
    }

    fn fixture(&self, tools: &Value, calls: &[(&str, &str)]) -> PathBuf {
        let dir = self.root.path().join("fx");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tools.json"), tools.to_string()).unwrap();
        for (name, body) in calls {
            fs::write(dir.join(name), body).unwrap();
        }
        dir
    }

    fn run(&self, args: &[&str]) -> Run {
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
        let (child, watchdog) = support::spawn_watched(&mut command);
        let group = child.id();
        let guard = support::KillGroup(group);
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

fn write_json(file: &Path, value: &Value) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, value.to_string()).unwrap();
}

struct Run {
    code: Option<i32>,
    lines: Vec<Value>,
    stderr: String,
}

impl Run {
    fn new(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines = stdout
            .lines()
            .filter(|l| !support::is_status(l))
            .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

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

fn tool_output_of(body: &[u8]) -> Option<String> {
    let parsed: Value = serde_json::from_slice(body).ok()?;
    parsed["input"].as_array()?.iter().find_map(|item| {
        if item["type"] != "function_call_output" {
            return None;
        }
        if let Value::String(text) = &item["output"] {
            Some(text.clone())
        } else {
            Some(item["output"].to_string())
        }
    })
}

#[test]
fn a_timed_out_call_sends_its_error_message_as_the_tool_result() {
    let setup = Setup::new_with_mcp_fixture();
    let dir = setup.fixture(
        &json!([{"name": "hang", "annotations": {"readOnlyHint": true, "openWorldHint": false}}]),
        &[("call-hang.json", "hang")],
    );
    let server = ProviderServer::start_responding(|request| {
        if tool_output_of(&request.body).is_none() {
            return stream(&[function_call("call_hang", "mcp__fx__hang", &json!({}))]);
        }
        let output = tool_output_of(&request.body).unwrap_or_default();
        if output.is_empty() {
            return Response::status(
                400,
                json!({"error": {"message": "messages.164.content.0.tool_result: content cannot be empty if is_error is true"}})
                    .to_string(),
            );
        }
        hello()
    })
    .unwrap();
    setup.provider(&server);
    configure_fx(&setup, &dir, json!({"timeout_ms": 200}));

    let run = setup.run(&["ask", "hang"]);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    let requests = server.requests();
    assert!(requests.len() >= 2, "expected two model requests");
    let output = tool_output_of(&requests[1].body).unwrap_or_default();
    assert!(
        !output.is_empty(),
        "the timed-out result reached the provider empty: 400 messages.164.content.0.tool_result: content cannot be empty if is_error is true"
    );
    assert!(
        output.contains("did not answer"),
        "the timed-out result carries the timeout message, got: {output}"
    );
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(completed["payload"]["status"], "failed");
    assert_eq!(completed["payload"]["error"]["code"], "timeout");
}
