//! Part 10 wiring at the binary level (`docs/state.md`, "Diagnostic
//! logs"): the built `fiber` times a background start as `jobs.start`
//! through the shell's timing clone and the registry installs. At `debug`
//! an `ask` whose model starts a background shell writes one `jobs.start`,
//! and one whose model spawns a delegate writes one per spawn, on new and
//! resumed sessions; otherwise nothing is written. Every wait is the
//! test's one deadline, and `ms` is normalized, never asserted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Run, Setup, function_call, is_status, run_to_exit, stream, text_reply, write_json};

/// Runs `fiber ask` with `args` in the workspace to exit and parses its
/// stdout.
#[track_caller]
fn ask_with(setup: &Setup, args: &[&str]) -> Run {
    let mut command = setup.fiber(args);
    command.current_dir(setup.workspace());
    let output = run_to_exit(setup.deadline, "fiber ask", command);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines = stdout
        .lines()
        .filter(|l| !is_status(l))
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    Run {
        code: output.status.code(),
        lines,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// Installs provider `name` at `url` with `protocol`, after the pattern in
/// `provider_diagnostics.rs`, which stays unchanged.
fn install_url(setup: &Setup, name: &str, url: &str, protocol: &str) {
    let source = setup.root.path().join(format!("src-{name}"));
    write_json(
        &source.join("extension.json"),
        &json!({"name": name, "version": "v0.0.0", "fiber": "0.0.0", "api": 1, "providers": {name: [format!("{url}/v1")]}}),
    );
    write_json(
        &source.join(format!("providers/{name}.json")),
        &json!({
            "name": name,
            "credential": {"env": "FIBER_TEST_FAKE_KEY"},
            "models": [{"id": "m", "protocol": protocol, "base_url": format!("{url}/v1"), "context_window": 100000}]
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
}

/// Every line of the diagnostic file `name`, parsed.
fn diagnostic_file(setup: &Setup, name: &str) -> Vec<Value> {
    let text = fs::read_to_string(setup.home().join("logs").join(name)).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The ask process's diagnostic file for session `id`.
fn ask_log(setup: &Setup, id: &str) -> Vec<Value> {
    diagnostic_file(setup, &format!("ask-{id}.log"))
}

/// Every `jobs.start` line in `lines`.
fn starts(lines: &[Value]) -> Vec<&Value> {
    lines
        .iter()
        .filter(|line| line["code"] == "op_timing")
        .filter(|line| line["data"]["op"] == "jobs.start")
        .collect()
}

/// Every `jobs.start` line under the home's `logs/`, parsed.
fn all_starts(setup: &Setup) -> Vec<Value> {
    let mut found = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return found;
    }
    for entry in fs::read_dir(&logs).unwrap() {
        let text = fs::read_to_string(entry.unwrap().path()).unwrap();
        for line in text.lines() {
            let line: Value = serde_json::from_str(line).unwrap();
            if line["code"] == "op_timing" && line["data"]["op"] == "jobs.start" {
                found.push(line);
            }
        }
    }
    found
}

/// The session's `events.jsonl` lines, found wherever its project is.
fn session_events(setup: &Setup, id: &str) -> Vec<Value> {
    let projects = setup.home().join("projects");
    let log = fs::read_dir(&projects)
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .path()
                .join("sessions")
                .join(id)
                .join("events.jsonl")
        })
        .find(|log| log.is_file())
        .expect("the session's log exists");
    fs::read_to_string(log)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The wire promise on every `jobs.start` line: `debug`, exactly the keys
/// `op` and `ms` with an unsigned `ms`, and none of the session's content,
/// the workspace or the configuration anywhere in its `data`.
fn assert_jobs_start_wire(lines: &[&Value], session_id: &str, forbidden: &[&str]) {
    assert!(!lines.is_empty(), "the test proved its lines exist first");
    for line in lines {
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        assert_eq!(line["process"], "ask", "{line}");
        assert_eq!(line["session_id"], session_id, "{line}");
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let mut keys: Vec<&str> = line["data"]
            .as_object()
            .expect("data is an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["ms", "op"], "{line}");
        assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
        let data = line["data"].to_string();
        for sentinel in forbidden {
            assert!(
                !data.contains(sentinel),
                "no content, path or configuration value in {data}"
            );
        }
    }
}

/// A shell journey at `extra` diagnostics: the model starts one background
/// shell carrying `sentinel`, then answers. Returns the run.
fn shell_journey(setup: &Setup, sentinel: &str, extra: &Value) -> Run {
    let command = format!("echo {sentinel}");
    let server = ProviderServer::start_responding(move |request| {
        let text = String::from_utf8_lossy(&request.body);
        if !text.contains(r#""name":"shell""#) {
            return text_reply("allow");
        }
        if !text.contains("Started in the background") {
            return stream(&[function_call(
                "call_shell",
                "shell",
                &json!({"command": command, "run_in_background": true}),
            )]);
        }
        text_reply("done")
    })
    .unwrap();
    install_url(setup, "pa", &server.url(), "openai-responses");
    let mut config = json!({"model": "pa/m", "reviewer": {"model": "pa/m"}});
    for (key, value) in extra.as_object().into_iter().flatten() {
        config[key] = value.clone();
    }
    write_json(&setup.home().join("config.json"), &config);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "shell", "prefix": ""})
        ),
    )
    .unwrap();
    let run = ask_with(
        setup,
        &["ask", &format!("run {sentinel} in the background")],
    );
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        server.await_requests(1, setup.deadline),
        "the model request arrived: a passing test really asked"
    );
    run
}

fn spawn_call(model: &str) -> Value {
    function_call(
        "call_delegate",
        "delegate_spawn",
        &json!({"description": "scan", "prompt": "say hello", "model": model}),
    )
}

/// A delegate journey at `extra` diagnostics: the model spawns one
/// delegate on the child server, then answers. Returns the run.
fn delegate_journey(
    setup: &Setup,
    parent: &ProviderServer,
    child: &ProviderServer,
    extra: &Value,
) -> Run {
    install_url(setup, "pa", &parent.url(), "openai-responses");
    install_url(setup, "pb", &child.url(), "openai-responses");
    let mut config = json!({"model": "pa/m", "reviewer": {"model": "pa/m"}});
    for (key, value) in extra.as_object().into_iter().flatten() {
        config[key] = value.clone();
    }
    write_json(&setup.home().join("config.json"), &config);
    fs::write(
        setup.home().join("rules"),
        format!(
            "{}\n",
            json!({"decision": "allow", "tool": "delegate_spawn", "prefix": ""})
        ),
    )
    .unwrap();
    let run = ask_with(setup, &["ask", "scan the tree"]);
    assert_eq!(run.code, Some(0), "stderr: {}", run.stderr);
    assert!(
        parent.await_requests(1, setup.deadline) && child.await_requests(1, setup.deadline),
        "both sides asked"
    );
    run
}

/// The parent side of a delegate journey: reviewer requests are allowed; a
/// session request without a started delegate spawns one, once per run,
/// and anything after is done. The resume run's request carries its own
/// receipt, so the count of started delegates tells the runs apart.
fn parent_server(resume_marker: &str) -> ProviderServer {
    let resume_marker = resume_marker.to_owned();
    ProviderServer::start_responding(move |request| {
        let text = String::from_utf8_lossy(&request.body).into_owned();
        let resumed = text.contains(resume_marker.as_str());
        if !text.contains(r#""name":"delegate_spawn""#) {
            return text_reply("allow");
        }
        let spawns = text.matches("Started delegate").count();
        if (!resumed && spawns == 0) || (resumed && spawns == 1) {
            return stream(&[spawn_call("fiber:pb/m")]);
        }
        text_reply("done")
    })
    .unwrap()
}

fn child_server() -> ProviderServer {
    ProviderServer::start_responding(|_| text_reply("hi from child")).unwrap()
}

/// The delegate sessions a parent run started, in start order.
fn child_ids(run: &Run) -> Vec<String> {
    run.all("delegate_started")
        .iter()
        .map(|line| {
            line["payload"]["delegate_session_id"]
                .as_str()
                .expect("the start names the child")
                .to_owned()
        })
        .collect()
}

#[test]
fn a_debug_shell_start_writes_one_jobs_start() {
    let setup = Setup::new();
    let sentinel = "sentinel-shell-8k2p";
    let run = shell_journey(
        &setup,
        sentinel,
        &json!({"diagnostics": {"level": "debug"}, "session": {"idle_exit_ms": 3600000}}),
    );
    let id = run.session_id().to_owned();
    // The tool result is the background receipt.
    let completed: Vec<&Value> = run.all("tool_call_completed");
    assert!(
        completed
            .iter()
            .any(|line| line.to_string().contains("Started in the background")),
        "the tool result holds the receipt: {completed:?}"
    );
    let lines = ask_log(&setup, &id);
    let found = starts(&lines);
    assert_eq!(found.len(), 1, "{lines:?}");
    let workspace = setup.root.path().display().to_string();
    assert_jobs_start_wire(&found, &id, &[sentinel, &workspace, "pa/m", "artifacts"]);
    // No timing text in the session log or the process output.
    let events = session_events(&setup, &id);
    assert!(
        events.iter().all(|line| line["kind"] != "op_timing"),
        "the session log holds no timing"
    );
    assert!(!run.stderr.contains("op_timing"), "nothing timed on stderr");
}

#[test]
fn a_debug_delegate_spawn_writes_one_jobs_start_and_none_in_the_child() {
    let setup = Setup::new();
    let parent = parent_server("never-resumed");
    let child = child_server();
    let run = delegate_journey(
        &setup,
        &parent,
        &child,
        &json!({"diagnostics": {"level": "debug"}, "session": {"idle_exit_ms": 3600000}}),
    );
    let parent_id = run.session_id().to_owned();
    let children = child_ids(&run);
    assert_eq!(children.len(), 1);
    let lines = ask_log(&setup, &parent_id);
    let found = starts(&lines);
    assert_eq!(found.len(), 1, "{lines:?}");
    let workspace = setup.root.path().display().to_string();
    assert_jobs_start_wire(
        &found,
        &parent_id,
        &["say hello", &workspace, "fiber:pb/m", "artifacts"],
    );
    // The child spawned no job: its own file holds no `jobs.start`.
    let child_lines = diagnostic_file(&setup, &format!("session-{}.log", children[0]));
    assert!(!child_lines.is_empty(), "the child wrote its own lines");
    assert!(
        starts(&child_lines).is_empty(),
        "the child timed no start: {child_lines:?}"
    );
    let events = session_events(&setup, &parent_id);
    assert!(
        events.iter().all(|line| line["kind"] != "op_timing"),
        "the session log holds no timing"
    );
}

#[test]
fn a_debug_resume_times_its_delegate_spawn() {
    let setup = Setup::new();
    let resume_marker = "sentinel-resume-9m4q";
    let parent = parent_server(resume_marker);
    let child = child_server();
    let run = delegate_journey(
        &setup,
        &parent,
        &child,
        &json!({"diagnostics": {"level": "debug"}, "session": {"idle_exit_ms": 3600000}}),
    );
    let id = run.session_id().to_owned();
    assert_eq!(starts(&ask_log(&setup, &id)).len(), 1);
    let resumed = ask_with(&setup, &["ask", "--resume", &id, resume_marker]);
    assert_eq!(resumed.code, Some(0), "stderr: {}", resumed.stderr);
    // The same file gains exactly one more `jobs.start`: the resumed
    // registry's install.
    let lines = ask_log(&setup, &id);
    let found = starts(&lines);
    assert_eq!(found.len(), 2, "{lines:?}");
    let workspace = setup.root.path().display().to_string();
    assert_jobs_start_wire(
        &found,
        &id,
        &[
            "say hello",
            &workspace,
            "fiber:pb/m",
            "artifacts",
            resume_marker,
        ],
    );
    let children = child_ids(&resumed);
    assert_eq!(children.len(), 1, "the resume spawned once more");
    for child_id in child_ids(&run).iter().chain(children.iter()) {
        let child_lines = diagnostic_file(&setup, &format!("session-{child_id}.log"));
        assert!(
            starts(&child_lines).is_empty(),
            "no child timed a start: {child_lines:?}"
        );
    }
}

#[test]
fn without_debug_no_jobs_start_is_written() {
    for extra in [json!({}), json!({"diagnostics": {"level": "info"}})] {
        let setup = Setup::new();
        let sentinel = "sentinel-quiet-3f7d";
        shell_journey(&setup, sentinel, &extra);
        assert!(
            all_starts(&setup).is_empty(),
            "no shell start at {extra}: {:?}",
            all_starts(&setup)
        );
    }
    for extra in [json!({}), json!({"diagnostics": {"level": "info"}})] {
        let setup = Setup::new();
        let parent = parent_server("never-resumed");
        let child = child_server();
        delegate_journey(&setup, &parent, &child, &extra);
        assert!(
            all_starts(&setup).is_empty(),
            "no delegate start at {extra}: {:?}",
            all_starts(&setup)
        );
    }
}
