//! The `alloc-stats` build's per-operation allocation fields at the binary
//! level (`docs/state.md`, "Diagnostic logs"): the built `fiber` runs
//! `ask` against the fake provider server with the read tool. At
//! `diagnostics.level: debug` the same-thread `tool.run` `op_timing` line
//! carries numeric `alloc_bytes`, `allocs` and `live_delta_bytes` beside
//! `op`, `ms` and `tool`.

#![cfg(feature = "alloc-stats")]
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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{KillGroup, Run, Setup, function_call, hello, is_status, spawn_watched, stream};

// Parses stdout into one JSON value per line, skipping status lines.
// This file's copy; every binary test file carries its own.
impl From<std::process::Output> for Run {
    fn from(output: std::process::Output) -> Self {
        let lines = String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|l| !is_status(l))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        Self {
            code: output.status.code(),
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Setup {
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

    /// Installs a provider `fake` with model `m` on `openai-responses` at the
    /// fake server, and writes configuration naming `fake/m` with `extra`.
    fn provider_with_extra(&self, server: &ProviderServer, extra: &Value) {
        let source = self.root.path().join("src").join("fake");
        write(
            &source.join("extension.json"),
            &json!({"name": "fake", "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {"fake": [format!("{}/v1", server.url())]}}).to_string(),
        );
        write(
            &source.join("providers/fake.json"),
            &json!({
                "name": "fake",
                "credential": {"env": "FIBER_TEST_FAKE_KEY"},
                "models": [{"id": "m", "protocol": "openai-responses", "base_url": format!("{}/v1", server.url()), "context_window": 100000}]
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

    /// Runs `fiber` in its own process group, waits for it under the
    /// test's [`Deadline`], and asserts that nothing it started is left in the
    /// group. A watchdog beside it kills that group if this process dies
    /// first.
    #[track_caller]
    fn run(&self, args: &[&str]) -> Run {
        self.run_with_env(args, &[])
    }

    /// [`Self::run`] with extra environment `env`.
    #[track_caller]
    fn run_with_env(&self, args: &[&str], env: &[(&str, &str)]) -> Run {
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
        for (key, value) in env {
            command.env(key, value);
        }
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
            !fakes::group_alive(group),
            "`fiber` left a process in its group behind"
        );
        std::mem::forget(guard);
        watchdog.stand_down(self.deadline.cleanup_phase());
        Run::from(output)
    }

    /// Writes `text` to `note.txt` in the workspace, starts a server whose
    /// model reads it and then answers, and runs `fiber ask`.
    #[track_caller]
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
        self.provider_with_extra(&server, config);
        let run = self.run(&["ask", "read the note"]);
        assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
        (run, server)
    }
}

fn write(file: &Path, text: &str) {
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, text).unwrap();
}

/// Every diagnostic line under the home's `logs/`, parsed.
fn log_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    for entry in fs::read_dir(&logs).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            lines.push(serde_json::from_str(line).unwrap());
        }
    }
    lines
}

/// Every `op_timing` line, in file order.
fn op_lines(setup: &Setup) -> Vec<Value> {
    log_lines(setup)
        .into_iter()
        .filter(|line| line["code"] == "op_timing")
        .collect()
}

fn ops(setup: &Setup, op: &str) -> Vec<Value> {
    op_lines(setup)
        .into_iter()
        .filter(|line| line["data"]["op"] == op)
        .collect()
}

/// At `debug` the `tool.run` line of a run whose model reads the note
/// carries the `alloc-stats` fields: numeric `alloc_bytes`, `allocs` and
/// `live_delta_bytes` beside `op`, `ms` and `tool`.
#[test]
fn a_debug_tool_run_line_carries_alloc_fields() {
    let setup = Setup::new();
    let (_run, _server) = setup.read_note(
        "the note says hi\n",
        &json!({"diagnostics": {"level": "debug"}}),
    );
    let runs = ops(&setup, "tool.run");
    assert_eq!(runs.len(), 1, "{:?}", op_lines(&setup));
    let line = &runs[0];
    assert_eq!(line["data"]["op"], "tool.run", "{line}");
    assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
    assert_eq!(line["data"]["tool"], "read", "{line}");
    assert!(
        line["data"]["alloc_bytes"].as_u64().is_some(),
        "numeric alloc_bytes: {line}"
    );
    assert!(
        line["data"]["allocs"].as_u64().is_some(),
        "numeric allocs: {line}"
    );
    assert!(
        line["data"]["live_delta_bytes"].is_i64(),
        "numeric live_delta_bytes: {line}"
    );
}
