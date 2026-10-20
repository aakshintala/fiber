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

#[test]
fn unknown_id_is_not_found_and_names_the_id() {
    let home = fakes::TempDir::new("main-continues-unknown");
    let missing = home.path().join("sessions").join(OLD);
    let error = resolve(&old(), &missing, home.path()).unwrap_err();
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
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
}

#[test]
fn torn_first_line_is_not_found() {
    let mut first = started(Path::new("/wt"), None);
    first.pop();
    let (_home, old_dir, workspace) = fixture("main-continues-torn", Some(&first));
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
}

#[test]
fn empty_log_is_not_found() {
    let (_home, old_dir, workspace) = fixture("main-continues-empty", Some(b""));
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::SessionNotFound);
}

#[test]
fn unreadable_first_line_is_corrupt() {
    let (_home, old_dir, workspace) = fixture("main-continues-unreadable", None);
    // A directory where the log should be: reading fails however the
    // tests run, even as root, unlike a mode-bit fixture.
    std::fs::create_dir(old_dir.join("events.jsonl")).unwrap();
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn unparseable_first_line_is_corrupt() {
    let (_home, old_dir, workspace) = fixture("main-continues-unparseable", Some(b"{nope}\n"));
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
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
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn missing_workspace_is_corrupt() {
    let line = json!({"kind": "session_started", "payload": {}});
    let mut bytes = serde_json::to_vec(&line).unwrap();
    bytes.push(b'\n');
    let (_home, old_dir, workspace) = fixture("main-continues-workspace", Some(&bytes));
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn no_recorded_worktree_is_invalid() {
    let (_home, old_dir, workspace) = fixture("main-continues-noworktree", None);
    let first = started(&workspace, None);
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
}

#[test]
fn non_object_worktree_is_invalid() {
    let (_home, old_dir, workspace) = fixture("main-continues-worktree-shape", None);
    let first = started(&workspace, Some(json!("/wt")));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
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
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::LogCorrupt);
}

#[test]
fn mismatched_worktree_is_invalid() {
    let elsewhere = Path::new("/elsewhere");
    let (_home, old_dir, workspace) = fixture("main-continues-mismatch", None);
    let first = started(&workspace, Some(worktree(elsewhere)));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let error = resolve(&old(), &old_dir, &workspace).unwrap_err();
    assert_eq!(error.code, contract::ErrorCode::InvalidArguments);
}

#[test]
fn matching_worktree_resolves_with_its_record() {
    let (_home, old_dir, workspace) = fixture("main-continues-match", None);
    let first = started(&workspace, Some(worktree(&workspace)));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let recorded = resolve(&old(), &old_dir, &workspace).unwrap();
    assert_eq!(recorded.path, workspace.to_string_lossy());
    assert_eq!(recorded.branch, "fiber/x");
    assert_eq!(recorded.repository.as_deref(), Some("/repo"));
}

#[test]
fn alias_spellings_of_the_workspace_resolve() {
    let (_home, old_dir, workspace) = fixture("main-continues-alias", None);
    // A trailing slash names the same directory without spelling it.
    let slashed = format!("{}/", workspace.to_string_lossy());
    let first = started(&workspace, Some(worktree(Path::new(&slashed))));
    std::fs::write(old_dir.join("events.jsonl"), first).unwrap();
    let recorded = resolve(&old(), &old_dir, &workspace).unwrap();
    assert_eq!(recorded.path, slashed);
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
    let recorded = resolve(&old(), &old_dir, &workspace).unwrap();
    assert_eq!(recorded.path, workspace.to_string_lossy());
}
