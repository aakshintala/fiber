//! Part 13 timers at the binary level (`docs/state.md`, "Diagnostic
//! logs"): the built `fiber` seeds a session, searches it from a second
//! `ask` through the `session_search` tool, resumes, and runs the CLI.
//! At `debug` the second ask writes one `session_search.scan` line, one
//! `log.append` per durable line and one `log.fsync` per fsynced kind;
//! otherwise nothing debug is written, and the CLI writes nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use fakes::ProviderServer;
use serde_json::{Value, json};
use support::{Run, Setup, function_call, hello, is_status, run_to_exit, stream, write_json};

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

/// Installs a provider `fake` with model `m` on `openai-responses` at the
/// fake server, and writes configuration naming `fake/m` with `extra`.
fn provider_with_extra(setup: &Setup, server: &ProviderServer, extra: &Value) {
    let source = setup.root.path().join("src").join("fake");
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
        &setup.home(),
        &extensions::Request::Path(source),
        "0.0.0",
        &extensions::Origin::github(),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap()
    .commit()
    .unwrap();
    let mut config = json!({"model": "fake/m"});
    for (key, value) in extra.as_object().into_iter().flatten() {
        config[key] = value.clone();
    }
    write_json(&setup.home().join("config.json"), &config);
}

/// Runs `fiber` with `args` to its exit through the shared watchdog and
/// deadline: files are read only after exit, so the writer has finished.
fn run(setup: &Setup, args: &[&str]) -> Run {
    Run::from(run_to_exit(
        setup.deadline,
        &format!("fiber {}", args.join(" ")),
        setup.fiber(args),
    ))
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

/// The ask process's diagnostic file for session `id`.
fn ask_log(setup: &Setup, id: &str) -> PathBuf {
    setup.home().join("logs").join(format!("ask-{id}.log"))
}

/// Every `op_timing` line in the file, parsed.
fn op_lines(path: &std::path::Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Every `op_timing` line under the home's `logs/`, parsed.
fn all_op_lines(setup: &Setup) -> Vec<Value> {
    let mut lines = Vec::new();
    let logs = setup.home().join("logs");
    if !logs.exists() {
        return lines;
    }
    for entry in fs::read_dir(&logs).unwrap() {
        for line in op_lines(&entry.unwrap().path()) {
            if line["code"] == "op_timing" {
                lines.push(line);
            }
        }
    }
    lines
}

/// Whether appending an event of `kind` ends with an fsync
/// (`docs/events.md`, "Writing"): the fsynced kinds, with
/// `usage_recorded` only outside an action.
fn fsyncs(line: &Value) -> bool {
    match line["kind"].as_str().unwrap() {
        "assistant_message_started"
        | "assistant_message_completed"
        | "tool_call_started"
        | "tool_call_completed"
        | "shell_command"
        | "extension_exec" => true,
        "usage_recorded" => line.get("action_id").is_none(),
        _ => false,
    }
}

/// The wire promise on every Part 13 line: `debug`, exactly the keys `op`
/// and `ms` with an unsigned `ms`, and none of the session's content, the
/// workspace or the configuration anywhere in the line.
fn assert_part13_wire(lines: &[Value], session_id: &str, forbidden: &[&str]) {
    assert!(!lines.is_empty(), "the test proved its lines exist first");
    for line in lines {
        assert_eq!(line["code"], "op_timing", "{line}");
        assert_eq!(line["level"], "debug", "{line}");
        assert_eq!(line["process"], "ask", "{line}");
        assert_eq!(line["session_id"], session_id, "{line}");
        assert_eq!(line["message"], "An operation ended.", "{line}");
        let data = line["data"].as_object().expect("data is an object");
        assert_eq!(data.keys().collect::<Vec<_>>(), vec!["ms", "op"], "{line}");
        assert!(line["data"]["ms"].as_u64().is_some(), "{line}");
        let raw = line.to_string();
        for sentinel in forbidden {
            assert!(
                !raw.contains(sentinel),
                "no content, path or configuration value in {raw}"
            );
        }
    }
}

/// Seeds one session with `sentinel` in its prompt, then searches for it
/// from a second `ask` through the `session_search` tool. Returns both
/// runs. Local fake-provider traffic only.
fn search_journey(setup: &Setup, sentinel: &str) -> (Run, Run) {
    let seed = ProviderServer::start([hello()]).unwrap();
    provider_with_extra(setup, &seed, &json!({"diagnostics": {"level": "debug"}}));
    let first = run(setup, &["ask", &format!("note {sentinel} for later")]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    let search = ProviderServer::start([
        stream(&[function_call(
            "call_search",
            "session_search",
            &json!({"text": sentinel}),
        )]),
        hello(),
    ])
    .unwrap();
    provider_with_extra(setup, &search, &json!({"diagnostics": {"level": "debug"}}));
    let second = run(setup, &["ask", "find that old note"]);
    assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
    (first, second)
}

#[test]
fn a_debug_search_times_one_scan_with_every_write_and_sync() {
    let setup = Setup::new();
    let sentinel = "sentinel-zulu-7q9x";
    let (first, second) = search_journey(&setup, sentinel);
    let first_id = first.session_id().to_owned();
    let second_id = second.session_id().to_owned();
    // The tool result holds the seeded session's hit.
    let completed: Vec<&Value> = second.all("tool_call_completed");
    assert!(
        completed
            .iter()
            .any(|line| line.to_string().contains(&first_id)),
        "the tool result holds the hit: {:?}",
        completed
    );
    let path = ask_log(&setup, &second_id);
    let lines = op_lines(&path);
    let scans: Vec<&Value> = lines
        .iter()
        .filter(|line| line["data"]["op"] == "session_search.scan")
        .collect();
    assert_eq!(scans.len(), 1, "{lines:?}");
    // One append per durable line of the session's own log: every line in
    // `events.jsonl` is durable.
    let events = session_events(&setup, &second_id);
    let appends = lines
        .iter()
        .filter(|line| line["data"]["op"] == "log.append")
        .count();
    assert_eq!(appends, events.len(), "{lines:?}");
    let syncs = lines
        .iter()
        .filter(|line| line["data"]["op"] == "log.fsync")
        .count();
    assert_eq!(syncs, events.iter().filter(|line| fsyncs(line)).count());
    // Every Part 13 line keeps the closed keys and carries no content.
    let workspace = setup.root.path().display().to_string();
    let part13: Vec<Value> = lines
        .into_iter()
        .filter(|line| line["code"] == "op_timing")
        .filter(|line| {
            ["log.append", "log.fsync", "session_search.scan"]
                .contains(&line["data"]["op"].as_str().unwrap())
        })
        .collect();
    assert_part13_wire(&part13, &second_id, &[sentinel, &workspace, "fake/m"]);
    // No timing text in the session log or the process output.
    assert!(
        events.iter().all(|line| line["kind"] != "op_timing"),
        "the session log holds no timing"
    );
    assert!(
        !second.stderr.contains("op_timing"),
        "nothing timed on stderr"
    );
}

#[test]
fn a_debug_resume_times_its_writes_with_no_new_scan() {
    let setup = Setup::new();
    let sentinel = "sentinel-resume-4d2w";
    let (_, second) = search_journey(&setup, sentinel);
    let id = second.session_id().to_owned();
    let path = ask_log(&setup, &id);
    let before = op_lines(&path);
    let events_before = session_events(&setup, &id).len();
    let scans_before = before
        .iter()
        .filter(|line| line["data"]["op"] == "session_search.scan")
        .count();
    assert_eq!(scans_before, 1);
    let search = ProviderServer::start([hello()]).unwrap();
    provider_with_extra(&setup, &search, &json!({"diagnostics": {"level": "debug"}}));
    let resumed = run(&setup, &["ask", "--resume", &id, "next"]);
    assert_eq!(resumed.code, Some(0), "stderr: {}", resumed.stderr);
    // The same file gains one append per durable line the resume added,
    // with fsyncs counted the same way, and no new scan.
    let after = op_lines(&path);
    let events = session_events(&setup, &id);
    let added = &events[events_before..];
    let appends = after
        .iter()
        .filter(|line| line["data"]["op"] == "log.append")
        .count()
        - before
            .iter()
            .filter(|line| line["data"]["op"] == "log.append")
            .count();
    assert_eq!(appends, added.len(), "{after:?}");
    let syncs = after
        .iter()
        .filter(|line| line["data"]["op"] == "log.fsync")
        .count()
        - before
            .iter()
            .filter(|line| line["data"]["op"] == "log.fsync")
            .count();
    assert_eq!(syncs, added.iter().filter(|line| fsyncs(line)).count());
    assert_eq!(
        after
            .iter()
            .filter(|line| line["data"]["op"] == "session_search.scan")
            .count(),
        1,
        "no search ran on resume"
    );
}

#[test]
fn without_debug_no_part13_line_is_written() {
    for extra in [json!({}), json!({"diagnostics": {"level": "info"}})] {
        let setup = Setup::new();
        let sentinel = "sentinel-quiet-1a8n";
        let seed = ProviderServer::start([hello()]).unwrap();
        provider_with_extra(&setup, &seed, &extra);
        let first = run(&setup, &["ask", &format!("note {sentinel} for later")]);
        assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
        let search = ProviderServer::start([
            stream(&[function_call(
                "call_search",
                "session_search",
                &json!({"text": sentinel}),
            )]),
            hello(),
        ])
        .unwrap();
        provider_with_extra(&setup, &search, &extra);
        let second = run(&setup, &["ask", "find that old note"]);
        assert_eq!(second.code, Some(0), "stderr: {}", second.stderr);
        assert!(
            all_op_lines(&setup).is_empty(),
            "no timing at {extra}: {:?}",
            all_op_lines(&setup)
        );
    }
}

#[test]
fn the_cli_search_writes_no_diagnostic_line() {
    let setup = Setup::new();
    let sentinel = "sentinel-cli-6h3v";
    let seed = ProviderServer::start([hello()]).unwrap();
    provider_with_extra(&setup, &seed, &json!({"diagnostics": {"level": "debug"}}));
    let first = run(&setup, &["ask", &format!("note {sentinel} for later")]);
    assert_eq!(first.code, Some(0), "stderr: {}", first.stderr);
    // Every diagnostic file, byte for byte, before the command.
    let logs = setup.home().join("logs");
    let mut before = BTreeMap::new();
    for entry in fs::read_dir(&logs).unwrap() {
        let path = entry.unwrap().path();
        before.insert(path.clone(), fs::read(&path).unwrap());
    }
    let output = run_to_exit(
        setup.deadline,
        "fiber sessions search",
        setup.fiber(&["sessions", "search", sentinel]),
    );
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("hits for"),
        "the hit is on stdout: {stdout}"
    );
    let mut after = BTreeMap::new();
    for entry in fs::read_dir(&logs).unwrap() {
        let path = entry.unwrap().path();
        after.insert(path.clone(), fs::read(&path).unwrap());
    }
    assert_eq!(before, after, "the command writes no diagnostic line");
}
