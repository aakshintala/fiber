//! The history chain over logs written by `Log`: segments root first, the
//! fold point, and the last line.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code may unwrap, panic and index (docs/code-quality.md, \"Lints\"), helpers outside a #[test] included"
)]

mod common;

use common::*;
use contract::Seq;
use contract::events::Event;
use serde_json::json;

fn turn_started() -> Event {
    event("turn_started", json!({"input": []}))
}

fn turn_completed() -> Event {
    event("turn_completed", json!({"outcome": "completed"}))
}

fn forked_started(from: &str, at: u64) -> Event {
    event(
        "session_started",
        json!({"workspace": "/w",
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
            "forked_from": {"session_id": from, "seq": at}}),
    )
}

#[test]
fn two_logs_written_by_log_chain_root_first() {
    let tmp = TestDir::new("history-chain");
    let clock = fakes::clock::FakeClock::new();
    let root = log::Log::create(tmp.path(), id("s_aaaaaaaaaaaaaaaa"), clock.clone()).unwrap();
    root.append(&session_started(), None, None).unwrap();
    root.append(&turn_started(), None, None).unwrap();
    root.append(&turn_completed(), None, None).unwrap();
    drop(root);
    let child = log::Log::create(tmp.path(), id("s_bbbbbbbbbbbbbbbb"), clock).unwrap();
    child
        .append(&forked_started("s_aaaaaaaaaaaaaaaa", 1), None, None)
        .unwrap();
    child.append(&turn_started(), None, None).unwrap();
    drop(child);

    let segments = log::history(&tmp.session(&id("s_bbbbbbbbbbbbbbbb"))).unwrap();
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[0].session_id, id("s_aaaaaaaaaaaaaaaa"));
    assert_eq!(segments[0].to, Some(Seq(1)));
    assert_eq!(segments[1].session_id, id("s_bbbbbbbbbbbbbbbb"));
    assert_eq!(segments[1].to, None);
    let root_kinds: Vec<String> = segments[0]
        .lines(0)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert_eq!(
        root_kinds,
        ["session_started", "turn_started", "turn_completed"]
            .into_iter()
            .take(2)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    );
    let folded = log::history_to(&tmp.session(&id("s_bbbbbbbbbbbbbbbb")), Seq(2)).unwrap();
    assert_eq!(folded[1].to, Some(Seq(2)));
}

#[test]
fn the_last_line_is_the_logs_last_complete_line() {
    let tmp = TestDir::new("history-last");
    let clock = fakes::clock::FakeClock::new();
    let log = log::Log::create(tmp.path(), id("s_aaaaaaaaaaaaaaaa"), clock).unwrap();
    log.append(&session_started(), None, None).unwrap();
    log.append(&turn_started(), None, None).unwrap();
    drop(log);
    let found = log::last_line(&tmp.session(&id("s_aaaaaaaaaaaaaaaa"))).unwrap();
    assert_eq!(found.kind, "turn_started");
    assert_eq!(found.seq.unwrap(), Seq(1));
}

fn project_sessions(home: &TestDir, key: &str) -> std::path::PathBuf {
    home.path().join("projects").join(key).join("sessions")
}

fn moved_started(from: &str, at: u64, project: Option<serde_json::Value>) -> Event {
    let mut forked = json!({"session_id": from, "seq": at});
    if let Some(project) = project {
        forked["project"] = project;
    }
    event(
        "session_started",
        json!({"workspace": "/w",
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"},
            "forked_from": forked}),
    )
}

fn moved_line(child: &str, at: u64, workspace: &str) -> Event {
    event(
        "moved",
        json!({"new_session_id": child, "seq": at, "workspace": workspace}),
    )
}

#[test]
fn a_moved_chain_links_projects_with_exact_ancestry() {
    let tmp = TestDir::new("history-moved-projects");
    let clock = fakes::clock::FakeClock::new();
    let old_sessions = project_sessions(&tmp, "-work-old");
    let old = log::Log::create(&old_sessions, id("s_aaaaaaaaaaaaaaaa"), clock.clone()).unwrap();
    old.append(&session_started(), None, None).unwrap();
    old.append(&turn_started(), None, None).unwrap();
    old.append(&turn_completed(), None, None).unwrap();
    old.append(
        &moved_line("s_bbbbbbbbbbbbbbbb", 2, "/work/new"),
        None,
        None,
    )
    .unwrap();
    drop(old);
    let new_sessions = project_sessions(&tmp, "-work-new");
    let child = log::Log::create(&new_sessions, id("s_bbbbbbbbbbbbbbbb"), clock).unwrap();
    child
        .append(
            &moved_started("s_aaaaaaaaaaaaaaaa", 2, Some(json!("-work-old"))),
            None,
            None,
        )
        .unwrap();
    child.append(&turn_started(), None, None).unwrap();
    drop(child);

    let segments = log::history(&new_sessions.join("s_bbbbbbbbbbbbbbbb")).unwrap();
    let parent = segments
        .iter()
        .find(|segment| segment.session_id == id("s_aaaaaaaaaaaaaaaa"))
        .expect("the moved parent is in the history");
    assert_eq!(
        parent.dir,
        log::session_dir(tmp.path(), "-work-old", &id("s_aaaaaaaaaaaaaaaa"))
    );
    assert_eq!(parent.to, Some(Seq(2)));
    let child = segments
        .iter()
        .find(|segment| segment.session_id == id("s_bbbbbbbbbbbbbbbb"))
        .expect("the moved child is in the history");
    assert_eq!(
        child.dir,
        log::session_dir(tmp.path(), "-work-new", &id("s_bbbbbbbbbbbbbbbb"))
    );
    assert_eq!(child.to, None);
    // The parent's own `moved` ending stays intact: the walk reads first
    // lines only.
    let tail = log::last_line(&parent.dir).unwrap();
    assert_eq!(tail.kind, "moved");
}

#[test]
fn alternating_project_hops_resolve_each_parent_where_recorded() {
    // X→Y→X through `Log`: root A in the old project, child B in the
    // new project naming A, and grandchild C back in the old project
    // naming B. Each segment carries its own directory and point.
    let tmp = TestDir::new("history-moved-alternating");
    let clock = fakes::clock::FakeClock::new();
    let old_sessions = project_sessions(&tmp, "-work-old");
    let root = log::Log::create(&old_sessions, id("s_aaaaaaaaaaaaaaaa"), clock.clone()).unwrap();
    root.append(&session_started(), None, None).unwrap();
    root.append(&turn_started(), None, None).unwrap();
    drop(root);
    let new_sessions = project_sessions(&tmp, "-work-new");
    let middle = log::Log::create(&new_sessions, id("s_bbbbbbbbbbbbbbbb"), clock.clone()).unwrap();
    middle
        .append(
            &moved_started("s_aaaaaaaaaaaaaaaa", 1, Some(json!("-work-old"))),
            None,
            None,
        )
        .unwrap();
    middle.append(&turn_started(), None, None).unwrap();
    drop(middle);
    let grandchild = log::Log::create(&old_sessions, id("s_cccccccccccccccc"), clock).unwrap();
    grandchild
        .append(
            &moved_started("s_bbbbbbbbbbbbbbbb", 1, Some(json!("-work-new"))),
            None,
            None,
        )
        .unwrap();
    grandchild.append(&turn_started(), None, None).unwrap();
    drop(grandchild);

    let segments = log::history(&old_sessions.join("s_cccccccccccccccc")).unwrap();
    assert_eq!(segments.len(), 3);
    assert_eq!(segments[0].session_id, id("s_aaaaaaaaaaaaaaaa"));
    assert_eq!(
        segments[0].dir,
        log::session_dir(tmp.path(), "-work-old", &id("s_aaaaaaaaaaaaaaaa"))
    );
    assert_eq!(segments[0].to, Some(Seq(1)));
    assert_eq!(segments[1].session_id, id("s_bbbbbbbbbbbbbbbb"));
    assert_eq!(
        segments[1].dir,
        log::session_dir(tmp.path(), "-work-new", &id("s_bbbbbbbbbbbbbbbb"))
    );
    assert_eq!(segments[1].to, Some(Seq(1)));
    assert_eq!(segments[2].session_id, id("s_cccccccccccccccc"));
    assert_eq!(
        segments[2].dir,
        log::session_dir(tmp.path(), "-work-old", &id("s_cccccccccccccccc"))
    );
    assert_eq!(segments[2].to, None);
    // The middle segment reads its own project's log, bounded by its
    // own point.
    let middle_kinds: Vec<String> = segments[1]
        .lines(0)
        .unwrap()
        .into_iter()
        .map(|line| line.kind)
        .collect();
    assert_eq!(middle_kinds, ["session_started", "turn_started"]);
}

#[test]
fn an_invalid_moved_parent_is_a_pointer() {
    // A `project` that is not a string cannot be written by `Log`: the
    // corrupt line is laid down raw, and the walk fails closed.
    let tmp = TestDir::new("history-moved-bad-parent");
    let dir = tmp.session(&id("s_bbbbbbbbbbbbbbbb"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("events.jsonl"),
        r#"{"kind":"session_started","session_id":"s_bbbbbbbbbbbbbbbb","ts":0,"schema_version":1,"seq":0,"payload":{"workspace":"/w","variables":{"path":"/usr/bin","names":[],"source":"inherited"},"forked_from":{"session_id":"s_aaaaaaaaaaaaaaaa","seq":1,"project":7}}}
"#,
    )
    .unwrap();
    let error = log::history(&dir).unwrap_err();
    assert!(matches!(error, log::Error::Pointer { .. }));
    assert_eq!(error.code(), contract::ErrorCode::LogCorrupt);
}

#[test]
fn a_parent_in_an_unknown_project_is_not_found() {
    let tmp = TestDir::new("history-moved-unknown-project");
    let clock = fakes::clock::FakeClock::new();
    let new_sessions = project_sessions(&tmp, "-work-new");
    let child = log::Log::create(&new_sessions, id("s_bbbbbbbbbbbbbbbb"), clock).unwrap();
    child
        .append(
            &moved_started("s_aaaaaaaaaaaaaaaa", 0, Some(json!("-work-gone"))),
            None,
            None,
        )
        .unwrap();
    drop(child);
    let error = log::history(&new_sessions.join("s_bbbbbbbbbbbbbbbb")).unwrap_err();
    assert!(matches!(error, log::Error::NotFound(_)));
}

#[test]
fn an_empty_parent_log_is_not_found() {
    let tmp = TestDir::new("history-moved-empty-parent");
    let clock = fakes::clock::FakeClock::new();
    let log = log::Log::create(tmp.path(), id("s_bbbbbbbbbbbbbbbb"), clock.clone()).unwrap();
    log.append(&forked_started("s_aaaaaaaaaaaaaaaa", 0), None, None)
        .unwrap();
    drop(log);
    let parent = tmp.session(&id("s_aaaaaaaaaaaaaaaa"));
    std::fs::create_dir_all(&parent).unwrap();
    std::fs::write(parent.join("events.jsonl"), b"").unwrap();
    let error = log::history(&tmp.session(&id("s_bbbbbbbbbbbbbbbb"))).unwrap_err();
    assert!(matches!(error, log::Error::NotFound(_)));
}

#[test]
fn a_moved_link_fsyncs_its_barrier_and_its_write() {
    let tmp = TestDir::new("history-moved-fsyncs");
    let clock = fakes::clock::FakeClock::new();
    let log = log::Log::create(tmp.path(), id("s_aaaaaaaaaaaaaaaa"), clock).unwrap();
    log.append(&session_started(), None, None).unwrap();
    let base = log.fsyncs();
    log.append(
        &event(
            "job_completed",
            json!({"job_id": "j_1", "status": "cancelled"}),
        ),
        None,
        None,
    )
    .unwrap();
    assert_eq!(log.fsyncs(), base, "an end waits for the next fsync");
    log.append(
        &moved_line("s_bbbbbbbbbbbbbbbb", 1, "/work/new"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(log.fsyncs(), base + 2, "the barrier plus the write itself");
}
