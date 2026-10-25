//! Tests for prune execution: confirmation, the hub round trip,
//! reconciliation and formatting, through `prune_run` with a fake hub.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::{
    HubPruneWant, OldFile, PruneArgs, delete_sessions, format_age, format_size, prune, prune_run,
    remove_diagnostics, remove_for_hub, select_for_hub,
};
use crate::CliError;
use crate::sessions::Ask;
use contract::clock::Clock;
use contract::shapes::Failure;
use contract::{ErrorCode, HubLine, SessionId};
use fakes::Deadline;
use serde_json::{Map, Value, json};

/// One named deadline for the fake hub's read and the test's receive.
const HUB_DEADLINE: Duration = Duration::from_secs(10);

fn wall() -> std::time::SystemTime {
    fakes::clock::FakeClock::new().wall()
}

fn clock() -> std::sync::Arc<fakes::clock::FakeClock> {
    fakes::clock::FakeClock::new()
}

fn args(older_than: Option<&str>, cascade: bool, dry_run: bool, yes: bool) -> PruneArgs {
    PruneArgs {
        older_than: older_than.map(str::to_owned),
        cascade,
        dry_run,
        yes,
        force: false,
    }
}

/// Fiber home and a workspace outside any repository, removed on drop.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let setup = Self {
            root: fakes::TempDir::new("cli-prune"),
        };
        fs::create_dir_all(setup.home()).unwrap();
        fs::create_dir_all(setup.root.path().join("workspace")).unwrap();
        setup
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn workspace(&self) -> PathBuf {
        fs::canonicalize(self.root.path().join("workspace")).unwrap()
    }

    fn sessions(&self) -> PathBuf {
        log::sessions_dir(&self.home(), &doors::project(&self.workspace()))
    }

    fn session(&self, id: &str, from: Option<&str>, ts: u64) -> PathBuf {
        let dir = self.sessions().join(id);
        fs::create_dir_all(dir.join("artifacts")).unwrap();
        let mut payload = json!({"workspace": "/w"});
        if let Some(from) = from {
            payload["forked_from"] = json!({"session_id": from, "seq": 1});
        }
        let first = json!({"kind": "session_started", "seq": 0, "payload": payload});
        let last = json!({"kind": "x", "ts": ts});
        fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
        fs::write(dir.join("artifacts/a.txt"), b"0123456789").unwrap();
        dir
    }

    fn diag(&self, kind: &str, name: &str, bytes: &[u8], age: Duration) {
        let dir = self.home().join(kind);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(name), bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(dir.join(name))
            .unwrap()
            .set_modified(wall() - age)
            .unwrap();
    }
}

/// Accepts every command; rejects the sessions in `reject` with its code
/// and message. Runs each command line by its session id.
struct FakeHub {
    connects: usize,
    received: Arc<Mutex<Vec<Value>>>,
    handler: Option<Box<dyn FnMut(Value) -> Value + Send>>,
}

impl FakeHub {
    fn new(handler: impl FnMut(Value) -> Value + Send + 'static) -> Self {
        Self {
            connects: 0,
            received: Arc::new(Mutex::new(Vec::new())),
            handler: Some(Box::new(handler)),
        }
    }

    fn accept_all() -> Self {
        Self::new(|line| {
            let id = line["id"].as_str().unwrap_or("c_prune_1").to_owned();
            json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
                "payload": {"command_id": id}})
        })
    }

    fn connect(&mut self) -> Result<doors::hub::Hub, doors::Error> {
        self.connects += 1;
        let (client, hub) = UnixStream::pair().map_err(doors::Error::Local)?;
        hub.set_read_timeout(Some(HUB_DEADLINE))
            .map_err(doors::Error::Local)?;
        let received = Arc::clone(&self.received);
        let mut handler = self.handler.take().expect("prune connects once");
        thread::spawn(move || {
            let mut read = BufReader::new(&hub);
            loop {
                let mut line = String::new();
                let n = read.read_line(&mut line).unwrap_or(0);
                if n == 0 {
                    break;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                received.lock().unwrap().push(request.clone());
                let answer = handler(request);
                (&hub)
                    .write_all(format!("{answer}\n").as_bytes())
                    .unwrap_or(());
            }
        });
        let hello = HubLine {
            kind: "hub_hello".to_owned(),
            ts: 1,
            schema_version: 1,
            payload: Map::new(),
        };
        Ok((client, hello))
    }
}

fn accept_line(id: &str) -> Value {
    json!({"kind": "command_accepted", "ts": 1, "schema_version": 1,
        "payload": {"command_id": id}})
}

fn reject_line(id: &str, code: &str, message: &str) -> Value {
    json!({"kind": "command_rejected", "ts": 1, "schema_version": 1,
        "payload": {"command_id": id, "code": code, "message": message}})
}

/// Input that fails the test if anything reads it.
struct Unread;

impl Read for Unread {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        panic!("nothing is read");
    }
}

impl BufRead for Unread {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        panic!("nothing is read");
    }

    fn consume(&mut self, _: usize) {}
}

/// Runs `prune_run` against `hub`: its stdout, stderr and result.
fn run_prune(
    setup: &Setup,
    hub: &mut FakeHub,
    args: &PruneArgs,
    yes: bool,
    terminal: bool,
    input: &mut dyn BufRead,
) -> (String, String, Result<(), CliError>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ask = Ask {
        yes,
        terminal,
        input,
        err: &mut err,
    };
    let got = prune_run(
        &setup.home(),
        &setup.workspace(),
        args,
        clock().as_ref(),
        ask,
        &mut out,
        &mut || hub.connect(),
    );
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        got,
    )
}

/// Runs `prune_run` with a hub connection that always fails.
fn run_prune_no_connect(
    setup: &Setup,
    args: &PruneArgs,
    yes: bool,
    terminal: bool,
    input: &mut dyn BufRead,
) -> (String, String, Result<(), CliError>) {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let ask = Ask {
        yes,
        terminal,
        input,
        err: &mut err,
    };
    let got = prune_run(
        &setup.home(),
        &setup.workspace(),
        args,
        clock().as_ref(),
        ask,
        &mut out,
        &mut || -> Result<doors::hub::Hub, doors::Error> {
            Err(doors::Error::Local(io::Error::other("down")))
        },
    );
    (
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
        got,
    )
}

/// Whole days old of a session whose last line has `ts`, floored.
fn age_days(ts: u64) -> u64 {
    (contract::clock::wall_ms(wall()) - ts) / (24 * 60 * 60 * 1000)
}

#[test]
fn dry_run_prints_every_row_and_the_exact_total() {
    let setup = Setup::new();
    let dir = setup.session("s_00000000000000a1", None, 0);
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    setup.diag(
        "crashes",
        "old.crash",
        b"12345",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let mut hub = FakeHub::accept_all();
    let (out, err, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, true, false),
        false,
        false,
        &mut Unread,
    );
    got.unwrap();
    assert_eq!(err, "");
    let session_bytes = log::session_bytes(&dir);
    assert!(session_bytes > 0);
    let total = session_bytes + 10 + 5;
    assert_eq!(
        out,
        format!(
            "session  s_00000000000000a1  {}d  {}\nlog  old.log  31d  10 B\ncrash  old.crash  31d  5 B\nwould free {}\n",
            age_days(0),
            format_size(session_bytes),
            format_size(total),
        )
    );
    assert_eq!(hub.connects, 0);
}

#[test]
fn dry_run_names_blockers_and_cycles_in_singular_and_plural() {
    // One young fork blocks with `continues it`.
    let single = Setup::new();
    single.session("s_00000000000000a1", None, 0);
    single.session(
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
        contract::clock::wall_ms(wall()),
    );
    // Two young forks block with `continue it`.
    let plural = Setup::new();
    plural.session("s_00000000000000a1", None, 0);
    plural.session(
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
        contract::clock::wall_ms(wall()),
    );
    plural.session(
        "s_00000000000000c3",
        Some("s_00000000000000a1"),
        contract::clock::wall_ms(wall()),
    );
    // A session continuing itself cycles with `continues itself`.
    let alone = Setup::new();
    alone.session("s_00000000000000a1", Some("s_00000000000000a1"), 0);
    // Two sessions continuing each other cycle with `continue each other`.
    let pair = Setup::new();
    pair.session("s_00000000000000a1", Some("s_00000000000000b2"), 0);
    pair.session("s_00000000000000b2", Some("s_00000000000000a1"), 0);
    let age = age_days(0);
    for (setup, expected) in [
        (
            &single,
            format!(
                "session  s_00000000000000a1  {age}d  skipped: s_00000000000000b2 continues it\nfreed 0 B\n"
            ),
        ),
        (
            &plural,
            format!(
                "session  s_00000000000000a1  {age}d  skipped: s_00000000000000b2, s_00000000000000c3 continue it\nfreed 0 B\n"
            ),
        ),
        (
            &alone,
            format!(
                "session  s_00000000000000a1  {age}d  skipped: s_00000000000000a1 continues itself\nfreed 0 B\n"
            ),
        ),
        (
            &pair,
            format!(
                "session  s_00000000000000a1  {age}d  skipped: s_00000000000000a1, s_00000000000000b2 continue each other\nsession  s_00000000000000b2  {age}d  skipped: s_00000000000000a1, s_00000000000000b2 continue each other\nfreed 0 B\n"
            ),
        ),
    ] {
        let mut hub = FakeHub::accept_all();
        let (out, _, got) = run_prune(
            setup,
            &mut hub,
            &args(Some("30d"), false, true, false),
            false,
            false,
            &mut Unread,
        );
        got.unwrap();
        assert_eq!(out, expected);
        assert_eq!(hub.connects, 0);
    }
}

#[test]
fn finish_reports_the_exact_number_of_deletions_on_failure() {
    let rows = vec![super::sessions::SessionRow::Deletable {
        id: "s_00000000000000a1".to_owned(),
        age_days: 31,
        bytes: 4,
        dir: PathBuf::from("session"),
        delete: 0,
    }];
    let mut out = Vec::new();
    let mut err = Vec::new();
    let failure = super::finish(
        &mut out,
        &mut err,
        &rows,
        17,
        vec![(
            "old.log".to_owned(),
            ErrorCode::IoFailed,
            "could not delete".to_owned(),
        )],
        2,
        2,
    )
    .unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::PruneFailed {
                code: ErrorCode::IoFailed,
                failed: 1,
                total: 5,
            }
        ),
        "{failure:?}"
    );
    assert_eq!(String::from_utf8(out).unwrap(), "freed 17 B\n");
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "fiber: old.log could not be deleted: could not delete\n"
    );
}

#[test]
fn remove_diagnostics_counts_every_file_and_keeps_every_failure() {
    let setup = Setup::new();
    let good = setup.home().join("good.log");
    fs::create_dir_all(setup.home()).unwrap();
    fs::write(&good, b"0123456789").unwrap();
    let more = setup.home().join("more.log");
    fs::write(&more, b"12345").unwrap();
    let missing = setup.home().join("gone.log");
    let blocked = setup.home().join("blocked");
    fs::create_dir_all(&blocked).unwrap();
    let removed = remove_diagnostics(&[
        OldFile {
            path: good.clone(),
            bytes: 10,
        },
        OldFile {
            path: missing.clone(),
            bytes: 0,
        },
        OldFile {
            path: blocked.clone(),
            bytes: 7,
        },
        OldFile {
            path: more.clone(),
            bytes: 5,
        },
    ]);
    assert_eq!(removed.freed, 15);
    assert_eq!(removed.failures.len(), 1);
    assert!(
        removed.failures[0].0.contains("blocked"),
        "{:?}",
        removed.failures
    );
    assert!(!good.exists());
    assert!(!more.exists());
    assert!(blocked.exists());
}

#[test]
fn a_diagnostics_failure_still_deletes_everything_else() {
    use std::os::unix::fs::PermissionsExt;
    let setup = Setup::new();
    let dir = setup.session("s_00000000000000a1", None, 0);
    let session_bytes = log::session_bytes(&dir);
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    setup.diag(
        "crashes",
        "old.crash",
        b"12345",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    // Taking write permission off `logs/` makes its removal fail, while
    // `crashes/` and the session still delete.
    let logs = setup.home().join("logs");
    fs::set_permissions(&logs, fs::Permissions::from_mode(0o555)).unwrap();
    let sessions = setup.sessions();
    let mut hub = FakeHub::new(move |line| {
        let id = line["id"].as_str().unwrap().to_owned();
        let session = line["args"]["session"].as_str().unwrap().to_owned();
        fs::remove_dir_all(sessions.join(&session)).unwrap_or(());
        accept_line(&id)
    });
    let (out, err, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, false, true),
        true,
        false,
        &mut Unread,
    );
    fs::set_permissions(&logs, fs::Permissions::from_mode(0o755)).unwrap();
    let failure = got.unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::PruneFailed {
                code: ErrorCode::IoFailed,
                failed: 1,
                total: 3,
            }
        ),
        "{failure:?}"
    );
    let freed = session_bytes + 5;
    assert_eq!(
        out,
        format!(
            "session  s_00000000000000a1  {}d  {}\nlog  old.log  31d  10 B\ncrash  old.crash  31d  5 B\nfreed {}\n",
            age_days(0),
            format_size(session_bytes),
            format_size(freed),
        )
    );
    assert!(err.contains("old.log"), "{err}");
    assert!(err.contains("could not be deleted"), "{err}");
    assert!(setup.home().join("logs/old.log").exists());
    assert!(!setup.home().join("crashes/old.crash").exists());
    assert!(!dir.exists());
}

#[test]
fn a_failed_hub_connection_fails_every_remaining_session() {
    let setup = Setup::new();
    let dir = setup.session("s_00000000000000a1", None, 0);
    let session_bytes = log::session_bytes(&dir);
    let (out, err, got) = run_prune_no_connect(
        &setup,
        &args(Some("30d"), false, false, true),
        true,
        false,
        &mut Unread,
    );
    let failure = got.unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::PruneFailed {
                code: ErrorCode::IoFailed,
                failed: 1,
                total: 1,
            }
        ),
        "{failure:?}"
    );
    assert_eq!(
        out,
        format!(
            "session  s_00000000000000a1  {}d  {}\nfreed 0 B\n",
            age_days(0),
            format_size(session_bytes),
        )
    );
    assert!(
        err.contains("session s_00000000000000a1 could not be deleted: the hub: down"),
        "{err}"
    );
    assert!(dir.exists());
}

#[test]
fn cascade_lists_an_unreadable_dependent_as_deleted_with_unknown_age() {
    let setup = Setup::new();
    let root = setup.session("s_00000000000000a1", None, 0);
    let child = setup.sessions().join("s_00000000000000b2");
    fs::create_dir_all(child.join("artifacts")).unwrap();
    let first = json!({
        "kind": "session_started",
        "seq": 0,
        "payload": {
            "workspace": "/w",
            "forked_from": {"session_id": "s_00000000000000a1", "seq": 1},
        },
    });
    fs::write(child.join("events.jsonl"), format!("{first}\nnot json\n")).unwrap();
    fs::write(child.join("artifacts/a.txt"), b"0123456789").unwrap();
    let mut hub = FakeHub::accept_all();
    let (out, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), true, true, false),
        false,
        false,
        &mut Unread,
    );
    got.unwrap();
    let root_bytes = log::session_bytes(&root);
    let child_bytes = log::session_bytes(&child);
    let total = root_bytes + child_bytes;
    assert_eq!(
        out,
        format!(
            "session  s_00000000000000a1  {}d  {}\nsession  s_00000000000000b2  unknown  {}  continues s_00000000000000a1\nwould free {}\n",
            age_days(0),
            format_size(root_bytes),
            format_size(child_bytes),
            format_size(total),
        )
    );
    assert_eq!(hub.connects, 0);
}

#[test]
fn dry_run_reads_nothing_and_never_connects() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", None, 0);
    let mut hub = FakeHub::accept_all();
    let (out, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, true, false),
        false,
        false,
        &mut Unread,
    );
    got.unwrap();
    assert!(out.contains("would free"), "{out}");
    assert!(setup.sessions().join("s_00000000000000a1").exists());
    assert_eq!(hub.connects, 0);
}

#[test]
fn neither_yes_nor_a_terminal_is_a_usage_error_with_no_scan_and_no_connect() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", None, 0);
    let mut hub = FakeHub::accept_all();
    let (_, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, false, false),
        false,
        false,
        &mut Unread,
    );
    let failure = got.unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::NobodyToAsk { ref message } if message == super::PRUNE_NOBODY
        ),
        "{failure:?}"
    );
    assert_eq!(hub.connects, 0);
}

#[test]
fn dry_run_with_no_terminal_is_fine() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", None, 0);
    let mut hub = FakeHub::accept_all();
    let (out, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, true, false),
        false,
        false,
        &mut Unread,
    );
    got.unwrap();
    assert!(out.contains("would free"), "{out}");
    assert_eq!(hub.connects, 0);
}

#[test]
fn a_declined_answer_deletes_nothing() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", None, 0);
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let mut hub = FakeHub::accept_all();
    let mut input = io::Cursor::new(b"n\n".to_vec());
    let (_, err, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, false, false),
        false,
        true,
        &mut input,
    );
    got.unwrap();
    assert!(err.contains("nothing deleted"), "{err}");
    assert!(setup.sessions().join("s_00000000000000a1").exists());
    assert!(setup.home().join("logs/old.log").exists());
    assert_eq!(hub.connects, 0);
}

#[test]
fn a_run_with_only_diagnostics_never_connects() {
    let setup = Setup::new();
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let mut hub = FakeHub::accept_all();
    let (out, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(None, false, false, true),
        true,
        false,
        &mut Unread,
    );
    got.unwrap();
    assert!(out.contains("freed"), "{out}");
    assert!(!setup.home().join("logs/old.log").exists());
    assert_eq!(hub.connects, 0);
}

#[test]
fn nothing_to_delete_prints_freed_0_b_and_asks_nothing() {
    let setup = Setup::new();
    let mut hub = FakeHub::accept_all();
    let (out, err, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, false, true),
        true,
        false,
        &mut Unread,
    );
    got.unwrap();
    assert!(out.contains("freed 0 B"), "{out}");
    assert_eq!(err, "");
    assert_eq!(hub.connects, 0);
}

#[test]
fn one_rejected_session_leaves_the_others_deleted_and_counts_only_removed_bytes() {
    let setup = Setup::new();
    let dir_a = setup.session("s_00000000000000a1", None, 0);
    let dir_b = setup.session("s_00000000000000b2", None, 0);
    let bytes_a = log::session_bytes(&dir_a);
    let bytes_b = log::session_bytes(&dir_b);
    assert!(bytes_a > 0 && bytes_b > 0);
    // The accepted delete removes its directory, as the hub would.
    let sessions = setup.sessions();
    let mut hub = FakeHub::new(move |line| {
        let id = line["id"].as_str().unwrap().to_owned();
        let session = line["args"]["session"].as_str().unwrap().to_owned();
        if session == "s_00000000000000a1" {
            reject_line(&id, "session_held", "Session is held.")
        } else {
            fs::remove_dir_all(sessions.join(&session)).unwrap_or(());
            accept_line(&id)
        }
    });
    let (out, err, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, false, true),
        true,
        false,
        &mut Unread,
    );
    let failure = got.unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::PruneFailed {
                code: ErrorCode::SessionHeld,
                failed: 1,
                total: 2,
            }
        ),
        "{failure:?}"
    );
    assert!(!dir_b.exists(), "the accepted session is gone");
    assert!(dir_a.exists(), "the rejected session stays");
    assert!(err.contains("s_00000000000000a1"), "{err}");
    assert!(out.contains(&format_size(bytes_b)), "{out}");
    assert!(!out.contains(&format_size(bytes_a + bytes_b)), "{out}");
}

#[test]
fn reconciliation_counts_a_removed_descendant_and_fails_only_the_root() {
    let setup = Setup::new();
    let dir_root = setup.session("s_00000000000000a1", None, 0);
    let dir_child = setup.session("s_00000000000000b2", Some("s_00000000000000a1"), 0);
    let bytes_child = log::session_bytes(&dir_child);
    let sessions = setup.sessions();
    let mut hub = FakeHub::new(move |line| {
        let id = line["id"].as_str().unwrap().to_owned();
        // The hub removes the descendant, then rejects the root's cascade.
        fs::remove_dir_all(sessions.join("s_00000000000000b2")).unwrap_or(());
        reject_line(&id, "session_held", "Session is held.")
    });
    let (out, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), true, false, true),
        true,
        false,
        &mut Unread,
    );
    let failure = got.unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::PruneFailed {
                code: ErrorCode::SessionHeld,
                failed: 1,
                total: 2,
            }
        ),
        "{failure:?}"
    );
    assert!(!dir_child.exists());
    assert!(dir_root.exists());
    assert!(out.contains(&format_size(bytes_child)), "{out}");
}

#[test]
fn partial_removal_is_a_failure_and_frees_listed_minus_remaining() {
    let setup = Setup::new();
    let dir = setup.session("s_00000000000000a1", None, 0);
    let listed = log::session_bytes(&dir);
    assert!(listed > 0);
    let sessions = setup.sessions();
    let mut hub = FakeHub::new(move |line| {
        let id = line["id"].as_str().unwrap().to_owned();
        let dir = sessions.join("s_00000000000000a1");
        fs::remove_file(dir.join("events.jsonl")).unwrap_or(());
        reject_line(&id, "io_failed", "Could not remove it.")
    });
    let (out, err, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, false, true),
        true,
        false,
        &mut Unread,
    );
    let failure = got.unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::PruneFailed {
                code: ErrorCode::IoFailed,
                ..
            }
        ),
        "{failure:?}"
    );
    let left = log::remaining(&dir).unwrap().unwrap();
    assert!(left < listed);
    assert!(out.contains(&format_size(listed - left)), "{out}");
    assert!(err.contains("Could not remove it."), "{err}");
}

#[test]
fn a_not_found_on_a_diagnostics_file_is_neither_a_failure_nor_counted() {
    let setup = Setup::new();
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    fs::remove_file(setup.home().join("logs/old.log")).unwrap();
    let mut hub = FakeHub::accept_all();
    let (out, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(None, false, false, true),
        true,
        false,
        &mut Unread,
    );
    got.unwrap();
    assert!(out.contains("freed 0 B"), "{out}");
    assert_eq!(hub.connects, 0);
}

#[test]
fn an_unreadable_older_than_is_usage_before_any_scan() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", None, 0);
    let mut hub = FakeHub::accept_all();
    let (out, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("soon"), false, false, false),
        false,
        false,
        &mut Unread,
    );
    let failure = got.unwrap_err();
    assert!(
        matches!(failure, CliError::OlderThanUnreadable),
        "{failure:?}"
    );
    assert_eq!(out, "");
    assert_eq!(hub.connects, 0);
}

#[test]
fn finish_takes_the_first_failures_code() {
    let rows = vec![super::sessions::SessionRow::Deletable {
        id: "s_00000000000000a1".to_owned(),
        age_days: 31,
        bytes: 4,
        dir: PathBuf::from("session"),
        delete: 0,
    }];
    let mut out = Vec::new();
    let mut err = Vec::new();
    let failure = super::finish(
        &mut out,
        &mut err,
        &rows,
        17,
        vec![
            ("a".to_owned(), ErrorCode::SessionHeld, "x".to_owned()),
            ("b".to_owned(), ErrorCode::IoFailed, "y".to_owned()),
        ],
        1,
        1,
    )
    .unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::PruneFailed {
                code: ErrorCode::SessionHeld,
                failed: 2,
                total: 3,
            }
        ),
        "{failure:?}"
    );
    assert_eq!(String::from_utf8(out).unwrap(), "freed 17 B\n");
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "fiber: a could not be deleted: x\nfiber: b could not be deleted: y\n"
    );
}

struct Closed;

impl Write for Closed {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("closed"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A writer that prints the first line, then fails: the listing prints
/// and the final `freed` line fails.
struct FailSecondWrite {
    done: bool,
}

impl Write for FailSecondWrite {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.done {
            return Err(io::Error::other("closed"));
        }
        if bytes.contains(&b'\n') {
            self.done = true;
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn check_forwarded_closed(got: Result<(), CliError>) {
    let failure = got.unwrap_err();
    assert!(
        matches!(
            failure,
            CliError::Forwarded(ref forwarded)
                if forwarded.code == ErrorCode::IoFailed
                    && forwarded.message == "standard output: closed"
        ),
        "{failure:?}"
    );
}

#[test]
fn a_closed_standard_output_is_io_failed_with_the_shared_sentence() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", None, 0);
    let mut hub = FakeHub::accept_all();
    let mut err = Vec::new();
    let mut input = Unread;
    let ask = Ask {
        yes: false,
        terminal: false,
        input: &mut input,
        err: &mut err,
    };
    let got = prune_run(
        &setup.home(),
        &setup.workspace(),
        &args(Some("30d"), false, true, false),
        clock().as_ref(),
        ask,
        &mut Closed,
        &mut || hub.connect(),
    );
    check_forwarded_closed(got);
    assert_eq!(hub.connects, 0);
}

#[test]
fn a_closed_standard_output_on_an_empty_run_is_io_failed() {
    let setup = Setup::new();
    let mut hub = FakeHub::accept_all();
    let mut err = Vec::new();
    let mut input = Unread;
    let ask = Ask {
        yes: false,
        terminal: false,
        input: &mut input,
        err: &mut err,
    };
    let got = prune_run(
        &setup.home(),
        &setup.workspace(),
        &args(Some("30d"), false, true, false),
        clock().as_ref(),
        ask,
        &mut Closed,
        &mut || hub.connect(),
    );
    check_forwarded_closed(got);
    assert_eq!(hub.connects, 0);
}

#[test]
fn a_failed_write_of_the_freed_line_after_deleting_still_deletes() {
    let setup = Setup::new();
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let mut hub = FakeHub::accept_all();
    let mut err = Vec::new();
    let mut input = Unread;
    let ask = Ask {
        yes: true,
        terminal: false,
        input: &mut input,
        err: &mut err,
    };
    let got = prune_run(
        &setup.home(),
        &setup.workspace(),
        &args(None, false, false, true),
        clock().as_ref(),
        ask,
        &mut FailSecondWrite { done: false },
        &mut || hub.connect(),
    );
    check_forwarded_closed(got);
    assert!(!setup.home().join("logs/old.log").exists());
    assert_eq!(hub.connects, 0);
}

#[test]
fn the_size_formatter_counts_below_1024_in_bytes_then_one_decimal() {
    assert_eq!(format_size(0), "0 B");
    assert_eq!(format_size(1023), "1023 B");
    assert_eq!(format_size(1024), "1.0 KiB");
    assert_eq!(format_size(1536), "1.5 KiB");
    assert_eq!(format_size(1_048_575), "1024.0 KiB");
    assert_eq!(format_size(1_048_576), "1.0 MiB");
    assert_eq!(format_size(1_073_741_824), "1.0 GiB");
}

#[test]
fn the_age_formatter_counts_whole_days_floored() {
    assert_eq!(format_age(0), "0d");
    assert_eq!(format_age(1), "1d");
    assert_eq!(format_age(41), "41d");
}

#[test]
fn prune_exits_zero_on_success_two_on_usage_and_one_on_refusal() {
    if let Ok(case) = std::env::var("FIBER_CLI_PRUNE_CHILD") {
        let clock = fakes::clock::FakeClock::new();
        let mut hub = FakeHub::new(|line| {
            let id = line["id"].as_str().unwrap().to_owned();
            if std::env::var("FIBER_CLI_PRUNE_CHILD").unwrap() == "refusal" {
                reject_line(&id, "session_held", "Session is held.")
            } else {
                accept_line(&id)
            }
        });
        let code = match case.as_str() {
            "usage" => prune(
                &args(Some("30d"), false, false, false),
                clock.as_ref(),
                &mut || hub.connect(),
            ),
            _ => prune(
                &args(Some("30d"), false, false, true),
                clock.as_ref(),
                &mut || hub.connect(),
            ),
        };
        if case == "usage" {
            assert_eq!(hub.connects, 0, "usage must not reach the hub");
        }
        std::process::exit(code);
    }
    let setup = Setup::new();
    setup.session("s_00000000000000a1", None, 0);
    // The failing delete leaves its directory, so refusal exits 1.
    let name = module_path!().split_once("::").unwrap().1;
    for (case, code) in [("success", 0), ("usage", 2), ("refusal", 1)] {
        // The fake hub in the child accepts without removing: success
        // still exits 0 with `freed` counting listed minus remaining.
        let output = fakes::rerun_prepared(
            &format!("{name}::prune_exits_zero_on_success_two_on_usage_and_one_on_refusal"),
            &[("FIBER_CLI_PRUNE_CHILD", case)],
            Deadline::after(HUB_DEADLINE),
            |command| {
                command
                    .env("FIBER_HOME", setup.home())
                    .current_dir(setup.workspace());
            },
        );
        assert_eq!(
            output.status.code(),
            Some(code),
            "{case}: {}",
            output.status
        );
    }
}

/// A session dir under `projects/<project>/sessions/<id>`: the hub-wide
/// selection finds it whatever the workspace is.
fn project_session(home: &std::path::Path, project: &str, id: &str, from: Option<&str>) -> PathBuf {
    let dir = home
        .join("projects")
        .join(project)
        .join("sessions")
        .join(id);
    fs::create_dir_all(dir.join("artifacts")).unwrap();
    let mut payload = json!({"workspace": "/w"});
    if let Some(from) = from {
        payload["forked_from"] = json!({"session_id": from, "seq": 1});
    }
    let first = json!({"kind": "session_started", "seq": 0, "payload": payload});
    let last = json!({"kind": "x", "ts": 0});
    fs::write(dir.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
    fs::write(dir.join("artifacts/a.txt"), b"0123456789").unwrap();
    dir
}

#[test]
fn delete_sessions_passes_each_previewed_expect_set_to_the_callback() {
    use std::collections::{BTreeMap, BTreeSet};
    let deletes = vec![
        super::sessions::Delete {
            id: SessionId("s_1".into()),
            cascade: false,
        },
        super::sessions::Delete {
            id: SessionId("s_2".into()),
            cascade: true,
        },
    ];
    let mut expects: BTreeMap<SessionId, BTreeSet<SessionId>> = BTreeMap::new();
    expects.insert(SessionId("s_1".into()), BTreeSet::new());
    expects.insert(
        SessionId("s_2".into()),
        BTreeSet::from([SessionId("s_2".into()), SessionId("s_3".into())]),
    );
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned()));
        if id.0 == "s_2" {
            Err(Failure {
                code: ErrorCode::SessionHeld,
                message: "held".to_owned(),
                provider: None,
                retry_after_ms: None,
            })
        } else {
            Ok(())
        }
    };
    let results = delete_sessions(&deletes, &expects, &mut delete);
    // An empty set stays unconstrained; a full one travels as `expect`.
    // Compared as a set: no area doc promises the callback order.
    calls.sort();
    assert_eq!(
        calls,
        [
            ("s_1".to_owned(), false, None),
            (
                "s_2".to_owned(),
                true,
                Some(BTreeSet::from([
                    SessionId("s_2".into()),
                    SessionId("s_3".into())
                ]))
            ),
        ]
    );
    // One outcome per delete, compared as a set for the same reason.
    let mut outcomes: Vec<String> = results
        .iter()
        .map(|result| match result {
            Ok(()) => "ok".to_owned(),
            Err(failure) => format!("err:{:?}", failure.code),
        })
        .collect();
    outcomes.sort();
    assert_eq!(outcomes, ["err:SessionHeld".to_owned(), "ok".to_owned()]);
}

#[test]
fn select_for_hub_lists_every_project_with_matching_lines_and_bytes() {
    let setup = Setup::new();
    let root = project_session(&setup.home(), "alpha", "s_00000000000000a1", None);
    let fork = project_session(
        &setup.home(),
        "beta",
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
    );
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let selection = select_for_hub(
        &setup.home(),
        Some(Duration::from_secs(30 * 24 * 60 * 60)),
        true,
        false,
        wall(),
        clock().as_ref(),
    );
    assert_eq!(
        selection.sessions.len(),
        1,
        "one cascade root across both projects"
    );
    let session = &selection.sessions[0];
    assert_eq!(session.id.0, "s_00000000000000a1");
    assert!(session.cascade);
    assert_eq!(
        session.expect,
        [
            SessionId("s_00000000000000a1".into()),
            SessionId("s_00000000000000b2".into())
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    );
    let root_bytes = log::session_bytes(&root);
    let fork_bytes = log::session_bytes(&fork);
    let listed = root_bytes + fork_bytes;
    assert_eq!(session.bytes, listed);
    assert_eq!(
        selection.worktrees,
        Vec::<String>::new(),
        "no git worktrees in the fixture"
    );
    assert!(selection.pinned.is_empty());
    assert_eq!(selection.diagnostics.len(), 1);
    assert!(selection.diagnostics[0].ends_with("old.log"));
    assert_eq!(selection.would_free, listed + 10);
    // The preview prints exactly what the local printers print.
    assert_eq!(
        selection.lines,
        [
            format!(
                "session  s_00000000000000a1  {}  {}",
                format_age(age_days(0)),
                format_size(root_bytes)
            ),
            format!(
                "session  s_00000000000000b2  {}  {}  continues s_00000000000000a1",
                format_age(age_days(0)),
                format_size(fork_bytes)
            ),
            format!("log  old.log  31d  {}", format_size(10)),
        ]
    );
}

#[test]
fn remove_for_hub_rejects_a_newly_selectable_item_before_any_removal() {
    use std::collections::{BTreeMap, BTreeSet};
    let setup = Setup::new();
    let root = project_session(&setup.home(), "alpha", "s_00000000000000a1", None);
    let fork = project_session(
        &setup.home(),
        "beta",
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
    );
    // Selectable at confirm time but never previewed: the preview was
    // taken before this session qualified.
    let fresh = project_session(&setup.home(), "alpha", "s_00000000000000c3", None);
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let old_log = setup
        .home()
        .join("logs")
        .join("old.log")
        .display()
        .to_string();
    let mut sessions: BTreeMap<SessionId, BTreeSet<SessionId>> = BTreeMap::new();
    sessions.insert(
        SessionId("s_00000000000000a1".into()),
        BTreeSet::from([
            SessionId("s_00000000000000a1".into()),
            SessionId("s_00000000000000b2".into()),
        ]),
    );
    let want = HubPruneWant {
        sessions,
        worktrees: BTreeSet::new(),
        pinned: BTreeSet::new(),
        diagnostics: BTreeSet::from([old_log]),
    };
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned().unwrap_or_default()));
        Ok(())
    };
    // A newly selectable item rejects the whole request: no callback,
    // no removal anywhere.
    let refused = remove_for_hub(
        &setup.home(),
        Some(&want),
        Some(Duration::from_secs(30 * 24 * 60 * 60)),
        true,
        false,
        clock().as_ref(),
        &mut delete,
    )
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    assert!(
        calls.is_empty(),
        "no session reaches the callback on mismatch"
    );
    for dir in [&root, &fork, &fresh] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
    assert!(
        setup.home().join("logs").join("old.log").is_file(),
        "no diagnostic is removed on mismatch"
    );
}

#[test]
fn remove_for_hub_rejects_a_worktree_only_preview_mismatch() {
    use std::collections::{BTreeMap, BTreeSet};
    let setup = Setup::new();
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let old_log = setup
        .home()
        .join("logs")
        .join("old.log")
        .display()
        .to_string();
    // Exactly one set mismatches: diagnostics, pinned and sessions all
    // equal the reselection, only the worktree set differs. An `&&` in
    // place of either `||` would accept this preview, so this case kills
    // both operator flips while the multi-mismatch cases cannot.
    let want = HubPruneWant {
        sessions: BTreeMap::new(),
        worktrees: BTreeSet::from(["/no/such/worktree".to_owned()]),
        pinned: BTreeSet::new(),
        diagnostics: BTreeSet::from([old_log]),
    };
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned().unwrap_or_default()));
        Ok(())
    };
    let refused = remove_for_hub(
        &setup.home(),
        Some(&want),
        Some(Duration::from_secs(30 * 24 * 60 * 60)),
        false,
        false,
        clock().as_ref(),
        &mut delete,
    )
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    assert!(
        calls.is_empty(),
        "no session reaches the callback on mismatch"
    );
    assert!(
        setup.home().join("logs").join("old.log").is_file(),
        "no diagnostic is removed on mismatch"
    );
}

#[test]
fn local_prune_numbers_each_delete_command_differently() {
    let setup = Setup::new();
    setup.session("s_00000000000000a1", None, 0);
    setup.session("s_00000000000000b2", None, 0);
    let mut hub = FakeHub::accept_all();
    let (_, _, got) = run_prune(
        &setup,
        &mut hub,
        &args(Some("30d"), false, false, true),
        true,
        false,
        &mut Unread,
    );
    got.unwrap();
    // The hub answers by command id, so two deletes sharing one id would
    // still match their answers in order: the ids must differ.
    let received = hub.received.lock().unwrap();
    let ids: Vec<String> = received
        .iter()
        .filter_map(|line| line.get("id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    assert_eq!(ids.len(), 2, "both deletes went out");
    assert_ne!(ids[0], ids[1], "each delete carries its own command id");
}

/// A kept git worktree under `projects/<project>/worktrees/<name>`, on
/// its own branch, with listed bytes.
fn git_worktree(setup: &Setup, project: &str, name: &str) -> PathBuf {
    let repo = setup.root.path().join(format!("repo-{name}"));
    fs::create_dir_all(&repo).unwrap();
    fakes::git::git(&repo, &["init", "--quiet"]);
    fs::write(repo.join("file.txt"), "x").unwrap();
    fakes::git::git(&repo, &["add", "."]);
    fakes::git::git(&repo, &["commit", "--quiet", "-m", "first"]);
    let dir = setup
        .home()
        .join("projects")
        .join(project)
        .join("worktrees")
        .join(name);
    fs::create_dir_all(dir.parent().unwrap()).unwrap();
    fakes::git::git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            &format!("fiber/{name}"),
            dir.to_str().unwrap(),
        ],
    );
    dir
}

/// A 64-hex-digit content hash no tree holds.
const UNMATCHED_HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// A pinned copy of `hash` owned by `key`'s project record, with listed
/// bytes, and a session giving the owner a recorded identity in
/// `identity`, so the hub-wide selection judges it removable.
fn unmatched_copy(setup: &Setup, key: &str, hash: &str, identity: &std::path::Path) {
    let servers = setup.home().join("projects").join(key).join("servers");
    fs::create_dir_all(&servers).unwrap();
    fs::write(servers.join(hash), b"").unwrap();
    let dir = setup.home().join("pinned").join(hash);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("file.txt"), b"0123456789").unwrap();
    let session = setup
        .home()
        .join("projects")
        .join(key)
        .join("sessions")
        .join("s_00000000000000d4");
    fs::create_dir_all(&session).unwrap();
    let first = json!({"kind": "session_started", "seq": 0,
        "session_id": "s_00000000000000d4", "ts": 0, "schema_version": 1,
        "payload": {"workspace": identity.to_str().unwrap(),
            "variables": {"path": "", "names": [], "source": "login_shell"}}});
    let last = json!({"kind": "x", "session_id": "s_00000000000000d4",
        "ts": 0, "schema_version": 1, "payload": {}});
    fs::write(session.join("events.jsonl"), format!("{first}\n{last}\n")).unwrap();
}

/// An identity directory with a project key of its own, holding a file
/// no pinned hash matches.
fn identity_with_key(setup: &Setup) -> (PathBuf, String) {
    let raw = setup.root.path().join("identity");
    fs::create_dir_all(&raw).unwrap();
    fs::write(raw.join("tree.txt"), b"tree").unwrap();
    let identity = fs::canonicalize(&raw).unwrap();
    let key = log::project_key(&identity);
    (identity, key)
}

#[test]
fn select_for_hub_sums_worktree_pinned_and_crash_bytes() {
    let setup = Setup::new();
    let worktree = git_worktree(&setup, "wt", "w1");
    let worktree_bytes = log::session_bytes(&worktree);
    assert!(worktree_bytes > 0);
    let (identity, key) = identity_with_key(&setup);
    unmatched_copy(&setup, &key, UNMATCHED_HASH, &identity);
    let pinned_bytes = log::session_bytes(&setup.home().join("pinned").join(UNMATCHED_HASH));
    assert!(pinned_bytes > 0);
    setup.diag(
        "crashes",
        "old.crash",
        b"1234567",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let selection = select_for_hub(&setup.home(), None, false, false, wall(), clock().as_ref());
    assert!(selection.sessions.is_empty());
    assert_eq!(selection.worktrees, [worktree.display().to_string()]);
    assert_eq!(selection.pinned, [UNMATCHED_HASH.to_owned()]);
    assert!(
        selection
            .diagnostics
            .iter()
            .any(|path| path.ends_with("old.crash"))
    );
    // Every side contributes its bytes: a dropped or multiplied addend
    // shows here.
    assert_eq!(selection.would_free, worktree_bytes + pinned_bytes + 7);
    assert!(
        selection.lines.iter().any(|line| line.contains("w1")),
        "{:?}",
        selection.lines
    );
    assert!(
        selection
            .lines
            .iter()
            .any(|line| line.contains(UNMATCHED_HASH))
    );
    assert!(
        selection
            .lines
            .iter()
            .any(|line| line.contains("old.crash"))
    );
}

#[test]
fn remove_for_hub_frees_previewed_worktree_and_pinned_bytes() {
    use std::collections::{BTreeMap, BTreeSet};
    let setup = Setup::new();
    let worktree = git_worktree(&setup, "wt", "w1");
    let worktree_bytes = log::session_bytes(&worktree);
    assert!(worktree_bytes > 0);
    let (identity, key) = identity_with_key(&setup);
    unmatched_copy(&setup, &key, UNMATCHED_HASH, &identity);
    let pinned_bytes = log::session_bytes(&setup.home().join("pinned").join(UNMATCHED_HASH));
    assert!(pinned_bytes > 0);
    let want = HubPruneWant {
        sessions: BTreeMap::new(),
        worktrees: BTreeSet::from([worktree.display().to_string()]),
        pinned: BTreeSet::from([UNMATCHED_HASH.to_owned()]),
        diagnostics: BTreeSet::new(),
    };
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned()));
        Ok(())
    };
    let removed = remove_for_hub(
        &setup.home(),
        Some(&want),
        None,
        false,
        false,
        clock().as_ref(),
        &mut delete,
    )
    .unwrap();
    assert!(calls.is_empty());
    assert!(removed.failures.is_empty());
    assert_eq!(removed.total, 2);
    // Both removals free their listed bytes: a subtracted addend shows
    // here.
    assert_eq!(removed.freed, worktree_bytes + pinned_bytes);
    assert!(!worktree.exists(), "the previewed worktree is gone");
    assert!(
        !setup.home().join("pinned").join(UNMATCHED_HASH).exists(),
        "the previewed copy is gone"
    );
}

#[test]
fn remove_for_hub_rejects_a_session_that_left_the_selection() {
    use std::collections::{BTreeMap, BTreeSet};
    let setup = Setup::new();
    let dir = project_session(&setup.home(), "alpha", "s_00000000000000a1", None);
    let mut sessions: BTreeMap<SessionId, BTreeSet<SessionId>> = BTreeMap::new();
    sessions.insert(SessionId("s_00000000000000a1".into()), BTreeSet::new());
    let want = HubPruneWant {
        sessions,
        worktrees: BTreeSet::new(),
        pinned: BTreeSet::new(),
        diagnostics: BTreeSet::new(),
    };
    // The session runs again while confirmation is open: its last line
    // is now, younger than `--older-than`.
    let clock = clock();
    let now_ms = contract::clock::wall_ms(clock.wall());
    let fresh = json!({"kind": "x", "ts": now_ms});
    let mut log = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    writeln!(log, "{fresh}").unwrap();
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned()));
        Ok(())
    };
    // A previewed session that no longer qualifies rejects the whole
    // request: no callback, nothing removed.
    let refused = remove_for_hub(
        &setup.home(),
        Some(&want),
        Some(Duration::from_secs(30 * 24 * 60 * 60)),
        false,
        false,
        clock.as_ref(),
        &mut delete,
    )
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    assert!(
        calls.is_empty(),
        "no session reaches the callback on mismatch"
    );
    assert!(dir.join("events.jsonl").is_file(), "nothing was removed");
}

#[test]
fn remove_for_hub_rejects_a_changed_cascade_set_before_any_removal() {
    use std::collections::{BTreeMap, BTreeSet};
    let setup = Setup::new();
    let root = project_session(&setup.home(), "alpha", "s_00000000000000a1", None);
    // A fork made while confirmation is open: the cascade set the
    // preview named no longer matches.
    let fork = project_session(
        &setup.home(),
        "beta",
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
    );
    let mut sessions: BTreeMap<SessionId, BTreeSet<SessionId>> = BTreeMap::new();
    sessions.insert(
        SessionId("s_00000000000000a1".into()),
        BTreeSet::from([SessionId("s_00000000000000a1".into())]),
    );
    let want = HubPruneWant {
        sessions,
        worktrees: BTreeSet::new(),
        pinned: BTreeSet::new(),
        diagnostics: BTreeSet::new(),
    };
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned().unwrap_or_default()));
        Ok(())
    };
    let refused = remove_for_hub(
        &setup.home(),
        Some(&want),
        Some(Duration::from_secs(30 * 24 * 60 * 60)),
        true,
        false,
        clock().as_ref(),
        &mut delete,
    )
    .unwrap_err();
    assert_eq!(refused.code, ErrorCode::StaleRequest);
    assert!(
        calls.is_empty(),
        "no session reaches the callback on mismatch"
    );
    for dir in [&root, &fork] {
        assert!(dir.join("events.jsonl").is_file(), "{}", dir.display());
    }
}

#[test]
fn remove_for_hub_without_expect_removes_the_current_selection() {
    use std::collections::BTreeSet;
    let setup = Setup::new();
    let root = project_session(&setup.home(), "alpha", "s_00000000000000a1", None);
    let fork = project_session(
        &setup.home(),
        "beta",
        "s_00000000000000b2",
        Some("s_00000000000000a1"),
    );
    setup.diag(
        "logs",
        "old.log",
        b"0123456789",
        Duration::from_secs(31 * 24 * 60 * 60),
    );
    let listed = log::session_bytes(&root) + log::session_bytes(&fork);
    let mut calls = Vec::new();
    let mut delete = |id: &SessionId, cascade: bool, expect: Option<&BTreeSet<SessionId>>| {
        calls.push((id.0.clone(), cascade, expect.cloned()));
        Ok(())
    };
    // Without `expect` (`--yes`) the confirm removes the current
    // selection, as the local path does.
    let removed = remove_for_hub(
        &setup.home(),
        None,
        Some(Duration::from_secs(30 * 24 * 60 * 60)),
        true,
        false,
        clock().as_ref(),
        &mut delete,
    )
    .unwrap();
    // The cascade set travels as `expect`, as with a preview.
    assert_eq!(
        calls,
        [(
            "s_00000000000000a1".to_owned(),
            true,
            Some(
                [
                    SessionId("s_00000000000000a1".into()),
                    SessionId("s_00000000000000b2".into())
                ]
                .into_iter()
                .collect()
            ),
        )]
    );
    assert_eq!(removed.total, 2);
    assert!(removed.failures.is_empty());
    assert_eq!(removed.freed, listed + 10);
}
