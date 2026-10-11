//! Binary-level tests of the `name_session` tool (`docs/tools.md`,
//! "Naming the session"; `docs/testing.md`, "Levels"): the built `fiber`
//! runs `fiber ask` in its own process group with its own `FIBER_HOME`,
//! with the fake provider.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use contract::SessionId;
use contract::events::{Event, NamedBy, SessionNamed};
use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{
    Setup, SystemClock, function_call, hello, is_status, read_kinds, run_to_exit, stream,
    tool_names,
};

/// A finished `function_call` for `name_session` with `arguments`.
fn name_call(call_id: &str, arguments: &Value) -> Value {
    function_call(call_id, "name_session", arguments)
}

/// Runs `fiber ask` with a model that answers `responses`, then returns
/// stdout's lines.
fn ask_with(setup: &Setup, prompt: &str) -> Vec<Value> {
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", prompt]));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect()
}

/// The session's directory: whichever project holds `id`.
fn session_dir(setup: &Setup, id: &str) -> std::path::PathBuf {
    let projects = setup.home().join("projects");
    let mut stack = vec![projects];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().unwrap() == id {
                    return path;
                }
                stack.push(path);
            }
        }
    }
    panic!("no session dir for {id}");
}

/// Reads `events.jsonl` in `dir` as one JSON value per line.
fn read_lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The session id stdout's first line names.
fn session_id(lines: &[Value]) -> &str {
    lines[0]["session_id"].as_str().unwrap()
}

#[test]
fn naming_writes_session_named_before_the_completion() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[name_call("c1", &json!({"name": "Fix the parser"}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let lines = ask_with(&setup, "go");

    let mut expected = read_kinds();
    let at = expected
        .iter()
        .position(|k| *k == "tool_call_completed")
        .unwrap();
    expected.insert(at, "session_named");
    assert_eq!(support::kinds(&lines), expected);

    let log = read_lines(&session_dir(&setup, session_id(&lines)).join("events.jsonl"));
    let named = log.iter().find(|l| l["kind"] == "session_named").unwrap();
    let completed = log
        .iter()
        .find(|l| l["kind"] == "tool_call_completed")
        .unwrap();
    assert_eq!(
        named["payload"],
        json!({"name": "Fix the parser", "by": "model"})
    );
    assert_eq!(named["action_id"], completed["action_id"]);
    assert_eq!(completed["payload"]["status"], "completed");
    assert_eq!(completed["payload"]["content"], json!([]));
    let started = log
        .iter()
        .find(|l| l["kind"] == "tool_call_started")
        .unwrap();
    assert_eq!(started["payload"]["effects"], json!([]));

    let body = &server.requests()[0].body;
    let tools: Value = serde_json::from_slice(body).unwrap();
    let tool = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "name_session")
        .unwrap();
    assert_eq!(tool["strict"], false);
    assert_eq!(tool["parameters"]["properties"]["name"]["minLength"], 1);
    assert_eq!(tool["parameters"]["properties"]["name"]["maxLength"], 60);
    assert!(
        tool["description"]
            .as_str()
            .unwrap()
            .contains("after the first prompt"),
        "{tool}"
    );
    assert!(
        tool["description"]
            .as_str()
            .unwrap()
            .contains("when the work's topic changes, a handoff included"),
        "{tool}"
    );
    assert!(tool_names(body).contains(&"name_session".to_owned()));
}

#[test]
fn length_limits_fail_invalid_arguments_and_write_no_name() {
    let setup = Setup::new();
    let long = "a".repeat(61);
    let sixty = "é".repeat(60);
    let server = ProviderServer::start([
        stream(&[
            name_call("c1", &json!({"name": ""})),
            name_call("c2", &json!({"name": long})),
            name_call("c3", &json!({"name": sixty})),
        ]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);

    // One step makes all three calls: the fake provider answers all three
    // in one reply.
    let output = run_to_exit(setup.deadline, "fiber ask", setup.fiber(&["ask", "go"]));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let lines: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or(Value::Null))
        .collect();
    let id = session_id(&lines).to_owned();
    let log = read_lines(&session_dir(&setup, &id).join("events.jsonl"));
    let completed: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "tool_call_completed")
        .collect();
    assert_eq!(completed.len(), 3);
    assert_eq!(completed[0]["payload"]["status"], "failed");
    assert_eq!(
        completed[0]["payload"]["error"]["code"],
        "invalid_arguments"
    );
    assert_eq!(completed[1]["payload"]["status"], "failed");
    assert_eq!(
        completed[1]["payload"]["error"]["code"],
        "invalid_arguments"
    );
    let named: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .collect();
    assert_eq!(named.len(), 1, "{log:?}");
    assert_eq!(named[0]["payload"]["name"], json!(sixty));
    assert_eq!(named[0]["action_id"], completed[2]["action_id"]);
}

#[test]
fn the_latest_name_is_the_session_name_after_a_resume() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[name_call("c1", &json!({"name": "first"}))]),
        stream(&[name_call("c2", &json!({"name": "second"}))]),
        hello(),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let lines = ask_with(&setup, "go");
    let id = session_id(&lines).to_owned();
    let log = read_lines(&session_dir(&setup, &id).join("events.jsonl"));
    let named: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .collect();
    assert_eq!(named.len(), 2);
    assert_eq!(named[0]["payload"]["name"], "first");
    assert_eq!(named[1]["payload"]["name"], "second");

    let output = run_to_exit(
        setup.deadline,
        "fiber ask --resume",
        setup.fiber(&["ask", "--resume", &id, "hi"]),
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let resumed: Vec<String> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    let status = resumed
        .iter()
        .filter(|line| is_status(line))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .next_back()
        .expect("a session_status line");
    assert_eq!(status["payload"]["name"], "second");
}

#[test]
fn a_person_name_pins_the_session_after_a_resume() {
    let setup = Setup::new();
    let server = ProviderServer::start([
        stream(&[name_call("c1", &json!({"name": "first"}))]),
        hello(),
        stream(&[name_call("c2", &json!({"name": "Theirs"}))]),
        hello(),
    ])
    .unwrap();
    setup.provider(&server);
    let lines = ask_with(&setup, "go");
    let id = session_id(&lines).to_owned();
    {
        let dir = session_dir(&setup, &id);
        let sessions = dir.parent().unwrap();
        let log = log::Log::open(sessions, SessionId(id.clone()), Arc::new(SystemClock)).unwrap();
        log.append(
            &Event::SessionNamed(SessionNamed {
                name: Some("Mine".into()),
                by: NamedBy::Person,
            }),
            None,
            None,
        )
        .unwrap();
    }
    let before = read_lines(&session_dir(&setup, &id).join("events.jsonl"))
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .count();

    let output = run_to_exit(
        setup.deadline,
        "fiber ask --resume",
        setup.fiber(&["ask", "--resume", &id, "hi"]),
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert_eq!(output.status.code(), Some(0), "stderr: {stderr}");
    let log = read_lines(&session_dir(&setup, &id).join("events.jsonl"));
    let named: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "session_named")
        .collect();
    assert_eq!(named.len(), before, "{log:?}");
    assert_eq!(named.last().unwrap()["payload"]["by"], "person");
    let completed: Vec<&Value> = log
        .iter()
        .filter(|l| l["kind"] == "tool_call_completed")
        .collect();
    let last = completed.last().unwrap();
    assert_eq!(last["payload"]["status"], "failed");
    assert_eq!(last["payload"]["error"]["code"], "name_pinned");
    assert_eq!(
        last["payload"]["error"]["message"],
        "the person named this session"
    );
    let status = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter(|line| is_status(line))
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .next_back()
        .expect("a session_status line");
    assert_eq!(status["payload"]["name"], "Mine");
}
