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
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use contract::clock::Clock;
use fakes::{Deadline, MUST_SUCCEED_WITHIN};
use serde_json::json;

use super::*;
use crate::connection::{Accept, serve_counted};
use crate::fake::{FakeStarter, status};
use crate::feed::{Closer, Feed};
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

/// Writes `label` into session `dir`'s lock file, as a session process
/// does when it takes the lock.
fn label(dir: &std::path::Path, label: &str) {
    fs::write(dir.join("session.lock"), format!("{label}\n")).unwrap();
}

/// This test process's pid as a lock label, as a session process writes
/// it when it takes the lock.
fn own_pid_label() -> String {
    std::process::id().to_string()
}

fn refused(hub: &Hub, value: serde_json::Value) -> DeleteRefused {
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

/// The dry-run answer's `held` ids as a set: `docs/invocation.md` ("The hub")
/// promises the ids it would remove that another process holds right now,
/// not their order.
fn held_as_set(answer: &Value) -> BTreeSet<String> {
    answer["held"]
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
    thread::spawn(
        move || match serving.poll_accept(&a, &AtomicBool::new(false)) {
            Accept::Counted(n) => serve_counted(a, serving, n),
            Accept::Dropped => panic!("test connection stream could not be cloned"),
            Accept::Exiting => panic!("fresh test hub unexpectedly exiting"),
        },
    );
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
        archived: false,
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
fn delete_removes_an_archived_session_and_keeps_its_row() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    // The archived marker and row change nothing about deletion.
    fs::write(
        dir.join("events.jsonl"),
        format!(
            "{}{{\"kind\":\"archived\",\"seq\":1,\"session_id\":\"{}\"}}\n",
            fs::read_to_string(dir.join("events.jsonl")).unwrap(),
            id(1),
        ),
    )
    .unwrap();
    let row = RecentRow {
        session_id: SessionId(id(1)),
        ts: 1,
        project: "-p".to_owned(),
        workspace: "/w".to_owned(),
        name: "n".to_owned(),
        how: crate::Left::Exited,
        status: None,
        archived: true,
    };
    recent::append(&temp.dir, &row).unwrap();
    let rows = fs::read(temp.dir.join("recent.jsonl")).unwrap();
    let hub = temp.hub();
    let line = json!({"id": "1", "command": "delete", "args": {"session": id(1)}});
    let answer = over_the_wire(&hub, &line.to_string());
    assert_eq!(answer["kind"], "command_accepted");
    assert!(!dir.exists(), "the log is gone with its archive marker");
    // The derived row stays: listings skip the missing directory.
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
    assert!(answer["payload"].get("held").is_none());

    let generic_session_held = crate::connection::rejection(
        Some(&contract::CommandId("generic".to_owned())),
        &ErrorCode::SessionHeld,
        "A session is held.",
    );
    assert!(generic_session_held.get("held").is_none());
}

#[test]
fn a_session_in_no_project_is_not_found() {
    let temp = Temp::new();
    temp.session("-p", 1, None);
    let hub = temp.hub();
    let refused = refused(&hub, json!({"session": id(2)}));
    assert_eq!(refused.code(), ErrorCode::SessionNotFound);
    assert!(refused.message().contains(&id(2)), "{}", refused.message());
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
        assert_eq!(refused.code(), ErrorCode::InvalidArguments, "{bad}");
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
    assert_eq!(refused.code(), ErrorCode::SessionHasDependents);
    assert_eq!(
        refused.message(),
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
    assert_eq!(refused.code(), ErrorCode::SessionHasDependents);
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
    assert_eq!(refused.code(), ErrorCode::SessionNotFound);
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
    assert_eq!(answer["held"], json!([]));
    for dir in [&root, &fork, &other] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
        assert!(dir.join("session.lock").is_file(), "{}", dir.display());
    }
    assert!(fork.join("artifacts/a_1.txt").is_file());
    // Without `cascade` the dry run names only the session itself.
    let answer = dry_run(&hub, json!({"session": id(3), "dry_run": true}));
    assert_eq!(answer, json!({"sessions": [id(3)], "held": []}));
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
    assert_eq!(dependents.code(), ErrorCode::SessionHasDependents);
    // A divergent confirmed set refuses, as a real delete does.
    let stale = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": [id(1)], "dry_run": true}),
    );
    assert_eq!(stale.code(), ErrorCode::StaleRequest);
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn a_dry_run_reports_held_candidates_and_removes_nothing() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let unrelated = temp.session("-p", 3, None);
    let mine = own_pid_label();
    label(&root, &mine);
    label(&fork, &format!("keeper {mine}"));
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(
        &hub,
        json!({"session": id(1), "cascade": true, "dry_run": true}),
    );
    assert_eq!(sessions_as_set(&answer), BTreeSet::from([id(1), id(2)]));
    assert_eq!(held_as_set(&answer), BTreeSet::from([id(1), id(2)]));
    assert_eq!(snapshot_tree(&temp.dir), before, "dry run writes nothing");
    for dir in [&root, &fork, &unrelated] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn a_real_delete_refuses_held_candidates_without_changes() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let unrelated = temp.session("-p", 3, None);
    let _locks = [hold(&root), hold(&fork), hold(&unrelated)];
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let refused = refused(&hub, json!({"session": id(1), "cascade": true}));
    assert_eq!(refused.code(), ErrorCode::SessionHeld);
    assert_eq!(
        snapshot_tree(&temp.dir),
        before,
        "real delete removes nothing"
    );
    for dir in [&root, &fork, &unrelated] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn a_dry_run_reports_a_held_dependent_without_removing_unheld_root() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    label(&fork, &own_pid_label());
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(
        &hub,
        json!({"session": id(1), "cascade": true, "dry_run": true}),
    );
    assert_eq!(sessions_as_set(&answer), BTreeSet::from([id(1), id(2)]));
    assert_eq!(held_as_set(&answer), BTreeSet::from([id(2)]));
    assert_eq!(snapshot_tree(&temp.dir), before);
    assert!(root.join("events.jsonl").is_file());
    assert!(fork.join("events.jsonl").is_file());
}

#[test]
fn a_real_delete_with_a_held_dependent_removes_nothing() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let _lock = hold(&fork);
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let refused = refused(&hub, json!({"session": id(1), "cascade": true}));
    assert_eq!(refused.code(), ErrorCode::SessionHeld);
    assert_eq!(
        snapshot_tree(&temp.dir),
        before,
        "the acquired root lock is released"
    );
    // The hub dropped the root lock it took before the refusal.
    drop(hold(&root));
    assert!(root.join("events.jsonl").is_file());
    assert!(fork.join("events.jsonl").is_file());
}

#[test]
fn a_dry_run_with_an_unreadable_lock_reads_not_held_and_writes_nothing() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    fs::remove_file(root.join("session.lock")).unwrap();
    fs::create_dir(root.join("session.lock")).unwrap();
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(&hub, json!({"session": id(1), "dry_run": true}));
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer, json!({"sessions": [id(1)], "held": []}));
    assert_eq!(snapshot_tree(&temp.dir), before);
}

#[test]
fn a_dry_run_over_the_wire_answers_the_sessions_and_held_ids() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    temp.session("-p", 2, Some(1));
    label(&root, &own_pid_label());
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
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer["payload"]["result"]["held"], json!([id(1)]));
}

#[test]
fn a_dry_run_reports_a_dead_pid_label_as_not_held() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    // Fits a pid but names no process: the probe reports no such process.
    label(&dir, "2147483647");
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(&hub, json!({"session": id(1), "dry_run": true}));
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer, json!({"sessions": [id(1)], "held": []}));
    assert_eq!(snapshot_tree(&temp.dir), before, "dry run writes nothing");
}

#[test]
fn a_dry_run_reports_a_dead_keeper_label_as_not_held() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    label(&dir, "keeper 2147483647");
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(&hub, json!({"session": id(1), "dry_run": true}));
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer, json!({"sessions": [id(1)], "held": []}));
    assert_eq!(snapshot_tree(&temp.dir), before, "dry run writes nothing");
}

#[test]
fn a_dry_run_reports_an_oversized_pid_label_as_not_held() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    // All digits, but past what a pid holds: no probe is sent for it.
    label(&dir, "99999999999");
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(&hub, json!({"session": id(1), "dry_run": true}));
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer, json!({"sessions": [id(1)], "held": []}));
    assert_eq!(snapshot_tree(&temp.dir), before, "dry run writes nothing");
}

#[test]
fn a_dry_run_reports_a_negative_pid_label_as_not_held() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    label(&dir, "-1");
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(&hub, json!({"session": id(1), "dry_run": true}));
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer, json!({"sessions": [id(1)], "held": []}));
    assert_eq!(snapshot_tree(&temp.dir), before, "dry run writes nothing");
}

#[test]
fn a_dry_run_reports_a_pid_with_trailing_words_as_not_held() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let mine = own_pid_label();
    // Live pids, but neither label form allows a third word.
    label(&root, &format!("{mine} extra"));
    label(&fork, &format!("keeper {mine} extra"));
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(
        &hub,
        json!({"session": id(1), "cascade": true, "dry_run": true}),
    );
    assert_eq!(sessions_as_set(&answer), BTreeSet::from([id(1), id(2)]));
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer["held"], json!([]));
    assert_eq!(snapshot_tree(&temp.dir), before, "dry run writes nothing");
}

#[test]
fn a_dry_run_reports_garbage_labels_as_not_held() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let deep = temp.session("-p", 3, Some(2));
    label(&root, "123abc");
    label(&fork, "keeper 42x");
    label(&deep, "keeper");
    let hub = temp.hub();
    let before = snapshot_tree(&temp.dir);

    let answer = dry_run(
        &hub,
        json!({"session": id(1), "cascade": true, "dry_run": true}),
    );
    assert_eq!(
        sessions_as_set(&answer),
        BTreeSet::from([id(1), id(2), id(3)])
    );
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer["held"], json!([]));
    assert_eq!(snapshot_tree(&temp.dir), before, "dry run writes nothing");
    for dir in [&root, &fork, &deep] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

/// A dry run takes no lock: the hook pauses the dry run inside its probe
/// window, so the concurrent open runs while the probe is in flight. The
/// channel rendezvous makes the overlap exact rather than timed, and the
/// open must succeed because the dry run holds no lock to refuse it.
#[test]
fn a_dry_run_takes_no_lock_while_a_concurrent_open_races_it() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let hub = temp.hub();
    let (fired, hook_fired) = mpsc::channel::<()>();
    let (release, release_hook) = mpsc::channel::<()>();
    let worker = thread::spawn(move || {
        // The seam is thread-local, so the dry-run thread arms it: the
        // hook fires inside the probe window and waits for the release.
        pause_dry_run(move || {
            fired.send(()).unwrap_or(());
            assert!(
                Deadline::after(MUST_SUCCEED_WITHIN)
                    .recv(&release_hook)
                    .is_ok(),
                "the test releases the probe"
            );
        });
        delete(&hub, &args(json!({"session": id(1), "dry_run": true})))
    });
    assert!(
        Deadline::after(MUST_SUCCEED_WITHIN)
            .recv(&hook_fired)
            .is_ok(),
        "the dry run probed its first candidate"
    );
    let sessions = dir.parent().unwrap().to_path_buf();
    let clock: Arc<dyn Clock> = fakes::clock::FakeClock::new();
    let opened = log::Log::open(&sessions, SessionId(id(1)), clock);
    let open_ok = opened.is_ok();
    drop(opened);
    release.send(()).unwrap_or(());
    let answer = match worker.join() {
        Ok(answer) => answer,
        Err(_) => panic!("the dry run thread ended"),
    };
    // (`docs/invocation.md`, "The hub"): dry-run `held` is the ids it would remove that another process holds right now.
    assert_eq!(answer, Ok(Some(json!({"sessions": [id(1)], "held": []}))));
    assert!(
        open_ok,
        "a concurrent open takes the lock the dry run must leave free"
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
    assert_eq!(answer["held"], json!([]));
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
        archived: false,
    };
    recent::append(&temp.dir, &row).unwrap();
    let hub = temp.hub();
    hub.feed.start();
    let answer = dry_run(&hub, json!({"session": id(1), "dry_run": true}));
    assert_eq!(answer, json!({"sessions": [id(1)], "held": []}));
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
    assert_eq!(refused.code(), ErrorCode::SessionHeld);
    assert!(refused.message().contains(&id(1)), "{}", refused.message());
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
    assert_eq!(refused.code(), ErrorCode::IoFailed);
    assert!(refused.message().contains(&id(1)), "{}", refused.message());
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
    assert_eq!(named.code(), ErrorCode::SessionHasDependents);
    assert_eq!(
        named.message(),
        format!(
            "Session `{}` has sessions that continue it: `{}`, `{}`, `{}`. `--cascade` deletes them too.",
            id(1),
            id(2),
            id(3),
            id(4)
        )
    );
    let refused = refused(&hub, json!({"session": id(1), "cascade": false}));
    assert_eq!(refused.code(), ErrorCode::SessionHasDependents);
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
    assert_eq!(refused.code(), ErrorCode::StaleRequest);
    assert_eq!(
        refused.message(),
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
    assert_eq!(refused.code(), ErrorCode::StaleRequest);
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
    assert_eq!(refused.code(), ErrorCode::StaleRequest);
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
    assert_eq!(refused.code(), ErrorCode::StaleRequest);
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
    assert_eq!(refused.code(), ErrorCode::SessionNotFound);
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
    assert!(
        answer["payload"].get("held").is_none(),
        "only a delete session_held refusal carries held"
    );
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
    assert_eq!(refused.code(), ErrorCode::SessionHeld);
    assert!(refused.message().contains(&id(2)), "{}", refused.message());
    for dir in [&root, &fork, &deep] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
    // The locks the hub took before the refusal were let go.
    drop(hold(&root));
    drop(hold(&deep));
}

#[test]
fn real_delete_with_one_held_keeps_the_single_message() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let hub = temp.hub();
    let _lock = hold(&fork);
    let refused = refused(&hub, json!({"session": id(1), "cascade": true}));
    assert_eq!(refused.code(), ErrorCode::SessionHeld);
    assert_eq!(
        refused.message(),
        format!("Session `{}` is held by another process.", id(2))
    );
    assert_eq!(refused.held(), Some([id(2)].as_slice()));
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn a_multi_session_held_delete_refusal_carries_ordered_ids_over_the_wire() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let deep = temp.session("-p", 3, Some(1));
    let hub = temp.hub();
    let _locks = [hold(&root), hold(&fork), hold(&deep)];
    let before = [&root, &fork, &deep].map(|dir| snapshot_tree(dir));
    let line = json!({
        "id": "c_delete",
        "command": "delete",
        "args": {"session": id(1), "cascade": true}
    });

    let answer = over_the_wire(&hub, &line.to_string());

    assert_eq!(answer["kind"], "command_rejected");
    assert_eq!(answer["payload"]["command_id"], "c_delete");
    assert_eq!(answer["payload"]["code"], "session_held");
    assert_eq!(
        answer["payload"]["message"],
        format!("Session `{}` is held by another process.", id(1))
    );
    assert_eq!(answer["payload"]["held"], json!([id(1), id(2), id(3)]));
    let refused = refused(&hub, json!({"session": id(1), "cascade": true}));
    assert!(matches!(refused, DeleteRefused::Held { .. }));
    assert_eq!(refused.held(), Some([id(1), id(2), id(3)].as_slice()));
    assert_eq!(
        [&root, &fork, &deep].map(|dir| snapshot_tree(dir)),
        before,
        "session data remains"
    );
}

/// Only a real `session_held` refusal carries `held`: every other
/// refusal is the generic shape, even when sessions are busy.
#[test]
fn only_a_session_held_refusal_carries_held_ids() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    // A stale `expect` refuses before the held check, with no `held`.
    let stale = refused(
        &temp.hub(),
        json!({"session": id(1), "cascade": true, "expect": [id(1)]}),
    );
    assert!(matches!(stale, DeleteRefused::Other(_)));
    assert_eq!(stale.held(), None);
    // A lock-probe failure refuses `io_failed` instead, with no `held`.
    let _lock = hold(&root);
    fs::remove_file(fork.join("session.lock")).unwrap();
    fs::create_dir(fork.join("session.lock")).unwrap();
    let failed = refused(&temp.hub(), json!({"session": id(1), "cascade": true}));
    assert!(matches!(failed, DeleteRefused::Other(_)));
    assert_eq!(failed.held(), None);
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn real_delete_io_error_after_a_busy_candidate_is_io_failed() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    let fork = temp.session("-p", 2, Some(1));
    let _lock = hold(&root);
    fs::remove_file(fork.join("session.lock")).unwrap();
    fs::create_dir(fork.join("session.lock")).unwrap();
    let hub = temp.hub();
    let refused = refused(&hub, json!({"session": id(1), "cascade": true}));
    assert_eq!(refused.code(), ErrorCode::IoFailed);
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
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
    assert_eq!(refused.code(), ErrorCode::IoFailed);
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
        archived: false,
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

/// A feed subscriber's far end: lines read under [`DEADLINE`]. The
/// feed is never started here: subscribing queues the snapshot, which
/// is empty with no entry, so the first line read is the broadcast.
struct Sub {
    read: BufReader<UnixStream>,
}

impl Sub {
    fn new(feed: &Arc<Feed>) -> Self {
        let (a, b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(DEADLINE)).unwrap();
        let closer = Closer {
            stream: a.try_clone().unwrap(),
            web: None,
        };
        feed.subscribe(Arc::new(Mutex::new(a)), closer).unwrap();
        Self {
            read: BufReader::new(b),
        }
    }

    /// The next line as JSON: it must arrive before the deadline.
    fn line(&mut self, what: &str) -> Value {
        let mut text = String::new();
        self.read
            .read_line(&mut text)
            .unwrap_or_else(|_| panic!("never received {what}"));
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("not a line: {text:?}"))
    }

    /// The next line within `within`, or `None` on timeout: a refusal
    /// must enqueue nothing.
    fn try_line(&mut self, within: Duration) -> Option<Value> {
        self.read.get_mut().set_read_timeout(Some(within)).unwrap();
        let mut text = String::new();
        let outcome = self.read.read_line(&mut text);
        self.read
            .get_mut()
            .set_read_timeout(Some(DEADLINE))
            .unwrap();
        match outcome {
            Ok(_) if text.is_empty() => None,
            Ok(_) => serde_json::from_str(&text).ok(),
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                None
            }
            Err(error) => panic!("reading the subscriber: {error}"),
        }
    }
}

#[test]
fn a_held_delete_notifies_nothing_then_the_real_delete_notifies() {
    let temp = Temp::new();
    let dir = temp.session("-p", 1, None);
    let hub = temp.hub();
    let mut sub = Sub::new(&hub.feed);
    // The subscribe-time snapshot is the always-on keepers line,
    // empty here (`docs/events.md`, "Hub feed", "keepers"): read it
    // before the broadcast lines, so the refusal wait below still
    // proves nothing else is enqueued.
    let snapshotted = sub.line("the snapshot keepers line");
    assert_eq!(snapshotted["kind"], "keepers");
    assert_eq!(snapshotted["payload"], json!({"keepers": []}));
    let _lock = hold(&dir);
    let refused = refused(&hub, json!({"session": id(1)}));
    assert_eq!(refused.code(), ErrorCode::SessionHeld);
    // A refusal enqueues nothing: the bounded receive must time out.
    // The subscriber proves live below, when the real delete notifies
    // it: a dead listener would time out there too.
    assert!(sub.try_line(Duration::from_secs(2)).is_none());
    drop(_lock);
    assert_eq!(delete(&hub, &args(json!({"session": id(1)}))), Ok(None));
    let line = sub.line("the delete's session_deleted");
    assert_eq!(line["kind"], "session_deleted", "{line}");
    assert_eq!(line["payload"]["session_id"], id(1), "{line}");
    assert_eq!(line["payload"]["project"], "-p", "{line}");
}

#[test]
fn two_clients_see_the_delete_line() {
    let temp = Temp::new();
    temp.session("-p", 1, None);
    let hub = temp.hub();
    let mut first = Sub::new(&hub.feed);
    let mut second = Sub::new(&hub.feed);
    // The subscribe-time snapshot is the always-on keepers line,
    // empty here (`docs/events.md`, "Hub feed", "keepers"): read it
    // before the broadcast lines it precedes.
    for sub in [&mut first, &mut second] {
        let snapshotted = sub.line("the snapshot keepers line");
        assert_eq!(snapshotted["kind"], "keepers");
        assert_eq!(snapshotted["payload"], json!({"keepers": []}));
    }
    assert_eq!(delete(&hub, &args(json!({"session": id(1)}))), Ok(None));
    for sub in [&mut first, &mut second] {
        let line = sub.line("the delete's session_deleted");
        assert_eq!(line["kind"], "session_deleted", "{line}");
        assert_eq!(line["payload"]["session_id"], id(1), "{line}");
        assert_eq!(line["payload"]["project"], "-p", "{line}");
    }
}

#[test]
fn two_clients_see_each_cascade_line_with_its_own_project() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    append_moved(&root, 1, 3);
    temp.session("-q", 3, Some(1));
    let hub = temp.hub();
    let mut first = Sub::new(&hub.feed);
    let mut second = Sub::new(&hub.feed);
    // The subscribe-time snapshot is the always-on keepers line,
    // empty here (`docs/events.md`, "Hub feed", "keepers"): read it
    // before the broadcast lines it precedes.
    for sub in [&mut first, &mut second] {
        let snapshotted = sub.line("the snapshot keepers line");
        assert_eq!(snapshotted["kind"], "keepers");
        assert_eq!(snapshotted["payload"], json!({"keepers": []}));
    }
    assert_eq!(
        delete(
            &hub,
            &args(json!({"session": id(1), "cascade": true, "expect": [id(1), id(3)]}))
        ),
        Ok(None)
    );
    // (`docs/invocation.md`, "What the hub speaks"): with `cascade` the
    // lines arrive in removal order, each dependent before the session
    // it continues, each id with its own project: a shared project here
    // would keep the rows.
    for sub in [&mut first, &mut second] {
        let moved = sub.line("the cascade's line for the child");
        assert_eq!(moved["kind"], "session_deleted", "{moved}");
        assert_eq!(moved["payload"]["session_id"], id(3), "{moved}");
        assert_eq!(moved["payload"]["project"], "-q", "{moved}");
        let parent = sub.line("the cascade's line for the parent");
        assert_eq!(parent["kind"], "session_deleted", "{parent}");
        assert_eq!(parent["payload"]["session_id"], id(1), "{parent}");
        assert_eq!(parent["payload"]["project"], "-p", "{parent}");
    }
}

#[test]
fn a_cascade_delete_notifies_removed_sessions_before_an_io_failure() {
    let temp = Temp::new();
    let root = temp.session("-p", 1, None);
    append_moved(&root, 1, 3);
    let moved = temp.session("-q", 3, Some(1));
    // A directory whose entries cannot be unlinked: the lock still
    // opens unheld, so the removal loop starts, removes the child
    // first, then fails on the parent.
    let perms = fs::metadata(&root).unwrap().permissions();
    fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o500)).unwrap();
    let hub = temp.hub();
    let mut sub = Sub::new(&hub.feed);
    // The subscribe-time snapshot is the always-on keepers line,
    // empty here (`docs/events.md`, "Hub feed", "keepers"): read it
    // before the broadcast lines it precedes.
    let snapshotted = sub.line("the snapshot keepers line");
    assert_eq!(snapshotted["kind"], "keepers");
    assert_eq!(snapshotted["payload"], json!({"keepers": []}));
    let failed = refused(
        &hub,
        json!({"session": id(1), "cascade": true, "expect": [id(1), id(3)]}),
    );
    fs::set_permissions(&root, perms).unwrap();
    assert_eq!(failed.code(), ErrorCode::IoFailed);
    assert!(failed.message().contains(&id(1)), "{}", failed.message());
    // (`docs/invocation.md`, "What the hub speaks"): every removed
    // session is told as it is removed. The child went first with its own
    // project, while the failed parent's directory stays:
    // notify-per-removal, not after the loop.
    let line = sub.line("the cascade's line for the removed child");
    assert_eq!(line["kind"], "session_deleted", "{line}");
    assert_eq!(line["payload"]["session_id"], id(3), "{line}");
    assert_eq!(line["payload"]["project"], "-q", "{line}");
    assert!(!moved.exists(), "the removed child is gone");
    assert!(
        root.join("events.jsonl").is_file(),
        "the failed parent stays"
    );
}
