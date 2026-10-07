//! Tests for `delete`: what it removes and leaves, the argument shape
//! (including the confirmed `expect` set), a linked session directory,
//! held sessions, dependents and `cascade`, and the feed entry it drops.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::json;

use super::*;
use crate::connection::serve_connection;
use crate::diag::Diag;
use crate::fake::{FakeStarter, status};
use crate::recent::{self, RecentRow};

/// One named deadline per receive: the hub answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hd");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    fn hub(&self) -> Arc<Hub> {
        let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
        Arc::new(Hub::new(
            &self.dir,
            "0.0.0",
            Arc::new(FakeStarter::bind_and_hold(&self.dir)),
            Arc::clone(&timed),
            Diag::open(&self.dir, timed),
        ))
    }

    fn dir_of(&self, project: &str, n: u64) -> PathBuf {
        recent::session_dir(&self.dir, project, &id(n))
    }

    /// Session `n` in `project`, continuing session `from` when given: its
    /// log, an artifact and an unheld lock file.
    fn session(&self, project: &str, n: u64, from: Option<u64>) -> PathBuf {
        let dir = self.dir_of(project, n);
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        let mut payload = json!({"workspace": "/w"});
        if let Some(from) = from {
            payload["forked_from"] = json!({"session_id": id(from), "seq": 2});
        }
        let first =
            json!({"kind": "session_started", "seq": 0, "session_id": id(n), "payload": payload});
        fs::write(dir.join("events.jsonl"), format!("{first}\n")).unwrap();
        fs::write(dir.join("artifacts").join("a_1.txt"), b"bytes").unwrap();
        fs::write(dir.join("session.lock"), b"").unwrap();
        dir
    }
}

fn id(n: u64) -> String {
    format!("s_{n:016x}")
}

fn args(value: serde_json::Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

/// Holds session `dir`'s lock, as a running session process does.
fn hold(dir: &std::path::Path) -> File {
    let file = File::open(dir.join("session.lock")).unwrap();
    file.try_lock().unwrap();
    file
}

fn refused(hub: &Hub, value: serde_json::Value) -> Refusal {
    delete(hub, &args(value)).unwrap_err()
}

/// Sends one line on a served connection and returns the answer after
/// `hub_hello`.
fn over_the_wire(hub: &Arc<Hub>, line: &str) -> Value {
    let (a, b) = UnixStream::pair().unwrap();
    let serving = Arc::clone(hub);
    thread::spawn(move || serve_connection(a, serving));
    b.set_read_timeout(Some(DEADLINE)).unwrap();
    (&b).write_all(format!("{line}\n").as_bytes()).unwrap();
    let mut read = BufReader::new(b);
    let mut next = || {
        let mut text = String::new();
        read.read_line(&mut text).unwrap();
        serde_json::from_str::<Value>(&text).unwrap()
    };
    assert_eq!(next()["kind"], "hub_hello");
    next()
}

#[test]
fn delete_removes_the_directory_and_is_accepted_with_no_result() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let keeper = temp.session("-p", 2, None);
    let worktree = temp.dir.join("projects/-p/worktrees").join(id(1));
    fs::create_dir_all(&worktree).unwrap();
    fs::write(worktree.join("file"), b"kept").unwrap();
    let row = RecentRow {
        session_id: SessionId(id(1)),
        ts: 1,
        project: "-p".to_owned(),
        workspace: "/w".to_owned(),
        name: "n".to_owned(),
        how: crate::Left::Exited,
        status: None,
    };
    recent::append(&temp.dir, &row).unwrap();
    let rows = fs::read(temp.dir.join("recent.jsonl")).unwrap();
    let hub = temp.hub();
    let line = json!({"id": "1", "command": "delete", "args": {"session": id(1)}});
    let answer = over_the_wire(&hub, &line.to_string());
    assert_eq!(answer["kind"], "command_accepted");
    assert_eq!(answer["payload"], json!({"command_id": "1"}));
    assert!(!dir.exists(), "the log and its artifacts are gone");
    assert!(
        keeper.join("events.jsonl").is_file(),
        "another session stays"
    );
    assert_eq!(fs::read(worktree.join("file")).unwrap(), b"kept");
    assert_eq!(fs::read(temp.dir.join("recent.jsonl")).unwrap(), rows);
}

#[test]
fn a_refusal_over_the_wire_carries_its_code() {
    let temp = Temp::new();
    let hub = temp.hub();
    let line = json!({"id": "7", "command": "delete", "args": {"session": id(9)}});
    let answer = over_the_wire(&hub, &line.to_string());
    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["command_id"], "7");
    assert_eq!(answer["payload"]["code"], "session_not_found");
}

#[test]
fn a_session_in_no_project_is_not_found() {
    let temp = Temp::new();
    temp.session("-p", 1, None);
    let hub = temp.hub();
    let (code, message) = refused(&hub, json!({"session": id(2)}));
    assert_eq!(code, ErrorCode::SessionNotFound);
    assert!(message.contains(&id(2)), "{message}");
}

#[test]
fn arguments_that_do_not_fit_are_invalid_and_remove_nothing() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let hub = temp.hub();
    for bad in [
        json!({}),
        json!({"session": 1}),
        json!({"session": id(1), "cascade": "yes"}),
        json!({"session": id(1), "cascade": null}),
        json!({"session": id(1), "extra": true}),
        json!({"session": "../x"}),
        json!({"session": "a/b"}),
        json!({"session": format!("../{}", id(1))}),
        json!({"session": "s_0123"}),
        json!({"session": id(1), "cascade": true, "expect": id(1)}),
        json!({"session": id(1), "cascade": true, "expect": null}),
        json!({"session": id(1), "cascade": true, "expect": {}}),
        json!({"session": id(1), "cascade": true, "expect": 1}),
        json!({"session": id(1), "cascade": true, "expect": [1]}),
        json!({"session": id(1), "cascade": true, "expect": [id(1), null]}),
        json!({"session": id(1), "cascade": true, "expect": ["../x"]}),
        json!({"session": id(1), "cascade": true, "expect": ["s_0123"]}),
        json!({"session": id(1), "cascade": true, "expect": [format!("../{}", id(1))]}),
    ] {
        let (code, _) = refused(&hub, bad.clone());
        assert_eq!(code, ErrorCode::InvalidArguments, "{bad}");
    }
    assert!(dir.join("events.jsonl").is_file());
}

#[test]
fn expect_without_cascade_is_checked_but_not_compared() {
    let temp = Temp::new();
    temp.session("-p", 1, None);
    let root = temp.session("-p", 2, None);
    let fork = temp.session("-p", 3, Some(2));
    let hub = temp.hub();
    // No dependents: a confirmed set naming another session still deletes.
    assert_eq!(
        delete(&hub, &args(json!({"session": id(1), "expect": [id(9)]}))),
        Ok(None)
    );
    // Dependents still refuse without `cascade`, even with a matching set.
    let (code, _) = refused(
        &hub,
        json!({"session": id(2), "cascade": false, "expect": [id(2), id(3)]}),
    );
    assert_eq!(code, ErrorCode::SessionHasDependents);
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn a_linked_session_directory_is_not_found_and_its_target_is_untouched() {
    let temp = Temp::new();
    let outside = temp.session("-elsewhere", 1, None);
    let sessions = temp.dir.join("projects/-p/sessions");
    fs::create_dir_all(&sessions).unwrap();
    // A whole session moved outside `projects/`, linked in as `id(1)`.
    let target = temp.dir.join("outside");
    fs::rename(&outside, &target).unwrap();
    std::os::unix::fs::symlink(&target, sessions.join(id(1))).unwrap();
    let hub = temp.hub();
    let (code, _) = refused(&hub, json!({"session": id(1)}));
    assert_eq!(code, ErrorCode::SessionNotFound);
    assert!(target.join("events.jsonl").is_file());
    assert!(target.join("artifacts/a_1.txt").is_file());
}

#[test]
fn a_held_session_is_refused_and_untouched() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let hub = temp.hub();
    let lock = hold(&dir);
    let (code, message) = refused(&hub, json!({"session": id(1)}));
    assert_eq!(code, ErrorCode::SessionHeld);
    assert!(message.contains(&id(1)), "{message}");
    assert!(dir.join("events.jsonl").is_file());
    drop(lock);
    assert_eq!(delete(&hub, &args(json!({"session": id(1)}))), Ok(None));
    assert!(!dir.exists());
}

#[test]
fn a_session_with_no_lock_file_is_not_held() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    fs::remove_file(dir.join("session.lock")).unwrap();
    let hub = temp.hub();
    assert_eq!(delete(&hub, &args(json!({"session": id(1)}))), Ok(None));
    assert!(!dir.exists());
}

#[test]
fn a_lock_that_cannot_be_opened_is_io_failed_and_removes_nothing() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let lock = dir.join("session.lock");
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    let hub = temp.hub();
    let (code, message) = refused(&hub, json!({"session": id(1)}));
    assert_eq!(code, ErrorCode::IoFailed);
    assert!(message.contains(&id(1)), "{message}");
    assert!(lock.is_dir());
    assert!(dir.join("events.jsonl").is_file());
    assert_eq!(fs::read(dir.join("artifacts/a_1.txt")).unwrap(), b"bytes");
}

#[test]
fn dependents_refuse_without_cascade_naming_every_one_sorted() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-q", 3, Some(1));
    let rewind = temp.session("-p", 2, Some(1));
    let deep = temp.session("-p", 4, Some(3));
    let hub = temp.hub();
    let (code, message) = refused(&hub, json!({"session": id(1)}));
    assert_eq!(code, ErrorCode::SessionHasDependents);
    assert_eq!(
        message,
        format!(
            "Session `{}` has sessions that continue it: `{}`, `{}`, `{}`. `--cascade` deletes them too.",
            id(1),
            id(2),
            id(3),
            id(4)
        )
    );
    let (code, _) = refused(&hub, json!({"session": id(1), "cascade": false}));
    assert_eq!(code, ErrorCode::SessionHasDependents);
    for dir in [&root, &fork, &rewind, &deep] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
    // A session nothing continues is deleted without cascade.
    assert_eq!(delete(&hub, &args(json!({"session": id(4)}))), Ok(None));
    assert!(!deep.exists());
}

#[test]
fn cascade_removes_the_session_and_everything_that_continues_it() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-q", 3, Some(1));
    let rewind = temp.session("-p", 2, Some(1));
    let deep = temp.session("-p", 4, Some(3));
    let other = temp.session("-p", 5, None);
    let hub = temp.hub();
    let got = delete(&hub, &args(json!({"session": id(1), "cascade": true})));
    assert_eq!(got, Ok(None));
    for dir in [&root, &fork, &rewind, &deep] {
        assert!(!dir.exists(), "{}", dir.display());
    }
    assert!(other.join("events.jsonl").is_file());
}

#[test]
fn cascade_with_the_confirmed_set_deletes_exactly_it() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-q", 3, Some(1));
    let rewind = temp.session("-p", 2, Some(1));
    let deep = temp.session("-p", 4, Some(3));
    let other = temp.session("-p", 5, None);
    let hub = temp.hub();
    let got = delete(
        &hub,
        &args(
            json!({"session": id(1), "cascade": true, "expect": [id(4), id(1), id(3), id(2), id(1)]}),
        ),
    );
    assert_eq!(got, Ok(None));
    for dir in [&root, &fork, &rewind, &deep] {
        assert!(!dir.exists(), "{}", dir.display());
    }
    assert!(other.join("events.jsonl").is_file());
}

#[test]
fn cascade_with_no_dependents_and_expect_naming_only_it_deletes_it() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let hub = temp.hub();
    assert_eq!(
        delete(
            &hub,
            &args(json!({"session": id(1), "cascade": true, "expect": [id(1)]}))
        ),
        Ok(None)
    );
    assert!(!dir.exists());
}

#[test]
fn a_dependent_added_after_the_question_is_stale_and_deletes_nothing() {
    let temp = Temp::new();
    let root = temp.session("-p", 5, None);
    let fork = temp.session("-p", 2, Some(5));
    let other = temp.session("-p", 3, Some(5));
    let hub = temp.hub();
    let late = temp.session("-p", 1, Some(3));
    let (code, message) = refused(
        &hub,
        json!({"session": id(5), "cascade": true, "expect": [id(5), id(2), id(3)]}),
    );
    assert_eq!(code, ErrorCode::StaleRequest);
    assert_eq!(
        message,
        format!(
            "The sessions this delete would remove are now `{}`, `{}`, `{}`, `{}`. Nothing was deleted.",
            id(1),
            id(2),
            id(3),
            id(5)
        )
    );
    for dir in [&root, &fork, &other, &late] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
    assert_eq!(
        delete(
            &hub,
            &args(
                json!({"session": id(5), "cascade": true, "expect": [id(5), id(2), id(3), id(1)]}),
            )
        ),
        Ok(None)
    );
    for dir in [&root, &fork, &other, &late] {
        assert!(!dir.exists(), "{}", dir.display());
    }
}

#[test]
fn an_expect_naming_more_than_the_set_is_stale() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let hub = temp.hub();
    let (code, _) = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": [id(1), id(2), id(7)]}),
    );
    assert_eq!(code, ErrorCode::StaleRequest);
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn an_empty_expect_with_cascade_is_stale() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let hub = temp.hub();
    let (code, _) = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": []}),
    );
    assert_eq!(code, ErrorCode::StaleRequest);
    assert!(dir.join("events.jsonl").is_file());
}

#[test]
fn a_stale_expect_is_refused_before_a_held_dependent() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let deep = temp.session("-p", 3, Some(1));
    let hub = temp.hub();
    let _lock = hold(&fork);
    let (code, _) = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": [id(1), id(2)]}),
    );
    assert_eq!(code, ErrorCode::StaleRequest);
    for dir in [&root, &fork, &deep] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
    // No session lock was left taken by the refusal.
    drop(hold(&root));
    drop(hold(&deep));
}

#[test]
fn a_missing_root_is_not_found_even_with_a_stale_expect() {
    let temp = Temp::new();
    temp.session("-p", 1, None);
    let hub = temp.hub();
    let (code, _) = refused(
        &hub,
        json!({"session": id(9), "cascade": true, "expect": [id(1)]}),
    );
    assert_eq!(code, ErrorCode::SessionNotFound);
}

#[test]
fn a_stale_expect_over_the_wire_is_rejected_with_its_message() {
    let temp = Temp::new();
    temp.session("-p", 1, None);
    temp.session("-p", 2, Some(1));
    let hub = temp.hub();
    let line = json!({
        "id": "c_1",
        "command": "delete",
        "args": {"session": id(1), "cascade": true, "expect": [id(1)]}
    });
    let answer = over_the_wire(&hub, &line.to_string());
    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["command_id"], "c_1");
    assert_eq!(answer["payload"]["code"], "stale_request");
    let message = answer["payload"]["message"].as_str().unwrap();
    assert!(message.contains(&id(1)), "{message}");
    assert!(message.contains(&id(2)), "{message}");
}

#[test]
fn cascade_with_one_held_dependent_removes_nothing() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let deep = temp.session("-p", 3, Some(2));
    let hub = temp.hub();
    let _lock = hold(&fork);
    let (code, message) = refused(&hub, json!({"session": id(1), "cascade": true}));
    assert_eq!(code, ErrorCode::SessionHeld);
    assert!(message.contains(&id(2)), "{message}");
    for dir in [&root, &fork, &deep] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
    // The locks the hub took before the refusal were let go.
    drop(hold(&root));
    drop(hold(&deep));
}

#[test]
fn a_removal_that_fails_keeps_every_session_it_continues() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    // A directory whose entries cannot be unlinked: its removal fails.
    let perms = fs::metadata(&fork).unwrap().permissions();
    fs::set_permissions(&fork, std::os::unix::fs::PermissionsExt::from_mode(0o500)).unwrap();
    let hub = temp.hub();
    let (code, _) = refused(&hub, json!({"session": id(1), "cascade": true}));
    fs::set_permissions(&fork, perms).unwrap();
    assert_eq!(code, ErrorCode::IoFailed);
    // The dependent went first and failed, so the session it continues is
    // still whole.
    assert!(root.join("events.jsonl").is_file());
    assert!(fork.join("events.jsonl").is_file());
}

#[test]
fn a_crashed_feed_entry_is_dropped_by_delete() {
    let temp = Temp::new();
    temp.session("-p", 1, None);
    let row = RecentRow {
        session_id: SessionId(id(1)),
        ts: 1,
        project: "-p".to_owned(),
        workspace: "/w".to_owned(),
        name: "n".to_owned(),
        how: crate::Left::Crashed,
        status: Some(serde_json::from_value(status("n", "/w", "idle", None)).unwrap()),
    };
    recent::append(&temp.dir, &row).unwrap();
    let hub = temp.hub();
    hub.feed.start();
    assert_eq!(delete(&hub, &args(json!({"session": id(1)}))), Ok(None));
    // A crashed entry still in the feed would be dismissed.
    let (code, _) = hub
        .feed
        .dismiss(&args(json!({"session": id(1)})))
        .unwrap_err();
    assert_eq!(code, ErrorCode::StaleRequest);
    hub.feed.stop();
}
