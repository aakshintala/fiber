//! Binary-level tests of configured `tools."<name>".max_result_bytes`
//! (`docs/testing.md`, "Levels"): the built `fiber` runs in its own process
//! group with its own `FIBER_HOME`, holding an ordinary provider whose base
//! URL is the fake server.

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
use std::process::Output;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Setup, hello, run_to_exit, stream};

/// A `session_status` line: ephemeral, and written by an observer thread, so
/// where it falls among the loop's own lines is not what these tests pin.
/// A `tool_call_delta` line is ephemeral too, and its count depends on
/// timing (`docs/tools.md`, "Progress").
fn is_ephemeral(line: &str) -> bool {
    line.contains(r#""kind":"session_status""#) || line.contains(r#""kind":"tool_call_delta""#)
}

/// One finished run: its exit code, stdout's lines, as text and parsed, and
/// stderr.
struct Run {
    code: Option<i32>,
    raw: Vec<String>,
    lines: Vec<Value>,
    stderr: String,
}

impl From<Output> for Run {
    fn from(output: Output) -> Self {
        let stdout = String::from_utf8(output.stdout).unwrap();
        let raw: Vec<String> = stdout
            .lines()
            .filter(|l| !is_ephemeral(l))
            .map(str::to_owned)
            .collect();
        let lines = raw
            .iter()
            .map(|l| serde_json::from_str(l).unwrap_or(Value::Null))
            .collect();
        Self {
            code: output.status.code(),
            raw,
            lines,
            stderr: String::from_utf8(output.stderr).unwrap(),
        }
    }
}

impl Run {
    fn kinds(&self) -> Vec<&str> {
        self.lines
            .iter()
            .map(|l| l["kind"].as_str().unwrap())
            .collect()
    }

    fn session_id(&self) -> &str {
        self.lines[0]["session_id"].as_str().unwrap()
    }

    /// The session's directory, from its id.
    fn session_dir(&self, setup: &Setup) -> PathBuf {
        let workspace = fs::canonicalize(setup.root.path()).unwrap();
        let key = workspace.to_string_lossy().replace('/', "-");
        setup
            .home()
            .join("projects")
            .join(key)
            .join("sessions")
            .join(self.session_id())
    }

    /// Stdout's durable lines for its session, one per line with the newline.
    fn durable(&self) -> String {
        self.raw
            .iter()
            .zip(&self.lines)
            .filter(|(_, l)| {
                l.get("seq").is_some() && l["session_id"] == self.lines[0]["session_id"]
            })
            .map(|(raw, _)| format!("{raw}\n"))
            .collect()
    }
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

/// The event kinds of a turn whose first reply calls one tool and whose
/// second is `hello`. A reads-only workspace call is a fast path, so no
/// `permission_` line is written (`docs/permissions.md`, "Fast paths").
fn tool_kinds() -> Vec<&'static str> {
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

/// The event kinds of a resumed turn whose first reply calls one tool and
/// whose second is `hello`: as [`tool_kinds`], without `session_started`
/// and `opening_message`.
fn resumed_tool_kinds() -> Vec<&'static str> {
    let mut kinds = vec![
        "fiber_started",
        "extensions_loaded",
        "preamble_built",
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

/// 300 bytes of `a`–`z` repeating with no newline.
fn long_bytes() -> String {
    (0..300)
        .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap()))
        .collect()
}

/// The shell's result text for `cat long.txt`: the 300 bytes, a newline,
/// then the exit line.
fn full_text() -> String {
    format!("{}\nExit code 0.\n", long_bytes())
}

fn write_config(setup: &Setup, tools: Value) {
    let config = json!({"model": "fake/m", "tools": tools});
    fs::write(setup.home().join("config.json"), config.to_string()).unwrap();
}

#[test]
fn a_new_session_cuts_a_shell_result_to_its_configured_cap() {
    let setup = Setup::new();
    fs::write(setup.root.path().join("long.txt"), long_bytes()).unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_shell",
            "shell",
            &json!({"command": "cat long.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    write_config(&setup, json!({"shell": {"max_result_bytes": 100}}));

    let output = run_to_exit("fiber ask", setup.fiber(&["ask", "go"]));
    let run = Run::from(output);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), tool_kinds());
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    let full = full_text();
    assert_eq!(full.len(), 314);
    let action = completed["action_id"].as_str().unwrap();
    let artifact = format!("artifacts/{action}.txt");
    assert_eq!(completed["payload"]["artifact"], artifact.as_str());
    let dir = run.session_dir(&setup);
    let path = dir.join(&artifact);
    assert_eq!(fs::read_to_string(&path).unwrap(), full);
    assert_eq!(
        completed["payload"]["content"][0]["text"],
        format!(
            "{}\n[214 bytes cut. The full output is in {}; read it with `read`.]\n{}",
            &full[..50],
            path.display(),
            &full[264..]
        )
    );
    let durable = run.durable();
    assert_eq!(
        fs::read_to_string(dir.join("events.jsonl")).unwrap(),
        durable
    );
}

#[test]
fn a_resumed_session_cuts_a_shell_result_to_its_configured_cap() {
    let setup = Setup::new();
    fs::write(setup.root.path().join("long.txt"), long_bytes()).unwrap();
    let server = ProviderServer::start([
        hello(),
        stream(&[function_call(
            "call_shell",
            "shell",
            &json!({"command": "cat long.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    let first = Run::from(run_to_exit("fiber ask", setup.fiber(&["ask", "one"])));
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let id = first.session_id().to_owned();

    write_config(&setup, json!({"shell": {"max_result_bytes": 100}}));
    let second = Run::from(run_to_exit(
        "fiber ask --resume",
        setup.fiber(&["ask", "--resume", &id, "go"]),
    ));
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    assert_eq!(second.kinds(), resumed_tool_kinds());

    let completed = second
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    let full = full_text();
    let action = completed["action_id"].as_str().unwrap();
    let artifact = format!("artifacts/{action}.txt");
    assert_eq!(completed["payload"]["artifact"], artifact.as_str());
    let dir = second.session_dir(&setup);
    let path = dir.join(&artifact);
    assert_eq!(fs::read_to_string(&path).unwrap(), full);
    assert_eq!(
        completed["payload"]["content"][0]["text"],
        format!(
            "{}\n[214 bytes cut. The full output is in {}; read it with `read`.]\n{}",
            &full[..50],
            path.display(),
            &full[264..]
        )
    );
    let log = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(log.starts_with(&first.durable()));
    assert_eq!(&log[first.durable().len()..], &second.durable());
    let seqs: Vec<u64> = log
        .lines()
        .map(|l| {
            serde_json::from_str::<Value>(l).unwrap()["seq"]
                .as_u64()
                .unwrap()
        })
        .collect();
    assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());
}

#[test]
fn a_new_session_cuts_a_read_at_its_configured_cap_with_no_artifact() {
    let setup = Setup::new();
    let line = format!("{}\n", "a".repeat(63));
    fs::write(setup.root.path().join("lines.txt"), line.repeat(3)).unwrap();
    let server = ProviderServer::start([
        stream(&[function_call(
            "call_read",
            "read",
            &json!({"path": "lines.txt"}),
        )]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    write_config(&setup, json!({"read": {"max_result_bytes": 100}}));

    let output = run_to_exit("fiber ask", setup.fiber(&["ask", "go"]));
    let run = Run::from(output);

    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert_eq!(run.kinds(), tool_kinds());
    let completed = run
        .lines
        .iter()
        .find(|line| line["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(
        completed["payload"]["content"][0]["text"],
        format!("{line}[Showing lines 1-1 of 3. Continue with offset=2.]")
    );
    assert_eq!(completed["payload"].get("artifact"), None);
    let dir = run.session_dir(&setup);
    let artifacts = dir.join("artifacts");
    assert!(
        !artifacts.is_dir() || fs::read_dir(&artifacts).unwrap().next().is_none(),
        "a cut read writes no artifact"
    );
    let durable = run.durable();
    assert_eq!(
        fs::read_to_string(dir.join("events.jsonl")).unwrap(),
        durable
    );
}
