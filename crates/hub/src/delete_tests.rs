//! Tests for `delete`: what it removes and leaves, the argument shape
//! (including the confirmed `expect` set), a linked session directory,
//! held sessions, dependents and `cascade`, and the feed entry it drops.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use serde_json::json;

use super::*;
use crate::connection::serve_connection;
use crate::fake::{FakeStarter, status};
use crate::recent::{self, RecentRow};
use crate::testkit::{args, id};

/// One named deadline per receive: the hub answers before it.
const DEADLINE: Duration = Duration::from_secs(10);

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::home("hd");
        Self { dir, held }
    }

    fn hub(&self) -> Arc<Hub> {
        let timed: Arc<dyn Clock> = fakes::clock::FakeClock::new();
        Arc::new(crate::testkit::hub(
            &self.dir,
            Arc::new(FakeStarter::bind_and_hold(&self.dir)),
            timed,
        ))
    }

    fn dir_of(&self, project: &str, n: u64) -> PathBuf {
        log::session_dir(&self.dir, project, &SessionId(id(n)))
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

/// Holds session `dir`'s lock, as a running session process does.
fn hold(dir: &std::path::Path) -> File {
    let file = File::open(dir.join("session.lock")).unwrap();
    file.try_lock().unwrap();
    file
}

fn refused(hub: &Hub, value: serde_json::Value) -> Refused {
    delete(hub, &args(value)).unwrap_err()
}

fn dry_run(hub: &Hub, value: serde_json::Value) -> Value {
    delete(hub, &args(value))
        .unwrap()
        .expect("a dry run answers")
}

/// The answer's `sessions` as a set: `docs/invocation.md` ("The hub")
/// promises the ids, not their order.
fn sessions_as_set(answer: &Value) -> BTreeSet<String> {
    answer["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
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
    let refused = refused(&hub, json!({"session": id(2)}));
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    assert!(refused.message.contains(&id(2)), "{}", refused.message);
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
        json!({"session": id(1), "dry_run": "yes"}),
        json!({"session": id(1), "dry_run": null}),
        json!({"session": id(1), "dry_run": 1}),
    ] {
        let refused = refused(&hub, bad.clone());
        assert_eq!(refused.code, ErrorCode::InvalidArguments, "{bad}");
    }
    assert!(dir.join("events.jsonl").is_file());
}

/// Appends a `moved` last line to `dir`'s log naming `next`: the parent's
/// post-move state, as a hub delete meets it.
fn append_moved(dir: &Path, old: u64, next: u64) {
    let line = json!({"kind": "moved", "seq": 3, "session_id": id(old), "ts": 1,
        "payload": {"new_session_id": id(next), "seq": 2, "workspace": "/w2"}});
    let mut log = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    log.push_str(&format!("{line}\n"));
    fs::write(dir.join("events.jsonl"), log).unwrap();
}

#[test]
fn a_moved_child_blocks_its_parent_without_cascade() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    append_moved(&root, 1, 3);
    let moved = temp.session("-q", 3, Some(1));
    let hub = temp.hub();
    // The moved child continues the parent across projects: deleting the
    // parent refuses, as with any dependent.
    let refused = refused(&hub, json!({"session": id(1)}));
    assert_eq!(refused.code, ErrorCode::SessionHasDependents);
    assert_eq!(
        refused.message,
        format!(
            "Session `{}` has sessions that continue it: `{}`. `--cascade` deletes them too.",
            id(1),
            id(3)
        )
    );
    for dir in [&root, &moved] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
    // A session nothing continues is deleted without cascade.
    assert_eq!(delete(&hub, &args(json!({"session": id(3)}))), Ok(None));
    assert!(!moved.exists());
}

#[test]
fn cascade_removes_a_moved_parent_and_its_cross_project_child() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    append_moved(&root, 1, 3);
    let moved = temp.session("-q", 3, Some(1));
    let other = temp.session("-p", 5, None);
    let hub = temp.hub();
    let got = delete(&hub, &args(json!({"session": id(1), "cascade": true})));
    assert_eq!(got, Ok(None));
    for dir in [&root, &moved] {
        assert!(!dir.exists(), "{}", dir.display());
    }
    assert!(other.join("events.jsonl").is_file());
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
    let refused = refused(
        &hub,
        json!({"session": id(2), "cascade": false, "expect": [id(2), id(3)]}),
    );
    assert_eq!(refused.code, ErrorCode::SessionHasDependents);
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
    let refused = refused(&hub, json!({"session": id(1)}));
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
    assert!(target.join("events.jsonl").is_file());
    assert!(target.join("artifacts/a_1.txt").is_file());
}

#[test]
fn a_dry_run_answers_the_set_and_deletes_nothing() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let other = temp.session("-p", 3, None);
    let hub = temp.hub();
    let answer = dry_run(
        &hub,
        json!({"session": id(1), "cascade": true, "dry_run": true}),
    );
    assert_eq!(sessions_as_set(&answer), BTreeSet::from([id(1), id(2)]));
    for dir in [&root, &fork, &other] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
        assert!(dir.join("session.lock").is_file(), "{}", dir.display());
    }
    assert!(fork.join("artifacts/a_1.txt").is_file());
    // Without `cascade` the dry run names only the session itself.
    let answer = dry_run(&hub, json!({"session": id(3), "dry_run": true}));
    assert_eq!(answer, json!({"sessions": [id(3)]}));
    assert!(other.join("events.jsonl").is_file());
}

#[test]
fn a_dry_run_keeps_the_refusals_and_still_removes_nothing() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let hub = temp.hub();
    // Dependents without `cascade` refuse, as a real delete does.
    let dependents = refused(&hub, json!({"session": id(1), "dry_run": true}));
    assert_eq!(dependents.code, ErrorCode::SessionHasDependents);
    // A divergent confirmed set refuses, as a real delete does.
    let stale = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": [id(1)], "dry_run": true}),
    );
    assert_eq!(stale.code, ErrorCode::StaleRequest);
    // A held session refuses, as a real delete does.
    let _lock = hold(&root);
    let held = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "dry_run": true}),
    );
    assert_eq!(held.code, ErrorCode::SessionHeld);
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn a_dry_run_over_the_wire_answers_the_sessions() {
    let temp = Temp::new();
    temp.session("-p", 1, None);
    temp.session("-p", 2, Some(1));
    let hub = temp.hub();
    let line = json!({
        "id": "c_dry",
        "command": "delete",
        "args": {"session": id(1), "cascade": true, "dry_run": true}
    });
    let answer = over_the_wire(&hub, &line.to_string());
    assert_eq!(answer["kind"], "command_accepted");
    assert_eq!(answer["payload"]["command_id"], "c_dry");
    assert_eq!(
        sessions_as_set(&answer["payload"]["result"]),
        BTreeSet::from([id(1), id(2)])
    );
}

/// Every path under `dir`, sorted: file bytes for files, so a comparison
/// catches anything a dry run writes.
fn snapshot_tree(dir: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        for entry in fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            let entry_path = entry.path();
            let relative = entry_path
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if entry.file_type().unwrap().is_dir() {
                stack.push(entry_path);
                out.push((relative, None));
            } else {
                out.push((relative, Some(fs::read(&entry_path).unwrap())));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_dry_run_on_a_lockless_tree_answers_and_writes_nothing() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    for dir in [&root, &fork] {
        fs::remove_file(dir.join("session.lock")).unwrap();
    }
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);
    let answer = dry_run(
        &hub,
        json!({"session": id(1), "cascade": true, "dry_run": true}),
    );
    assert_eq!(sessions_as_set(&answer), BTreeSet::from([id(1), id(2)]));
    assert_eq!(snapshot_tree(&temp.dir), before);
    for dir in [&root, &fork] {
        assert!(!dir.join("session.lock").exists(), "{}", dir.display());
    }
}

#[test]
fn a_dry_run_keeps_the_feed_entry() {
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
    let answer = dry_run(&hub, json!({"session": id(1), "dry_run": true}));
    assert_eq!(answer, json!({"sessions": [id(1)]}));
    // The entry is still in the feed: nothing was forgotten.
    assert_eq!(hub.feed.dismiss(&args(json!({"session": id(1)}))), Ok(None));
    hub.feed.stop();
}

#[test]
fn a_held_session_is_refused_and_untouched() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let hub = temp.hub();
    let lock = hold(&dir);
    let refused = refused(&hub, json!({"session": id(1)}));
    assert_eq!(refused.code, ErrorCode::SessionHeld);
    assert!(refused.message.contains(&id(1)), "{}", refused.message);
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
    let refused = refused(&hub, json!({"session": id(1)}));
    assert_eq!(refused.code, ErrorCode::IoFailed);
    assert!(refused.message.contains(&id(1)), "{}", refused.message);
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
    let named = refused(&hub, json!({"session": id(1)}));
    assert_eq!(named.code, ErrorCode::SessionHasDependents);
    assert_eq!(
        named.message,
        format!(
            "Session `{}` has sessions that continue it: `{}`, `{}`, `{}`. `--cascade` deletes them too.",
            id(1),
            id(2),
            id(3),
            id(4)
        )
    );
    let refused = refused(&hub, json!({"session": id(1), "cascade": false}));
    assert_eq!(refused.code, ErrorCode::SessionHasDependents);
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
    let refused = refused(
        &hub,
        json!({"session": id(5), "cascade": true, "expect": [id(5), id(2), id(3)]}),
    );
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    assert_eq!(
        refused.message,
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
    let refused = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": [id(1), id(2), id(7)]}),
    );
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn an_empty_expect_with_cascade_is_stale() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let hub = temp.hub();
    let refused = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": []}),
    );
    assert_eq!(refused.code, ErrorCode::StaleRequest);
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
    let refused = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": [id(1), id(2)]}),
    );
    assert_eq!(refused.code, ErrorCode::StaleRequest);
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
    let refused = refused(
        &hub,
        json!({"session": id(9), "cascade": true, "expect": [id(1)]}),
    );
    assert_eq!(refused.code, ErrorCode::SessionNotFound);
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
    let refused = refused(&hub, json!({"session": id(1), "cascade": true}));
    assert_eq!(refused.code, ErrorCode::SessionHeld);
    assert!(refused.message.contains(&id(2)), "{}", refused.message);
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
    let refused = refused(&hub, json!({"session": id(1), "cascade": true}));
    fs::set_permissions(&fork, perms).unwrap();
    assert_eq!(refused.code, ErrorCode::IoFailed);
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
    let refused = hub
        .feed
        .dismiss(&args(json!({"session": id(1)})))
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    hub.feed.stop();
}
