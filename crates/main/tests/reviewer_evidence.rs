//! Binary-level test of `fiber reviewer evidence --json`
//! (`docs/invocation.md`, "Commands and flags"): the command prints each
//! topic's evidence as the panel gathers it, with credentials removed from
//! URLs as today. The fixtures seed session-log escalations and
//! reviewer-allow calls whose URLs carry userinfo, plus the git remote, CI
//! files and registries; the test asserts the stdout JSON equals the
//! in-process `gather` serialization per topic with `strip_userinfo`
//! applied to every URL-bearing tally field, and asserts the redaction
//! directly, since equality with raw `gather` alone cannot prove it. Every
//! run carries a wall-clock deadline.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

mod support;

use std::fs;
use std::path::{Path, PathBuf};

use contract::SessionId;
use contract::notes::{Evidence, Topic, strip_userinfo};
use serde_json::{Map, Value};
use support::*;

fn write_log(dir: &Path, bytes: &[u8]) {
    fs::create_dir_all(dir).unwrap();
    fs::write(dir.join("events.jsonl"), bytes).unwrap();
}

fn session_dir(home: &Path, key: &str, id: &str) -> PathBuf {
    home.join("projects").join(key).join("sessions").join(id)
}

fn line(kind: &str, session: &str, seq: u64, payload: &Value) -> String {
    serde_json::json!({
        "kind": kind,
        "session_id": session,
        "ts": seq,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    })
    .to_string()
        + "\n"
}

fn aline(kind: &str, action: &str, seq: u64, payload: &Value) -> String {
    serde_json::json!({
        "kind": kind,
        "session_id": "s_1",
        "action_id": action,
        "ts": seq,
        "schema_version": 1,
        "seq": seq,
        "payload": payload,
    })
    .to_string()
        + "\n"
}

fn started(workspace: &str) -> String {
    serde_json::json!({"kind": "session_started", "payload": {"workspace": workspace}}).to_string()
        + "\n"
}

fn pair(session: &str, seq: u64, req: &str, prefix: &str) -> String {
    let request = serde_json::json!({
        "request_id": req,
        "step": "review",
        "escalation": {"cause": "consecutive_blocks", "reason": "blocked"},
        "rule": {"subject": prefix, "prefix": prefix},
    });
    let resolved = serde_json::json!({
        "request_id": req,
        "decision": "allow",
        "decided_by": "person",
    });
    line("permission_requested", session, seq, &request)
        + &line("permission_resolved", session, seq + 1, &resolved)
}

/// Allowed shell calls: `reviewer` allows of one command.
fn allow_calls(actions: &[&str], command: &str, first_seq: u64) -> String {
    let mut body = String::new();
    for (at, action) in actions.iter().enumerate() {
        let seq = first_seq + u64::try_from(at).unwrap() * 2;
        let requested = serde_json::json!({"name": "shell", "arguments": {"command": command}});
        body.push_str(&aline("tool_call_requested", action, seq, &requested));
        body.push_str(&aline(
            "permission_resolved",
            action,
            seq + 1,
            &serde_json::json!({"decision": "allow", "decided_by": "reviewer"}),
        ));
    }
    body
}

/// The evidence the command must print for `topic`: the in-process `gather`
/// serialization with `strip_userinfo` on every URL-bearing tally field.
fn expected_topic(
    home: &Path,
    dir: &Path,
    workspace: &Path,
    shell: &tools::Shell,
    topic: Topic,
) -> Value {
    let items = doors::gather(home, dir, workspace, topic, Some(shell));
    Value::Array(items.iter().map(sanitized).collect())
}

/// One evidence item as the command prints it: tallies carry the wire form
/// with credentials removed from URLs; files and answer items print as
/// gathered.
fn sanitized(item: &Evidence) -> Value {
    let mut value = serde_json::to_value(item).unwrap();
    let fields: &[&str] = match item {
        Evidence::Session(_) => &["prefix", "text"],
        Evidence::Allows(_) => &["subject", "text"],
        Evidence::Files { .. } | Evidence::Answer { .. } => &[],
    };
    for field in fields {
        let stripped = value.get(field).and_then(Value::as_str).map(strip_userinfo);
        if let Some(text) = stripped
            && let Some(slot) = value.get_mut(field)
        {
            *slot = Value::String(text);
        }
    }
    value
}

/// Every string the JSON object carries, depth first.
fn texts(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => {
            for item in items {
                texts(item, out);
            }
        }
        Value::Object(map) => {
            for (_, item) in map {
                texts(item, out);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// The seeded repository: a git repository counts as one only after the
/// project key is read, so the key must be derived after this runs.
fn seed_repo(setup: &Setup, workspace: &Path) {
    git(setup.deadline, workspace, &["init", "--quiet"]);
    git(
        setup.deadline,
        workspace,
        &[
            "remote",
            "add",
            "origin",
            "https://user:token@github.com/acme/fiber.git",
        ],
    );
    git(
        setup.deadline,
        workspace,
        &["commit", "--quiet", "--allow-empty", "-m", "first"],
    );
    fs::create_dir_all(workspace.join(".github/workflows")).unwrap();
    fs::write(workspace.join(".github/workflows/job.yml"), "name: job\n").unwrap();
    fs::write(
        workspace.join(".npmrc"),
        "registry=https://npm:user@example.com/npm/\n",
    )
    .unwrap();
}

/// Session logs with a userinfo escalation prefix and reviewer allows
/// above and below the counting threshold.
fn seed_logs(home: &Path, key: &str, workspace: &str) {
    let mut log = started(workspace);
    log.push_str(&pair(
        "s_1",
        1,
        "s_1-r0",
        "gh pr merge https://person:secret@gh.example.com",
    ));
    log.push_str(&pair(
        "s_1",
        3,
        "s_1-r1",
        "gh pr merge https://person:secret@gh.example.com",
    ));
    log.push_str(&allow_calls(
        &["a_1", "a_2", "a_3", "a_4", "a_5"],
        "acme-cli fetch https://user:token@host/x",
        5,
    ));
    log.push_str(&allow_calls(
        &["b_1", "b_2", "b_3", "b_4"],
        "other-cli run https://other:pw@host/y",
        15,
    ));
    write_log(&session_dir(home, key, "s_1"), log.as_bytes());
}

/// The stdout of one evidence run over the seeded fixtures.
fn run(setup: &Setup, workspace: &Path) -> std::process::Output {
    let mut command = setup.fiber(&["reviewer", "evidence", "--json"]);
    command.current_dir(workspace);
    run_to_exit(setup.deadline, "fiber reviewer evidence --json", command)
}

#[test]
fn evidence_json_matches_gather_with_credentials_removed() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    fs::create_dir_all(&workspace).unwrap();
    seed_repo(&setup, &workspace);
    let home = setup.home();
    let key = log::project_key(&worktree::project(&workspace));
    seed_logs(&home, &key, workspace.to_str().unwrap());
    let dir = log::session_dir(&home, &key, &SessionId("cli".to_owned()));
    let shell = tools::Shell::new(workspace.clone(), fakes::clock::FakeClock::new());

    let output = run(&setup, &workspace);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: Value = serde_json::from_slice(&output.stdout).unwrap();

    let mut expected = Map::new();
    for topic in Topic::ALL {
        let name = serde_json::to_value(topic).unwrap();
        expected.insert(
            name.as_str().unwrap().to_owned(),
            expected_topic(&home, &dir, &workspace, &shell, topic),
        );
    }
    assert_eq!(shown, Value::Object(expected));

    let mut found = Vec::new();
    texts(&shown, &mut found);
    assert!(
        !found.is_empty(),
        "the seeded fixtures must show some evidence"
    );
    for text in &found {
        assert!(
            !text.contains("user:token@")
                && !text.contains("person:secret@")
                && !text.contains("other:pw@")
                && !text.contains("npm:user@"),
            "credential leaked: {text}"
        );
    }
    assert!(
        found
            .iter()
            .any(|text| text.contains("github.com/acme/fiber.git")),
        "the remote shows stripped: {found:?}"
    );
    assert!(
        found
            .iter()
            .any(|text| text.contains("acme-cli fetch https://host/x")),
        "the allow subject shows stripped: {found:?}"
    );
    assert!(
        found
            .iter()
            .any(|text| text.contains("gh.example.com") && !text.contains("person:secret@")),
        "the escalation prefix shows stripped: {found:?}"
    );
    assert!(
        !found.iter().any(|text| text.contains("other-cli")),
        "four allows stay below the threshold: {found:?}"
    );
}

#[test]
fn evidence_without_a_repo_skips_remotes_and_shows_the_rest() {
    let setup = Setup::new();
    let plain = setup.root.path().join("plain");
    fs::create_dir_all(&plain).unwrap();
    let home = setup.home();
    let key = log::project_key(&worktree::project(&plain));
    let mut log = started(plain.to_str().unwrap());
    log.push_str(&pair("s_1", 1, "s_1-r0", "gh pr merge"));
    write_log(&session_dir(&home, &key, "s_1"), log.as_bytes());

    let output = run(&setup, &plain);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut found = Vec::new();
    texts(&shown, &mut found);
    assert!(
        !found.iter().any(|text| text.contains("git remote")),
        "no repo means no remotes: {found:?}"
    );
    assert!(
        found.iter().any(|text| text.contains("answered allow")),
        "session tallies still show: {found:?}"
    );
}

#[test]
fn evidence_without_json_fails_naming_the_flag() {
    let setup = Setup::new();
    let workspace = setup.workspace();
    fs::create_dir_all(&workspace).unwrap();
    let output = run_to_exit(
        setup.deadline,
        "fiber reviewer evidence",
        setup.fiber(&["reviewer", "evidence"]),
    );
    assert_ne!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--json"), "usage names the flag: {stderr}");
    assert!(output.stdout.is_empty(), "a usage failure prints no JSON");
}
