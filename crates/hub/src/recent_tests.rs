//! Tests for `recent.jsonl`: one line per append, skipped bad lines, the
//! newest row per session, paging, and the hub-start seeds.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::cell::Cell;
use std::collections::BTreeSet;
use std::fs;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;

use super::*;
use crate::fake::status;
use crate::prompt_history::CHUNK;
use crate::testkit::id;
use log::session_dir;

struct Temp {
    dir: PathBuf,
    #[expect(dead_code, reason = "Drop removes the directory")]
    held: fakes::TempDir,
}

impl Temp {
    fn new() -> Self {
        let (held, dir) = crate::testkit::home("hr");
        Self { dir, held }
    }

    /// A row for session `n` in `project`, with its directory made.
    fn row(&self, n: u64, project: &str, how: Left, state: Option<&str>) -> RecentRow {
        let row = row(n, project, how, state);
        fs::create_dir_all(session_dir(&self.dir, project, &row.session_id)).unwrap();
        row
    }

    fn append(&self, row: &RecentRow) {
        append(&self.dir, row).unwrap();
    }
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
        archived: false,
    }
}

fn ids(rows: &[RecentRow]) -> Vec<String> {
    rows.iter().map(|row| row.session_id.0.clone()).collect()
}

fn page_of(home: &Path, before: Option<&str>, project: Option<&str>) -> Vec<String> {
    ids(
        &page_with_total(home, before, project, &BTreeSet::new(), false)
            .unwrap()
            .0,
    )
}

fn archived_page_of(home: &Path, before: Option<&str>, project: Option<&str>) -> Vec<String> {
    ids(
        &page_with_total(home, before, project, &BTreeSet::new(), true)
            .unwrap()
            .0,
    )
}

fn total_of(home: &Path, archived: bool) -> usize {
    page_with_total(home, None, None, &BTreeSet::new(), archived)
        .unwrap()
        .1
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
    let listed = page_with_total(&temp.dir, None, None, &BTreeSet::new(), false)
        .unwrap()
        .0;
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
        ids(&page_with_total(&temp.dir, None, None, &running, false)
            .unwrap()
            .0),
        [id(1)]
    );
    // A running session still anchors `before`.
    assert_eq!(
        ids(
            &page_with_total(&temp.dir, Some(&id(2)), None, &running, false)
                .unwrap()
                .0
        ),
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
fn a_delegate_row_is_skipped_and_pages_stay_full() {
    let temp = Temp::new();
    let total = RECENT_PAGE as u64 + 3;
    // A delegate's row, its status naming a parent, between every two
    // root rows.
    for n in 0..total {
        temp.append(&temp.row(n, "p", Left::Exited, Some("idle")));
        let mut delegate = temp.row(1000 + n, "p", Left::Exited, None);
        delegate.status =
            Some(serde_json::from_value(status("n", "/w", "idle", Some(&id(n)))).unwrap());
        temp.append(&delegate);
    }
    let first = page_of(&temp.dir, None, None);
    assert_eq!(first.len(), RECENT_PAGE);
    assert_eq!(first.first(), Some(&id(total - 1)));
    assert_eq!(first.last(), Some(&id(3)));
    let second = page_of(&temp.dir, Some(&id(3)), None);
    assert_eq!(second, [id(2), id(1), id(0)]);
}

#[test]
fn total_covers_every_row_past_the_page() {
    let temp = Temp::new();
    for n in 0..53 {
        temp.append(&temp.row(n, "p", Left::Exited, None));
    }
    for n in 100..102 {
        temp.append(&temp.row(n, "q", Left::Exited, None));
    }
    let (_, project) =
        page_with_total(&temp.dir, None, Some("p"), &BTreeSet::new(), false).unwrap();
    assert_eq!(project, 53);
    let (_, all) = page_with_total(&temp.dir, None, None, &BTreeSet::new(), false).unwrap();
    assert_eq!(all, 55);
}

#[test]
fn total_skips_running_delegate_and_missing_directory_rows() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Exited, None));
    temp.append(&temp.row(2, "p", Left::Exited, None));
    temp.append(&temp.row(3, "p", Left::Exited, None));
    let mut delegate = temp.row(4, "p", Left::Exited, None);
    delegate.status =
        Some(serde_json::from_value(status("n", "/w", "idle", Some(&id(1)))).unwrap());
    temp.append(&delegate);
    temp.append(&row(5, "p", Left::Exited, None));
    let (_, all) = page_with_total(&temp.dir, None, None, &BTreeSet::new(), false).unwrap();
    assert_eq!(all, 3);
    let running = BTreeSet::from([id(3)]);
    let (_, unfiltered) = page_with_total(&temp.dir, None, None, &running, false).unwrap();
    assert_eq!(unfiltered, 2);
    let (_, project) = page_with_total(&temp.dir, None, Some("p"), &running, false).unwrap();
    assert_eq!(project, 2);
}

#[test]
fn total_counts_every_row_whatever_before_holds() {
    let temp = Temp::new();
    for n in 0..3 {
        temp.append(&temp.row(n, "p", Left::Exited, None));
    }
    let (rows, total) =
        page_with_total(&temp.dir, Some(&id(2)), None, &BTreeSet::new(), false).unwrap();
    assert_eq!(ids(&rows), [id(1), id(0)]);
    assert_eq!(total, 3);
}

#[test]
fn an_unknown_before_is_an_error() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Exited, None));
    assert_eq!(
        page_with_total(&temp.dir, Some(&id(9)), None, &BTreeSet::new(), false)
            .map(|(rows, _)| rows),
        Err(PageError::UnknownBefore)
    );
    // Outside the project filter is not in the list either.
    assert_eq!(
        page_with_total(&temp.dir, Some(&id(1)), Some("q"), &BTreeSet::new(), false)
            .map(|(rows, _)| rows),
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
fn seeds_skip_delegate_rows() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Crashed, Some("idle")));
    // A crashed delegate and a waiting delegate: neither seeds the feed.
    let mut crashed_delegate = temp.row(2, "p", Left::Crashed, None);
    crashed_delegate.status =
        Some(serde_json::from_value(status("n", "/w", "idle", Some(&id(1)))).unwrap());
    temp.append(&crashed_delegate);
    let mut waiting_delegate = temp.row(3, "p", Left::Exited, None);
    waiting_delegate.status =
        Some(serde_json::from_value(status("n", "/w", "waiting", Some(&id(1)))).unwrap());
    temp.append(&waiting_delegate);
    assert_eq!(ids(&seeds(&temp.dir)), [id(1)]);
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

/// A reader over `bytes` that refuses the head: any read starting below
/// `floor` panics, and the smallest read offset is kept in `min`.
struct Counted<'a> {
    inner: Cursor<&'a [u8]>,
    floor: u64,
    min: Rc<Cell<u64>>,
}

impl Read for Counted<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let at = self.inner.stream_position().unwrap_or(u64::MAX);
        if at < self.min.get() {
            self.min.set(at);
        }
        assert!(
            at >= self.floor,
            "a read at {at} reaches into the head ending at {}",
            self.floor
        );
        self.inner.read(buf)
    }
}

impl Seek for Counted<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

#[test]
fn seeds_do_not_read_the_head_of_the_file() {
    const OLD: u64 = 800;
    let temp = Temp::new();
    // A head of 128 KiB of non-UTF-8 bytes and a newline: more than two
    // `CHUNK`, so a forward reader trips the guard below.
    let mut head = vec![0xFF; 128 * 1024];
    head.push(b'\n');
    fs::write(temp.dir.join("recent.jsonl"), &head).unwrap();
    // Older rows, enough that the tail runs past two `CHUNK`.
    for n in 0..OLD {
        temp.append(&row(n, "p", Left::Exited, Some("idle")));
    }
    // Exactly the newest 100 rows, distinct sessions with directories:
    // one crashed, one waiting, the rest exited-idle.
    for j in 0..RECENT_KEEP {
        let n = OLD + j as u64;
        let (how, state) = if j == 0 {
            (Left::Crashed, Some("idle"))
        } else if j == 1 {
            (Left::Exited, Some("waiting"))
        } else {
            (Left::Exited, Some("idle"))
        };
        temp.append(&temp.row(n, "p", how, state));
    }
    let bytes = fs::read(temp.dir.join("recent.jsonl")).unwrap();
    let file_len = bytes.len() as u64;
    let tail = file_len - head.len() as u64;
    assert!(
        tail > 2 * CHUNK as u64,
        "the tail of {tail} fits in two chunks"
    );
    // The newest 100 rows and the delimiter before them fit inside the
    // final `CHUNK`: the reader stops there.
    let mut newlines = bytes
        .iter()
        .enumerate()
        .filter_map(|(at, byte)| (*byte == b'\n').then_some(at as u64));
    let mut delimiter = newlines.next().unwrap();
    for _ in 0..OLD {
        delimiter = newlines.next().unwrap();
    }
    assert!(
        file_len - delimiter <= CHUNK as u64,
        "the newest rows spill past one chunk"
    );
    let seen = Rc::new(Cell::new(u64::MAX));
    let counted = Counted {
        inner: Cursor::new(bytes.as_slice()),
        floor: head.len() as u64,
        min: Rc::clone(&seen),
    };
    let rows = tail_with(counted);
    assert!(
        seen.get() >= head.len() as u64,
        "a read at {} reaches the head",
        seen.get()
    );
    assert_eq!(rows.len(), RECENT_KEEP);
    assert!(ids(&rows).contains(&id(OLD)), "the crashed row is a seed");
    assert!(
        ids(&rows).contains(&id(OLD + 1)),
        "the waiting row is a seed"
    );
}

#[test]
fn seeds_skip_lines_that_do_not_parse_and_count_only_rows() {
    let temp = Temp::new();
    let mut lines = Vec::new();
    for n in 0..100u64 {
        let row = if n == 0 {
            temp.row(n, "p", Left::Crashed, Some("idle"))
        } else {
            temp.row(n, "p", Left::Exited, Some("idle"))
        };
        if n > 0 {
            lines.push("{not json}".to_owned());
        }
        lines.push(serde_json::to_string(&row).unwrap());
    }
    fs::write(temp.dir.join("recent.jsonl"), lines.join("\n") + "\n").unwrap();
    // 100 valid rows past 99 malformed lines: the oldest valid row is
    // still in the window, so the crashed row seeds.
    assert_eq!(ids(&seeds(&temp.dir)), [id(0)]);
}

#[test]
fn a_torn_final_line_is_not_a_seed_and_costs_no_row() {
    let temp = Temp::new();
    assert!(seeds(&temp.dir).is_empty(), "a missing file seeds nothing");
    fs::write(temp.dir.join("recent.jsonl"), b"").unwrap();
    assert!(seeds(&temp.dir).is_empty(), "an empty file seeds nothing");
    for n in 0..RECENT_KEEP as u64 {
        temp.append(&temp.row(n, "p", Left::Crashed, None));
    }
    // A write in progress: bytes with no newline are not a row and do
    // not count toward the window, so the oldest of the 100 still seeds.
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(temp.dir.join("recent.jsonl"))
        .unwrap();
    file.write_all(b"{\"session_id\":\"").unwrap();
    drop(file);
    let seeded = seeds(&temp.dir);
    assert_eq!(seeded.len(), RECENT_KEEP);
    assert!(
        ids(&seeded).contains(&id(0)),
        "the torn line costs no window row"
    );
}

/// An exited row's last `session_status`, as `close` reads it.
fn exited_status(name: &str) -> contract::events::SessionStatus {
    serde_json::from_value(status(name, "/w", "idle", None)).unwrap()
}

#[test]
fn append_exited_writes_nothing_when_the_directory_is_gone() {
    let temp = Temp::new();
    let missing = temp.dir.join("projects/p/sessions").join(id(1));
    append_exited(
        &temp.dir,
        &missing,
        Path::new("/w"),
        Some(exited_status("one")),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    // A deleted session leaves nothing behind: not even the file.
    assert!(!temp.dir.join("recent.jsonl").exists());
}

#[test]
fn append_exited_appends_the_exited_row_for_a_live_directory() {
    let temp = Temp::new();
    let dir = temp.dir.join("projects/myproj/sessions").join(id(1));
    fs::create_dir_all(&dir).unwrap();
    let status = exited_status("three");
    append_exited(
        &temp.dir,
        &dir,
        Path::new("/w"),
        Some(status.clone()),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let rows = read_all(&temp.dir);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.session_id.0, id(1));
    // `projects/<key>/sessions/<id>`: the key names the grandparent.
    assert_eq!(row.project, "myproj");
    assert_eq!(row.ts, 1_700_000_000_000);
    assert_eq!(row.workspace, "/w");
    assert_eq!(row.name, "three");
    assert_eq!(row.how, Left::Exited);
    assert_eq!(row.status, Some(status));
}

#[test]
fn append_exited_reports_an_unwritable_recent_file() {
    let temp = Temp::new();
    let dir = temp.dir.join("projects/myproj/sessions").join(id(1));
    fs::create_dir_all(&dir).unwrap();
    // `home` names a file, so `recent.jsonl` under it cannot be made.
    let blocked = temp.dir.join("blocked");
    fs::write(&blocked, "blocked").unwrap();
    let error = append_exited(
        &blocked,
        &dir,
        Path::new("/w"),
        Some(exited_status("three")),
        &*fakes::clock::FakeClock::new(),
    )
    .unwrap_err();
    assert_eq!(error.code(), contract::ErrorCode::IoFailed);
    assert!(error.to_string().contains("recent.jsonl"));
}

#[test]
fn an_ordinary_row_omits_archived_and_an_old_row_reads_as_not_archived() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Exited, Some("idle")));
    let text = fs::read_to_string(temp.dir.join("recent.jsonl")).unwrap();
    assert!(!text.contains("archived"));
    // A row from a build without the flag reads back as not archived.
    let old: serde_json::Value = serde_json::from_str(text.trim_end()).unwrap();
    assert!(old.get("archived").is_none());
    let rows = read_all(&temp.dir);
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].archived);
    assert_eq!(rows, [temp.row(1, "p", Left::Exited, Some("idle"))]);
}

#[test]
fn an_archived_row_writes_the_flag_and_reads_back() {
    let temp = Temp::new();
    let mut archived = temp.row(1, "p", Left::Exited, Some("idle"));
    archived.archived = true;
    temp.append(&archived);
    let text = fs::read_to_string(temp.dir.join("recent.jsonl")).unwrap();
    assert!(text.contains("\"archived\":true"), "{text}");
    assert_eq!(read_all(&temp.dir), [archived]);
}

#[test]
fn archived_partitions_pages_and_totals_by_newest_row() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Exited, Some("idle")));
    temp.append(&temp.row(2, "p", Left::Exited, Some("idle")));
    // Session 3 exits, then archives: its newest row decides.
    temp.append(&temp.row(3, "p", Left::Exited, Some("idle")));
    let mut archived = temp.row(3, "p", Left::Exited, Some("idle"));
    archived.archived = true;
    temp.append(&archived);
    // Session 4 archives with no earlier ordinary row.
    let mut only = temp.row(4, "p", Left::Crashed, None);
    only.archived = true;
    temp.append(&only);
    assert_eq!(page_of(&temp.dir, None, None), [id(2), id(1)]);
    assert_eq!(total_of(&temp.dir, false), 2);
    assert_eq!(archived_page_of(&temp.dir, None, None), [id(4), id(3)]);
    assert_eq!(total_of(&temp.dir, true), 2);
}

#[test]
fn a_newer_ordinary_row_unarchives_the_listing() {
    let temp = Temp::new();
    let mut archived = temp.row(5, "p", Left::Exited, Some("idle"));
    archived.archived = true;
    temp.append(&archived);
    assert_eq!(archived_page_of(&temp.dir, None, None), [id(5)]);
    assert!(page_of(&temp.dir, None, None).is_empty());
    // Its next exit appends a row without the flag.
    temp.append(&temp.row(5, "p", Left::Exited, Some("idle")));
    assert_eq!(page_of(&temp.dir, None, None), [id(5)]);
    assert!(archived_page_of(&temp.dir, None, None).is_empty());
}

#[test]
fn archived_lists_keep_projection_delegate_and_running_exclusions() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "q", Left::Exited, Some("idle")));
    let mut other = temp.row(1, "q", Left::Exited, Some("idle"));
    other.archived = true;
    temp.append(&other);
    // An archived delegate: skipped by both partitions.
    let mut delegate = temp.row(2, "p", Left::Exited, None);
    delegate.status =
        Some(serde_json::from_value(status("n", "/w", "idle", Some(&id(1)))).unwrap());
    delegate.archived = true;
    temp.append(&delegate);
    let mut archived = temp.row(3, "p", Left::Exited, Some("idle"));
    archived.archived = true;
    temp.append(&archived);
    // Other projects stay out of both partitions.
    assert!(archived_page_of(&temp.dir, None, Some("p")).contains(&id(3)));
    assert!(!archived_page_of(&temp.dir, None, Some("p")).contains(&id(1)));
    // Delegates stay out of the archived partition too.
    assert_eq!(archived_page_of(&temp.dir, None, None), [id(3), id(1)]);
    // A running session's archived row is suppressed in both.
    let running = BTreeSet::from([id(3)]);
    let (rows, total) = page_with_total(&temp.dir, None, None, &running, false).unwrap();
    assert!(rows.is_empty());
    assert_eq!(total, 0);
    let (rows, total) = page_with_total(&temp.dir, None, None, &running, true).unwrap();
    assert_eq!(ids(&rows), [id(1)]);
    assert_eq!(total, 1);
}

#[test]
fn archived_pages_walk_past_fifty_with_matching_totals() {
    let temp = Temp::new();
    for n in 0..55 {
        let mut archived = temp.row(n, "p", Left::Exited, Some("idle"));
        archived.archived = true;
        temp.append(&archived);
    }
    for n in 100..103 {
        temp.append(&temp.row(n, "p", Left::Exited, Some("idle")));
    }
    assert_eq!(total_of(&temp.dir, true), 55);
    assert_eq!(total_of(&temp.dir, false), 3);
    let first = archived_page_of(&temp.dir, None, None);
    assert_eq!(first.len(), RECENT_PAGE);
    assert_eq!(first.first(), Some(&id(54)));
    let second = archived_page_of(&temp.dir, first.last().map(String::as_str), None);
    assert_eq!(second.len(), 5);
    assert_eq!(second.last(), Some(&id(0)));
}

#[test]
fn a_before_outside_the_partition_is_unknown() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Exited, Some("idle")));
    let mut archived = temp.row(2, "p", Left::Exited, Some("idle"));
    archived.archived = true;
    temp.append(&archived);
    // An archived session is no anchor in the default list, and an
    // ordinary one is none in the archived list.
    assert_eq!(
        page_with_total(&temp.dir, Some(&id(2)), None, &BTreeSet::new(), false)
            .map(|(rows, _)| rows),
        Err(PageError::UnknownBefore)
    );
    assert_eq!(
        page_with_total(&temp.dir, Some(&id(1)), None, &BTreeSet::new(), true)
            .map(|(rows, _)| rows),
        Err(PageError::UnknownBefore)
    );
    // Anchors inside the partition still page.
    assert!(
        page_with_total(&temp.dir, Some(&id(2)), None, &BTreeSet::new(), true)
            .unwrap()
            .0
            .is_empty()
    );
}

#[test]
fn seeds_skip_archived_crashed_and_waiting_rows() {
    let temp = Temp::new();
    temp.append(&temp.row(1, "p", Left::Crashed, Some("idle")));
    temp.append(&temp.row(2, "p", Left::Exited, Some("waiting")));
    let mut crashed = temp.row(3, "p", Left::Crashed, Some("idle"));
    crashed.archived = true;
    temp.append(&crashed);
    let mut waiting = temp.row(4, "p", Left::Exited, Some("waiting"));
    waiting.archived = true;
    temp.append(&waiting);
    assert_eq!(ids(&seeds(&temp.dir)), [id(2), id(1)]);
}
