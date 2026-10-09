//! Tests for `fiber resume` and `fiber continue`: the tty refusal, the
//! latest-session pick, and the memoized project test.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use super::{in_project, select_latest, tty_refusal};

/// The refusal sentence, as `fiber` without a tty prints it.
const SENTENCE: &str =
    "The terminal needs a tty; run `fiber ask \"<prompt>\"`. Run `fiber --help` for usage.";

#[test]
fn the_terminal_refuses_unless_both_standard_input_and_output_are_ttys() {
    assert_eq!(tty_refusal(true, true), None);
    assert_eq!(tty_refusal(false, false), Some(SENTENCE));
    assert_eq!(tty_refusal(false, true), Some(SENTENCE));
    assert_eq!(tty_refusal(true, false), Some(SENTENCE));
}

/// Session `id` in `sessions` with a first line at `first` and a last
/// line at `last`.
fn logged(sessions: &Path, id: &str, first: u64, last: u64) {
    let dir = sessions.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let started = serde_json::json!({"kind": "session_started", "session_id": id,
        "ts": first, "schema_version": 1,
        "payload": {"workspace": "/w",
            "variables": {"path": "/usr/bin", "names": [], "source": "inherited"}}});
    let last = serde_json::json!({"kind": "fiber_exited", "session_id": id,
        "ts": last, "schema_version": 1, "payload": {}});
    std::fs::write(dir.join("events.jsonl"), format!("{started}\n{last}\n")).unwrap();
}

#[test]
fn select_latest_returns_the_session_with_the_newest_last_line() {
    let root = fakes::TempDir::new("fiber-open");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    logged(&sessions, "s_old", 200, 200);
    logged(&sessions, "s_new", 100, 300);

    assert_eq!(select_latest(&sessions, &|_| true).unwrap().0, "s_new");
}

#[test]
fn select_latest_without_a_session_is_a_usage_error_naming_fiber() {
    let root = fakes::TempDir::new("fiber-open-empty");
    let sessions = root.path().join("sessions");
    std::fs::create_dir_all(&sessions).unwrap();

    let Err(e) = select_latest(&sessions, &|_| true) else {
        panic!("expected no session");
    };
    assert_eq!(e.code, contract::ErrorCode::Usage);
    assert_eq!(
        e.message,
        "No session in this project to continue; run `fiber` to start one. Run `fiber --help` for usage."
    );
}

#[test]
fn in_project_runs_the_identity_once_per_distinct_workspace() {
    let project = Path::new("/a/b");
    let calls = RefCell::new(Vec::new());
    let identity = |path: &Path| {
        calls.borrow_mut().push(path.to_owned());
        if path == Path::new("/w") {
            PathBuf::from("/a/b")
        } else {
            path.to_owned()
        }
    };
    let accept = in_project(project, &identity);

    assert!(accept("/w"));
    assert!(accept("/w"));
    assert!(!accept("/x"));
    assert!(!accept("/x"));
    assert!(accept("/w"));
    assert_eq!(
        *calls.borrow(),
        vec![PathBuf::from("/w"), PathBuf::from("/x")]
    );
}
