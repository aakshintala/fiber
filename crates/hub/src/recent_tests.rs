//! Tests for `recent.jsonl`: one line per append, skipped bad lines, the
//! newest row per session, paging, and the hub-start seeds.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::thread;

use super::*;
use crate::fake::status;

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let held = fakes::TempDir::new("hr");
        let dir = held.path().join("h");
        fs::create_dir_all(&dir).unwrap();
        Self { dir, held }
    }

    /// A row for session `n` in `project`, with its directory made.
    fn row(&self, n: u64, project: &str, how: Left, state: Option<&str>) -> RecentRow {
        let row = row(n, project, how, state);
        fs::create_dir_all(session_dir(&self.dir, project, &row.session_id.0)).unwrap();
        row
    }

    fn append(&self, row: &RecentRow) {
        append(&self.dir, row).unwrap();
    }
}

fn id(n: u64) -> String {
    format!("s_{n:016x}")
}

/// A row for session `n` with no directory made.
fn row(n: u64, project: &str, how: Left, state: Option<&str>) -> RecentRow {
    RecentRow {
        session_id: SessionId(id(n)),
        ts: 1_700_000_000_000 + n,
        project: project.to_owned(),
        workspace: "/w".to_owned(),
        name: format!("session {n}"),
        how,
        status: state.map(|state| serde_json::from_value(status("n", "/w", state, None)).unwrap()),
    }
}

fn ids(rows: &[RecentRow]) -> Vec<String> {
    rows.iter().map(|row| row.session_id.0.clone()).collect()
}

fn page_of(home: &Path, before: Option<&str>, project: Option<&str>) -> Vec<String> {
    ids(&page(home, before, project, &BTreeSet::new()).unwrap())
}

#[test]
fn a_row_is_one_line_with_the_documented_keys_and_mode_0600() {
    let temp = Temp::new();
    temp.append(&row(1, "p", Left::Exited, Some("idle")));
    let text = fs::read_to_string(temp.dir.join("recent.jsonl")).unwrap();
    assert_eq!(text.matches('\n').count(), 1);
    let value: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
    let keys: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "how",
            "name",
            "project",
            "session_id",
            "status",
            "ts",
            "workspace"
        ]
    );
    assert_eq!(value["how"], "exited");
    let mode = fs::metadata(temp.dir.join("recent.jsonl"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_row_with_no_status_omits_the_key_and_reads_back() {
    let temp = Temp::new();
    let written = temp.row(1, "p", Left::Crashed, None);
    temp.append(&written);
    let text = fs::read_to_string(temp.dir.join("recent.jsonl")).unwrap();
    assert!(!text.contains("status"));
    assert!(text.contains("\"how\":\"crashed\""));
    assert_eq!(read_all(&temp.dir), [written]);
}

#[test]
fn concurrent_appends_never_interleave() {
    let temp = Temp::new();
    let home = temp.dir.clone();
    let writers: Vec<_> = (0..2u64)
        .map(|w| {
            let home = home.clone();
            thread::spawn(move || {
                for n in 0..200 {
                    append(&home, &row(w * 1000 + n, "p", Left::Exited, Some("idle"))).unwrap();
                }
            })
        })
        .collect();
    for writer in writers {
        writer.join().unwrap();
    }
    let text = fs::read_to_string(home.join("recent.jsonl")).unwrap();
    assert_eq!(text.lines().count(), 400);
    assert_eq!(read_all(&home).len(), 400);
}

#[test]
fn a_malformed_line_is_skipped_and_a_missing_file_is_empty() {
    let temp = Temp::new();
    assert!(read_all(&temp.dir).is_empty());
    temp.append(&row(1, "p", Left::Exited, None));
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(temp.dir.join("recent.jsonl"))
        .unwrap();
    file.write_all(b"{not json\n{\"session_id\":3}\n").unwrap();
    temp.append(&row(2, "p", Left::Exited, None));
    assert_eq!(ids(&read_all(&temp.dir)), [id(1), id(2)]);
}

#[test]
fn the_newest_row_of_each_session_wins_newest_first() {
    let temp = Temp::new();
    let first = temp.row(1, "p", Left::Crashed, None);
    temp.append(&first);
    temp.append(&temp.row(2, "p", Left::Exited, None));
    let mut again = first;
    again.how = Left::Exited;
    temp.append(&again);
    let listed = page(&temp.dir, None, None, &BTreeSet::new()).unwrap();
    assert_eq!(ids(&listed), [id(1), id(2)]);
    assert_eq!(listed.first().unwrap().how, Left::Exited);
}

#[test]
fn project_filters_by_exact_key() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "a", Left::Exited, None));
    temp.append(&temp.row(2, "ab", Left::Exited, None));
    temp.append(&temp.row(3, "a", Left::Exited, None));
    assert_eq!(page_of(&temp.dir, None, Some("a")), [id(3), id(1)]);
    assert_eq!(page_of(&temp.dir, None, Some("ab")), [id(2)]);
    assert!(page_of(&temp.dir, None, Some("b")).is_empty());
}

#[test]
fn a_row_whose_directory_is_gone_is_skipped() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Exited, None));
    temp.append(&row(2, "p", Left::Exited, None));
    assert_eq!(page_of(&temp.dir, None, None), [id(1)]);
}

#[test]
fn a_running_session_is_skipped() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Exited, None));
    temp.append(&temp.row(2, "p", Left::Exited, None));
    let running = BTreeSet::from([id(2)]);
    assert_eq!(
        ids(&page(&temp.dir, None, None, &running).unwrap()),
        [id(1)]
    );
    // A running session still anchors `before`.
    assert_eq!(
        ids(&page(&temp.dir, Some(&id(2)), None, &running).unwrap()),
        [id(1)]
    );
}

#[test]
fn before_pages_across_the_page_boundary_exclusively() {
    let temp = Temp::new();
    let total = RECENT_PAGE as u64 + 3;
    for n in 0..total {
        temp.append(&temp.row(n, "p", Left::Exited, None));
    }
    let first = page_of(&temp.dir, None, None);
    assert_eq!(first.len(), RECENT_PAGE);
    assert_eq!(first.first(), Some(&id(total - 1)));
    let last = first.last().unwrap().clone();
    assert_eq!(last, id(3));
    let second = page_of(&temp.dir, Some(&last), None);
    assert_eq!(second, [id(2), id(1), id(0)]);
    assert!(page_of(&temp.dir, Some(&id(0)), None).is_empty());
}

#[test]
fn an_unknown_before_is_an_error() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Exited, None));
    assert_eq!(
        page(&temp.dir, Some(&id(9)), None, &BTreeSet::new()),
        Err(PageError::UnknownBefore)
    );
    // Outside the project filter is not in the list either.
    assert_eq!(
        page(&temp.dir, Some(&id(1)), Some("q"), &BTreeSet::new()),
        Err(PageError::UnknownBefore)
    );
}

#[test]
fn seeds_are_crashed_and_waiting_rows_whose_directory_is_there() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Crashed, Some("idle")));
    temp.append(&temp.row(2, "p", Left::Exited, Some("waiting")));
    temp.append(&temp.row(3, "p", Left::Exited, Some("idle")));
    temp.append(&temp.row(4, "p", Left::Exited, None));
    temp.append(&row(5, "p", Left::Crashed, Some("idle")));
    // Crashed while waiting is still crashed.
    temp.append(&temp.row(6, "p", Left::Crashed, Some("waiting")));
    assert_eq!(ids(&seeds(&temp.dir)), [id(6), id(2), id(1)]);
}

#[test]
fn a_seed_is_the_sessions_newest_row() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Crashed, Some("idle")));
    temp.append(&temp.row(1, "p", Left::Exited, Some("idle")));
    assert!(seeds(&temp.dir).is_empty());
}

#[test]
fn seeds_come_from_the_newest_hundred_rows_only() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Crashed, Some("idle")));
    for n in 2..=RECENT_KEEP as u64 {
        temp.append(&temp.row(n, "p", Left::Exited, Some("idle")));
    }
    // Exactly 100 rows: the oldest is kept.
    assert_eq!(ids(&seeds(&temp.dir)), [id(1)]);
    temp.append(&temp.row(500, "p", Left::Exited, Some("idle")));
    // 101 rows: the oldest is not.
    assert!(seeds(&temp.dir).is_empty());
}

#[test]
fn find_names_the_project_holding_the_session() {
    let temp = Temp::new();
    temp.row(1, "other", Left::Exited, None);
    let (project, dir) = find(&temp.dir, &id(1)).unwrap();
    assert_eq!(project, "other");
    assert_eq!(dir, session_dir(&temp.dir, "other", &id(1)));
    assert_eq!(find(&temp.dir, &id(2)), None);
}
