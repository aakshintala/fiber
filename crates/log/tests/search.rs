//! Searching past sessions through the public scan (`docs/tools.md`,
//! "Searching past sessions"), over a temporary Fiber home.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::event;
use contract::ActionId;
use contract::SessionId;
use contract::session_search::{Label, Query, Scan};
use fakes::clock::FakeClock;
use log::{Identity, Log, SessionScan, project_key, sessions_dir};
use serde_json::json;

/// Workspaces under `/other` are another project; every other is `/proj`.
fn identity() -> Identity {
    Arc::new(|path: &Path| {
        if path.starts_with("/other") {
            PathBuf::from("/other")
        } else {
            PathBuf::from("/proj")
        }
    })
}

fn search(scan: &SessionScan, text: &str, all_projects: bool) -> contract::session_search::Found {
    let query = Query {
        text: text.into(),
        all_projects,
        limit: 20,
    };
    scan.scan(&query, &fakes::CancelToken::new())
}

#[test]
fn a_search_finds_a_message_an_input_and_an_artifact_of_its_own_project() {
    let home = fakes::TempDir::new("log-search-public");
    let clock = FakeClock::new();
    let sessions = sessions_dir(home.path(), Path::new("/proj"));
    let log = Log::create(&sessions, SessionId("s_past".into()), clock.clone()).unwrap();
    let started = |workspace: &str| {
        event(
            "session_started",
            json!({"workspace": workspace, "variables": {"path": "/bin", "names": [], "source": "inherited"}}),
        )
    };
    log.append(&started("/proj/sub"), None, None).unwrap();
    clock.advance(Duration::from_secs(1));
    log.append(
        &event(
            "session_named",
            json!({"name": "retry work", "by": "person"}),
        ),
        None,
        None,
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    let prompt = json!({"input": [{"type": "message", "content": [{"type": "text", "text": "We keep the Retry Budget at 3"}], "source": "driver"}]});
    log.append(&event("turn_started", prompt), None, None)
        .unwrap();
    clock.advance(Duration::from_secs(1));
    let call = json!({"name": "shell", "arguments": {"command": "grep -n \"retry budget\""}});
    log.append(&event("tool_call_requested", call), None, None)
        .unwrap();
    clock.advance(Duration::from_secs(1));
    let dir = sessions.join("s_past");
    let artifact = dir.join("artifacts").join("call_1.txt");
    fs::write(&artifact, "full output: RETRY BUDGET exhausted\n").unwrap();
    let completed = json!({"status": "completed", "content": [{"type": "text", "text": "retry budget (cut)"}], "artifact": "artifacts/call_1.txt"});
    log.append(&event("tool_call_completed", completed), None, None)
        .unwrap();
    drop(log);

    // Same key, another project: skipped without `all_projects`.
    let other = Log::create(&sessions, SessionId("s_other".into()), clock.clone()).unwrap();
    other.append(&started("/other/w"), None, None).unwrap();
    clock.advance(Duration::from_secs(1));
    other
        .append(
            &event("text_completed", json!({"text": "retry budget"})),
            None,
            None,
        )
        .unwrap();
    drop(other);

    let scan = SessionScan::new(home.path(), Path::new("/proj"), identity());
    let found = search(&scan, "retry budget", false);
    let rows: Vec<(Label, u64, &str)> = found
        .hits
        .iter()
        .map(|hit| (hit.label, hit.seq.0, hit.snippet.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            (Label::ToolInput, 3, "grep -n \"retry budget\""),
            (Label::Message, 2, "We keep the Retry Budget at 3"),
            (Label::ToolOutput, 4, "full output: RETRY BUDGET exhausted"),
        ]
    );
    assert_eq!(found.total, 3);
    for hit in &found.hits {
        assert_eq!(hit.name, "retry work");
        assert_eq!(hit.log, dir.join("events.jsonl"));
    }
    assert_eq!(found.hits[2].artifact.as_deref(), Some(artifact.as_path()));
    assert!(found.problems.is_empty(), "{:?}", found.problems);

    let found = search(&scan, "retry budget", true);
    assert_eq!(found.total, 4);
    assert_eq!(found.hits[0].session_id, SessionId("s_other".into()));
    let key = project_key(Path::new("/proj"));
    assert_eq!(
        scan.scope(false),
        PathBuf::from(format!("{}/projects/{key}/", home.path().display()))
    );
}

#[test]
fn a_search_never_returns_session_search_own_calls_or_results() {
    let home = fakes::TempDir::new("log-search-self");
    let clock = FakeClock::new();
    let sessions = sessions_dir(home.path(), Path::new("/proj"));
    let log = Log::create(&sessions, SessionId("s_mixed".into()), clock.clone()).unwrap();
    log.append(
        &event(
            "session_started",
            json!({"workspace": "/proj", "variables": {"path": "/bin", "names": [], "source": "inherited"}}),
        ),
        None,
        None,
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    // The tool's own call: its arguments, its output and its artifact all
    // hold the query, and none gives a hit.
    log.append(
        &event(
            "tool_call_requested",
            json!({"name": "session_search", "arguments": {"text": "retry budget"}}),
        ),
        None,
        Some(ActionId("a_1".into())),
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    let dir = sessions.join("s_mixed");
    fs::write(
        dir.join("artifacts").join("self_1.txt"),
        "full output: RETRY BUDGET exhausted\n",
    )
    .unwrap();
    log.append(
        &event(
            "tool_call_completed",
            json!({"status": "completed", "content": [{"type": "text", "text": "retry budget results"}], "artifact": "artifacts/self_1.txt"}),
        ),
        None,
        Some(ActionId("a_1".into())),
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    // Another tool's call with the same text still hits.
    log.append(
        &event(
            "tool_call_requested",
            json!({"name": "shell", "arguments": {"command": "grep -n \"retry budget\""}}),
        ),
        None,
        Some(ActionId("a_2".into())),
    )
    .unwrap();
    clock.advance(Duration::from_secs(1));
    log.append(
        &event(
            "tool_call_completed",
            json!({"status": "completed", "content": [{"type": "text", "text": "found retry budget"}]}),
        ),
        None,
        Some(ActionId("a_2".into())),
    )
    .unwrap();
    drop(log);
    let scan = SessionScan::new(home.path(), Path::new("/proj"), identity());
    let found = search(&scan, "retry budget", false);
    let rows: Vec<(Label, u64, &str)> = found
        .hits
        .iter()
        .map(|hit| (hit.label, hit.seq.0, hit.snippet.as_str()))
        .collect();
    assert_eq!(
        rows,
        [
            (Label::ToolInput, 3, "grep -n \"retry budget\""),
            (Label::ToolOutput, 4, "found retry budget"),
        ]
    );
    assert_eq!(found.total, 2);
    assert!(found.hits.iter().all(|hit| hit.artifact.is_none()));
    assert!(found.problems.is_empty(), "{:?}", found.problems);
}
