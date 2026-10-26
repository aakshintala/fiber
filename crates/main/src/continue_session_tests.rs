//! Tests for the `start` `continues` validation: which old logs a new
//! session process continues, and which it refuses before writing any
//! session line. The validation is pure over the old log directory and
//! the workspace, so every refusal pins its code with no environment,
//! working directory or provider involved. The dispatch passes the
//! `per_run` layer, model first, as the `resume` arm does.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use contract::SessionId;
use serde_json::{Value, json};

use super::resolve;

const OLD: &str = "s_aaaaaaaaaaaaaaaa";

/// An old session directory under a temporary home, with `first` as its
/// `events.jsonl` content, beside a real workspace directory. The home
/// guard returns with the paths, so the fixture lives for the test.
fn fixture(name: &str, first: Option<&[u8]>) -> (fakes::TempDir, PathBuf, PathBuf) {
    let home = fakes::TempDir::new(name);
    let workspace = home.path().join("wt");
    std::fs::create_dir(&workspace).unwrap();
    let old_dir = home.path().join("sessions").join(OLD);
    std::fs::create_dir_all(&old_dir).unwrap();
    if let Some(first) = first {
        std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    }
    (home, old_dir, workspace)
}

/// A `session_started` first line with `workspace` and `worktree`,
/// trailing newline included.
fn started(workspace: &Path, worktree: Option<Value>) -> Vec<u8> {
    let mut payload = json!({"workspace": workspace.to_string_lossy()});
    if let Some(worktree) = worktree {
        payload["worktree"] = worktree;
    }
    let line = json!({"kind": "session_started", "payload": payload});
    let mut bytes = serde_json::to_vec(&line).unwrap();
    bytes.push(b'\n');
    bytes
}

fn worktree(path: &Path) -> Value {
    json!({
        "path": path.to_string_lossy(),
        "branch": "fiber/x",
        "repository": "/repo",
    })
}

fn old() -> SessionId {
    SessionId(OLD.to_owned())
}

/// A session id that is never the old session, for the self-naming check.
fn new() -> SessionId {
    SessionId("s_bbbbbbbbbbbbbbbb".to_owned())
}

/// The typed session directory for the fixture's old session: the same
/// `sessions/<id>` join production builds, over the fixture's parents.
fn session_dir(old_dir: &Path) -> log::SessionDir {
    let sessions = old_dir.parent().unwrap();
    log::SessionDir::new(sessions, &old())
}

#[test]
fn self_continuation_is_invalid_and_names_the_id() {
    // No old directory at all: the self check fires before any disk is
    // touched, so even a missing log refuses `invalid_arguments`.
    let home = fakes::TempDir::new("main-continues-self");
    let workspace = home.path().join("wt");
    std::fs::create_dir(&workspace).unwrap();
    let missing = home.path().join("sessions").join(OLD);
    let error = resolve(&old(), &old(), &session_dir(&missing), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
    assert!(
        error.message.contains(OLD),
        "the refusal names the session: {}",
        error.message
    );
}

#[test]
fn unknown_id_is_not_found_and_names_the_id() {
    let home = fakes::TempDir::new("main-continues-unknown");
    let missing = home.path().join("sessions").join(OLD);
    let error = resolve(&new(), &old(), &session_dir(&missing), home.path()).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
    assert!(
        error.message.contains(OLD),
        "the refusal names the id: {}",
        error.message
    );
}

#[test]
fn missing_log_is_not_found() {
    let (_home, old_dir, workspace) = fixture("main-continues-missing", None);
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
}

#[test]
fn removed_worktree_after_share_refuses_not_found_with_no_new_log() {
    let (_home, old_dir, workspace) = fixture("main-continues-rechecked-gone", None);
    let first = started(&workspace, Some(worktree(&workspace)));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let recorded = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap();
    // The old session ended and cleaned up between validation and the
    // shared hold: `share` still succeeds (it creates the lock beside
    // the missing path), then the recheck under the hold refuses.
    let shared = doors::share(Path::new(&recorded.path)).unwrap();
    std::fs::remove_dir_all(&workspace).unwrap();
    let error = super::rechecked(&shared, &recorded, &old()).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
    assert!(
        error.message.contains(OLD),
        "the refusal names the old session: {}",
        error.message
    );
    // No new session line was written: the sessions dir holds only the
    // old session, never the refused new one.
    assert!(
        !old_dir
            .parent()
            .unwrap()
            .join("s_bbbbbbbbbbbbbbbb")
            .exists(),
        "no new session log was created"
    );
}

#[test]
fn share_failure_with_worktree_and_parent_gone_is_not_found() {
    // The worktree nested one deeper, so its parent can go with it
    // while the sessions directory stands.
    let home = fakes::TempDir::new("main-continues-share-gone");
    let workspace = home.path().join("par").join("wt");
    std::fs::create_dir_all(&workspace).unwrap();
    let old_dir = home.path().join("sessions").join(OLD);
    std::fs::create_dir_all(&old_dir).unwrap();
    let first = started(&workspace, Some(worktree(&workspace)));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let recorded = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap();
    // The worktree went with its parents: `share` cannot open its
    // lock, so the mapping refuses `session_not_found` naming the
    // old session.
    std::fs::remove_dir_all(home.path().join("par")).unwrap();
    let error = super::shared(&recorded, &old()).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
    assert!(
        error.message.contains(OLD),
        "the refusal names the old session: {}",
        error.message
    );
    // No new session line was written: the sessions dir holds only the
    // old session, never the refused new one. No lock file was created
    // beside the missing path either: `share` never opened it.
    assert!(
        !old_dir
            .parent()
            .unwrap()
            .join("s_bbbbbbbbbbbbbbbb")
            .exists(),
        "no new session log was created"
    );
    assert!(
        !home.path().join("par").join("wt.lock").exists(),
        "no lock file was left behind"
    );
}

#[test]
fn share_failure_with_worktree_standing_propagates_io_failed() {
    let (_home, old_dir, workspace) = fixture("main-continues-share-lock", None);
    let first = started(&workspace, Some(worktree(&workspace)));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let recorded = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap();
    // A directory planted where the sibling lock file goes: `share`
    // cannot open it while the worktree stands, so the `io_failed`
    // failure propagates unchanged.
    let mut lock = workspace.clone();
    lock.set_file_name("wt.lock");
    std::fs::create_dir(&lock).unwrap();
    let error = super::shared(&recorded, &old()).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::IoFailed);
    assert!(
        error.message.contains("wt.lock"),
        "the failure names the lock file: {}",
        error.message
    );
}

#[test]
fn a_longer_old_log_resolves_on_its_first_line() {
    // A live old session keeps appending: only the first line is read.
    // Parsing the whole log would refuse a log like this as corrupt.
    let (_home, old_dir, workspace) = fixture("main-continues-long", None);
    let mut log = started(&workspace, Some(worktree(&workspace)));
    log.extend_from_slice(b"{\"kind\":\"fiber_started\"}\n{\"kind\":\"turn_completed\"}\n");
    std::fs::write(old_dir.join("events.jsonl"), log).unwrap();
    let recorded = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap();
    assert_eq!(recorded.path, workspace.to_string_lossy());
}

#[test]
fn torn_first_line_is_not_found() {
    let mut first = started(Path::new("/wt"), None);
    first.pop();
    let (_home, old_dir, workspace) = fixture("main-continues-torn", Some(&first));
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
}

#[test]
fn empty_log_is_not_found() {
    let (_home, old_dir, workspace) = fixture("main-continues-empty", Some(b""));
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
}

#[test]
fn unreadable_first_line_is_corrupt() {
    let (_home, old_dir, workspace) = fixture("main-continues-unreadable", None);
    // A directory where the log should be: reading fails however the
    // tests run, even as root, unlike a mode-bit fixture.
    std::fs::create_dir(old_dir.join("events.jsonl")).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn unparseable_first_line_is_corrupt() {
    let (_home, old_dir, workspace) = fixture("main-continues-unparseable", Some(b"{nope}\n"));
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn wrong_kind_first_line_is_corrupt() {
    // A workspace the check would accept, so only the kind refuses:
    // without the kind check this answers `invalid_arguments`.
    let (_home, old_dir, workspace) = fixture("main-continues-kind", None);
    let mut line = serde_json::to_vec(&json!({
        "kind": "turn_completed",
        "payload": {"workspace": workspace.to_string_lossy()},
    }))
    .unwrap();
    line.push(b'\n');
    std::fs::write(old_dir.join("events.jsonl"), line).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn missing_workspace_is_corrupt() {
    let line = json!({"kind": "session_started", "payload": {}});
    let mut bytes = serde_json::to_vec(&line).unwrap();
    bytes.push(b'\n');
    let (_home, old_dir, workspace) = fixture("main-continues-workspace", Some(&bytes));
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn no_recorded_worktree_is_invalid() {
    let (_home, old_dir, workspace) = fixture("main-continues-noworktree", None);
    let first = started(&workspace, None);
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
}

#[test]
fn non_object_worktree_is_corrupt() {
    // Absent (missing or null) is no worktree to continue; a present
    // value that is not a record is corrupt.
    let (_home, old_dir, workspace) = fixture("main-continues-worktree-shape", None);
    let first = started(&workspace, Some(json!("/wt")));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn array_worktree_is_corrupt_with_no_new_log() {
    let (_home, old_dir, workspace) = fixture("main-continues-worktree-array", None);
    let first = started(&workspace, Some(json!([])));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
    assert!(
        !old_dir
            .parent()
            .unwrap()
            .join("s_bbbbbbbbbbbbbbbb")
            .exists(),
        "no new session log was created"
    );
}

#[test]
fn null_worktree_is_corrupt_with_no_new_log() {
    // The payload rules forbid `null`: an optional key is absent, never
    // `null`, so a `null` worktree is foreign data and malformed.
    let (_home, old_dir, workspace) = fixture("main-continues-worktree-null", None);
    let first = started(&workspace, Some(Value::Null));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
    assert!(
        !old_dir
            .parent()
            .unwrap()
            .join("s_bbbbbbbbbbbbbbbb")
            .exists(),
        "no new session log was created"
    );
}

#[test]
fn malformed_worktree_object_is_corrupt() {
    // The path matches, so only the object's shape refuses: without the
    // shape check this resolves.
    let (_home, old_dir, workspace) = fixture("main-continues-malformed", None);
    let first = started(
        &workspace,
        Some(json!({"path": workspace.to_string_lossy()})),
    );
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn mismatched_worktree_is_invalid() {
    let elsewhere = Path::new("/elsewhere");
    let (_home, old_dir, workspace) = fixture("main-continues-mismatch", None);
    let first = started(&workspace, Some(worktree(elsewhere)));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
}

#[test]
fn matching_worktree_resolves_with_its_record() {
    let (_home, old_dir, workspace) = fixture("main-continues-match", None);
    let first = started(&workspace, Some(worktree(&workspace)));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let recorded = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap();
    assert_eq!(recorded.path, workspace.to_string_lossy());
    assert_eq!(recorded.branch, "fiber/x");
    assert_eq!(recorded.repository.as_deref(), Some("/repo"));
}

#[test]
fn missing_repository_is_corrupt() {
    // A record from a log that never carries one: `docs/events.md`
    // requires the absolute repository path, so the worktree cannot be
    // continued and reads as corrupt.
    let (_home, old_dir, workspace) = fixture("main-continues-no-repository", None);
    let first = started(
        &workspace,
        Some(json!({"path": workspace.to_string_lossy(), "branch": "fiber/x"})),
    );
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
    assert!(
        !old_dir
            .parent()
            .unwrap()
            .join("s_bbbbbbbbbbbbbbbb")
            .exists(),
        "no new session log was created"
    );
}

#[test]
fn relative_repository_is_corrupt() {
    let (_home, old_dir, workspace) = fixture("main-continues-relative-repository", None);
    let first = started(
        &workspace,
        Some(json!({
            "path": workspace.to_string_lossy(),
            "branch": "fiber/x",
            "repository": "relative/repo",
        })),
    );
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
    assert!(
        !old_dir
            .parent()
            .unwrap()
            .join("s_bbbbbbbbbbbbbbbb")
            .exists(),
        "no new session log was created"
    );
}

#[test]
fn relative_path_is_corrupt() {
    // Every recorded path is absolute, so a relative recorded path is
    // foreign data and malformed: no new session log is created.
    let (_home, old_dir, workspace) = fixture("main-continues-relative-path", None);
    let first = started(
        &workspace,
        Some(json!({
            "path": "relative",
            "branch": "b",
            "repository": "/r",
        })),
    );
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
    assert!(
        !old_dir
            .parent()
            .unwrap()
            .join("s_bbbbbbbbbbbbbbbb")
            .exists(),
        "no new session log was created"
    );
}

/// Re-running this test's entry-point case in a child process: `run`
/// reports through the process's stdout and stderr and every non-usage
/// code exits 1, so only a child run can pin the reported code and the
/// message naming the old session.
const ENTRY_CHILD: &str = "FIBER_MAIN_CONTINUES_ENTRY_CHILD";

/// The child's bound: the entry fails before any session starts, so the
/// child exits at once; the wait stays inside half of nextest's per-test
/// timeout (`.config/nextest.toml`, `docs/testing.md`, "Waits and timeouts").
const ENTRY_WITHIN: Duration = Duration::from_secs(30);

/// This test's full path, for `fakes::rerun_within`: the crate name
/// `module_path!` carries is not part of a test's path.
fn this(test: &str) -> String {
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    format!("{module}::{test}")
}

#[test]
fn entry_point_with_removed_workspace_and_continues_is_not_found() {
    if std::env::var(ENTRY_CHILD).is_ok() {
        entry_child();
        return;
    }
    let home = fakes::TempDir::new("main-continues-entry");
    let workspace = home.path().join("wt");
    std::fs::create_dir(&workspace).unwrap();
    // The worktree went away after hub validation, before the child
    // started: entering it fails with the path absent.
    std::fs::remove_dir(&workspace).unwrap();
    let home_str = home.path().to_string_lossy();
    let workspace_str = workspace.to_string_lossy();
    let out = fakes::rerun_within(
        &this("entry_point_with_removed_workspace_and_continues_is_not_found"),
        &[
            (ENTRY_CHILD, "1"),
            ("FIBER_HOME", home_str.as_ref()),
            ("FIBER_MAIN_CONTINUES_ENTRY_WS", workspace_str.as_ref()),
            ("FIBER_MAIN_CONTINUES_ENTRY_OLD", OLD),
            ("FIBER_MAIN_CONTINUES_ENTRY_NEW", "s_bbbbbbbbbbbbbbbb"),
        ],
        fakes::Deadline::after(ENTRY_WITHIN),
    );
    assert_eq!(out.status.code(), Some(1), "the entry reports a failure");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("session_not_found"), "{stdout}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("No session `{OLD}`")),
        "the refusal names the old session: {stderr}"
    );
    assert!(
        !home.path().join("sessions").exists(),
        "no new session log was created"
    );
}

/// The re-run child: calls the session entry point with a removed
/// workspace and `continues` set, then exits with what it returns.
fn entry_child() {
    let args = crate::cli::SessionArgs {
        id: std::env::var("FIBER_MAIN_CONTINUES_ENTRY_NEW").unwrap(),
        workspace: PathBuf::from(std::env::var("FIBER_MAIN_CONTINUES_ENTRY_WS").unwrap()),
        model: None,
        overrides: Vec::new(),
        prompt: None,
        resume: false,
        credential: None,
        worktree: false,
        rewound_from: None,
        continues: Some(std::env::var("FIBER_MAIN_CONTINUES_ENTRY_OLD").unwrap()),
        forked_from: None,
        forked_seq: None,
        parent: None,
        delegate_id: None,
        depth: None,
        login_shell: false,
        supervised: false,
    };
    let clock: Arc<dyn contract::clock::Clock> = fakes::clock::FakeClock::new();
    let code = crate::session_command::run(args, clock, Err("no fiber in test".to_owned()));
    std::process::exit(code);
}

#[test]
fn alias_spellings_of_the_workspace_refuse() {
    for alias in ["slash", "link"] {
        let (home, old_dir, workspace) = fixture(&format!("main-continues-alias-{alias}"), None);
        // An alias spelling names the same directory without matching
        // the recorded string: a trailing slash, or a symlink at the
        // current directory.
        let current = match alias {
            "slash" => PathBuf::from(format!("{}/", workspace.to_string_lossy())),
            _ => {
                let link = home.path().join("link");
                std::os::unix::fs::symlink(&workspace, &link).unwrap();
                link
            }
        };
        let first = started(&workspace, Some(worktree(&workspace)));
        std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
        let error = resolve(&new(), &old(), &session_dir(&old_dir), &current).unwrap_err();
        assert_eq!(error.code, contract::ErrorCode::InvalidArguments, "{alias}");
    }
}

#[test]
fn a_valid_continues_reaches_run_new_with_model_and_overrides_in_the_layer() {
    let (_home, old_dir, workspace) = fixture("main-continues-layer", None);
    let first = started(&workspace, Some(worktree(&workspace)));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    // The layer the dispatch passes: the model first, then each `-c`,
    // the shape the `resume` arm passes.
    let layer = crate::per_run(Some("fake/m".to_owned()), vec!["key=value".to_owned()]);
    assert_eq!(
        layer,
        vec!["model=fake/m".to_owned(), "key=value".to_owned()]
    );
    // The fixture validates, so the layer reaches `run_new`: an empty
    // layer, or a fixture refusal, never gets there.
    let recorded = resolve(&new(), &old(), &session_dir(&old_dir), &workspace).unwrap();
    assert_eq!(recorded.path, workspace.to_string_lossy());
}
