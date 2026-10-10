//! Tests for the scan: which sessions it reads, ranking and the limit,
//! cancellation, links and problems.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test helpers; a failure is the test's"
)]

use std::cmp::Ordering;
use std::fs::{self};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

use contract::clock::Wake;
use contract::events::Event;
use contract::session_search::Label;
use contract::Seq;
use fakes::clock::FakeClock;
use fakes::{CancelToken, TempDir};
use proptest::prelude::*;
use serde_json::json;

use super::*;
use crate::Log;
use crate::fixtures::event;
pub(super) use crate::fixtures::append_raw as raw;

/// A Fiber home whose sessions are written with [`Log`].
pub(super) struct Home {
    dir: TempDir,
    clock: Arc<FakeClock>,
    /// How many times the identity function ran.
    calls: Arc<AtomicUsize>,
}

impl Home {
    pub(super) fn new() -> Self {
        Self {
            dir: TempDir::new("log-search-scan"),
            clock: FakeClock::new(),
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub(super) fn path(&self) -> &Path {
        self.dir.path()
    }

    pub(super) fn sessions(&self, key: &str) -> PathBuf {
        self.path().join("projects").join(key).join("sessions")
    }

    /// A session `id` under `key` started in `workspace`, holding
    /// `text_completed` lines with `texts`.
    pub(super) fn session(&self, key: &str, id: &str, workspace: &str, texts: &[&str]) -> PathBuf {
        let log = Log::create(
            &self.sessions(key),
            SessionId(id.into()),
            self.clock.clone(),
        )
        .unwrap();
        log.append(&started(workspace), None, None).unwrap();
        for text in texts {
            self.clock.advance(Duration::from_secs(1));
            log.append(&event("text_completed", json!({"text": text})), None, None)
                .unwrap();
        }
        self.sessions(key).join(id)
    }

    /// The scanner for a session in `workspace`. Workspaces under `/b` are
    /// project `/b`, under `/c` project `/c`; every other is project `/a`.
    pub(super) fn scanner(&self, workspace: &str) -> SessionScan {
        let calls = Arc::clone(&self.calls);
        let identity: Identity = Arc::new(move |path: &Path| {
            calls.fetch_add(1, AtomicOrdering::SeqCst);
            for project in ["/b", "/c"] {
                if path.starts_with(project) {
                    return PathBuf::from(project);
                }
            }
            PathBuf::from("/a")
        });
        SessionScan::new(self.path(), Path::new(workspace), identity)
    }

    pub(super) fn calls(&self) -> usize {
        self.calls.load(AtomicOrdering::SeqCst)
    }
}

fn started(workspace: &str) -> Event {
    event(
        "session_started",
        json!({
            "workspace": workspace,
            "variables": {"path": "/bin", "names": [], "source": "inherited"},
        }),
    )
}

pub(super) fn query(text: &str, all_projects: bool, limit: usize) -> Query {
    Query {
        text: text.into(),
        all_projects,
        limit,
    }
}

fn find(scan: &SessionScan, text: &str, all_projects: bool) -> Found {
    scan.scan(&query(text, all_projects, 100), &CancelToken::new())
}

fn sessions_of(found: &Found) -> Vec<&str> {
    let mut ids: Vec<&str> = found.hits.iter().map(|h| h.session_id.0.as_str()).collect();
    ids.dedup();
    ids
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

pub(super) fn hit(label: Label, ts: u64, session: &str, seq: u64) -> Hit {
    Hit {
        session_id: SessionId(session.into()),
        name: String::new(),
        seq: Seq(seq),
        ts,
        label,
        snippet: String::new(),
        log: PathBuf::new(),
        artifact: None,
    }
}

fn keys(hits: &[Hit]) -> Vec<(Label, u64, String, u64)> {
    hits.iter()
        .map(|h| (h.label, h.ts, h.session_id.0.clone(), h.seq.0))
        .collect()
}

/// A [`Cancel`] that turns true after `after` checks.
pub(super) struct After {
    pub(super) checks: AtomicUsize,
    after: usize,
}

impl After {
    pub(super) fn new(after: usize) -> Self {
        Self {
            checks: AtomicUsize::new(0),
            after,
        }
    }
}

impl Cancel for After {
    fn is_cancelled(&self) -> bool {
        self.checks.fetch_add(1, AtomicOrdering::SeqCst) >= self.after
    }

    fn subscribe(&self, _: Weak<dyn Wake>) {}
}

#[test]
fn only_the_own_projects_sessions_are_searched_unless_all_projects() {
    let home = Home::new();
    home.session("-a", "s_own", "/a/main", &["needle own"]);
    home.session("-a", "s_tree", "/a2/worktree", &["needle worktree"]);
    // Another project whose key collides with this one's.
    home.session("-a", "s_collide", "/c/x", &["needle collide"]);
    home.session("-b", "s_far", "/b/far", &["needle far"]);
    let scan = home.scanner("/a/main");
    let found = find(&scan, "needle", false);
    assert_eq!(sessions_of(&found), ["s_tree", "s_own"]);
    let found = find(&scan, "needle", true);
    assert_eq!(
        sessions_of(&found),
        ["s_far", "s_collide", "s_tree", "s_own"]
    );
}

#[test]
fn identity_runs_once_per_workspace_per_call_and_once_for_the_own() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/one", &["needle"]);
    home.session("-a", "s_2", "/a/one", &["needle"]);
    home.session("-a", "s_3", "/a/two", &["needle"]);
    home.session("-a", "s_4", "/c/x", &["needle"]);
    let scan = home.scanner("/a/main");
    assert_eq!(home.calls(), 0, "nothing runs at construction");
    find(&scan, "needle", false);
    // The own workspace, then /a/one, /a/two and /c/x.
    assert_eq!(home.calls(), 4);
    find(&scan, "needle", false);
    assert_eq!(home.calls(), 7);
    scan.scope(false);
    assert_eq!(home.calls(), 7);
}

#[test]
fn all_projects_never_runs_identity() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/one", &["needle"]);
    let scan = home.scanner("/a/main");
    let found = find(&scan, "needle", true);
    assert_eq!(found.total, 1);
    assert_eq!(home.calls(), 0);
    assert_eq!(
        scan.scope(true),
        PathBuf::from(format!("{}/projects/", home.path().display()))
    );
    assert_eq!(home.calls(), 0);
}

#[test]
fn scope_names_the_own_project_or_every_project() {
    let home = Home::new();
    let scan = home.scanner("/a/main");
    let projects = home.path().join("projects");
    assert_eq!(
        scan.scope(false),
        PathBuf::from(format!("{}/-a/", projects.display()))
    );
    assert_eq!(
        scan.scope(true),
        PathBuf::from(format!("{}/", projects.display()))
    );
}

#[test]
fn hits_rank_by_class_then_newer_then_session_then_seq() {
    let mut out = Collect::new(100);
    for hit in [
        hit(Label::ToolOutput, 9, "s_a", 1),
        hit(Label::Message, 1, "s_a", 1),
        hit(Label::ToolInput, 5, "s_b", 3),
        hit(Label::Message, 5, "s_a", 2),
        hit(Label::Message, 5, "s_a", 4),
        hit(Label::ToolInput, 5, "s_a", 4),
        hit(Label::ToolOutput, 2, "s_a", 7),
    ] {
        out.hit(hit);
    }
    let found = out.found();
    let want: Vec<(Label, u64, String, u64)> = vec![
        (Label::Message, 5, "s_a".into(), 4),
        (Label::ToolInput, 5, "s_a".into(), 4),
        (Label::Message, 5, "s_a".into(), 2),
        (Label::ToolInput, 5, "s_b".into(), 3),
        (Label::Message, 1, "s_a".into(), 1),
        (Label::ToolOutput, 9, "s_a".into(), 1),
        (Label::ToolOutput, 2, "s_a".into(), 7),
    ];
    assert_eq!(keys(&found.hits), want);
}

#[test]
fn hits_tied_on_rank_order_by_log_then_snippet_then_artifact() {
    let with = |log: &str, snippet: &str, artifact: Option<&str>| {
        let mut hit = hit(Label::ToolOutput, 5, "s_a", 4);
        hit.log = PathBuf::from(log);
        snippet.clone_into(&mut hit.snippet);
        hit.artifact = artifact.map(PathBuf::from);
        hit
    };
    let base = with("/b", "b", Some("/b"));
    for (smaller, larger) in [
        (with("/a", "c", Some("/c")), base),
        (with("/b", "a", Some("/c")), with("/b", "b", Some("/a"))),
        (with("/b", "b", None), with("/b", "b", Some("/a"))),
        (with("/b", "b", Some("/a")), with("/b", "b", Some("/b"))),
    ] {
        assert_eq!(key(&smaller).cmp(&key(&larger)), Ordering::Less);
        assert_ne!(key(&smaller), key(&larger));
    }
}

#[test]
fn the_limit_keeps_the_best_and_counts_every_hit() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/one", &["needle 1", "needle 2", "needle 3"]);
    let scan = home.scanner("/a/one");
    let found = scan.scan(&query("needle", false, 2), &CancelToken::new());
    let seqs: Vec<u64> = found.hits.iter().map(|h| h.seq.0).collect();
    assert_eq!(seqs, [3, 2]);
    assert_eq!(found.total, 3);
    let found = scan.scan(&query("needle", false, 0), &CancelToken::new());
    assert!(found.hits.is_empty());
    assert_eq!(found.total, 3);
}

#[test]
fn a_limit_of_zero_keeps_no_hit_while_scanning() {
    let mut out = Collect::new(0);
    for seq in 0..1000 {
        out.hit(hit(Label::Message, seq, "s", seq));
        assert!(out.hits.is_empty());
    }
    assert_eq!(out.found().total, 1000);
}

#[test]
fn names_reach_the_kept_hits_of_their_session_only() {
    let mut out = Collect::new(10);
    out.hit(hit(Label::Message, 1, "s_a", 1));
    out.hit(hit(Label::Message, 2, "s_b", 1));
    out.name(&SessionId("s_a".into()), "alpha");
    let found = out.found();
    let names: Vec<&str> = found.hits.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["", "alpha"]);
}

proptest! {
    /// The kept hits equal sorting every hit by rank and taking the first
    /// `limit`.
    #[test]
    fn the_kept_hits_equal_sort_then_truncate(
        rows in prop::collection::vec((0..3_usize, 0..4_u64, 0..3_usize, 0..5_u64), 0..40),
        limit in 0..12_usize,
    ) {
        let labels = [Label::Message, Label::ToolInput, Label::ToolOutput];
        let sessions = ["s_a", "s_b", "s_c"];
        let all: Vec<Hit> = rows
            .iter()
            .map(|(label, ts, session, seq)| hit(labels[*label], *ts, sessions[*session], *seq))
            .collect();
        let mut out = Collect::new(limit);
        for hit in all.clone() {
            out.hit(hit);
        }
        let mut sorted = all;
        sorted.sort_by(|a, b| key(a).cmp(&key(b)));
        let want: Vec<Hit> = sorted.into_iter().take(limit).collect();
        let found = out.found();
        prop_assert_eq!(keys(&found.hits), keys(&want));
        prop_assert_eq!(found.total, rows.len() as u64);
    }
}

#[test]
fn an_empty_text_finds_nothing() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/one", &["anything"]);
    let scan = home.scanner("/a/one");
    assert_eq!(find(&scan, "", true), Found::default());
}

#[test]
fn a_missing_projects_directory_finds_nothing() {
    let home = Home::new();
    let scan = home.scanner("/a/one");
    assert_eq!(find(&scan, "needle", true), Found::default());
    assert_eq!(find(&scan, "needle", false), Found::default());
}

#[test]
fn an_unreadable_project_parent_is_a_problem_but_a_missing_project_is_silent() {
    assert_ne!(
        effective_uid(),
        0,
        "this test must run as a non-root user: root bypasses file modes"
    );
    let home = Home::new();
    home.session("-a", "s_1", "/a/one", &["needle"]);
    let project = home.path().join("projects").join("-a");
    let sessions = project.join("sessions");
    let scan = home.scanner("/a/one");

    set_mode(&project, 0o000);
    let found = find(&scan, "needle", false);
    set_mode(&project, 0o755);

    assert!(found.hits.is_empty());
    assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
    assert!(
        found.problems[0].starts_with(&format!("Could not read: {}: ", sessions.display())),
        "{:?}",
        found.problems
    );

    fs::remove_dir_all(&project).unwrap();
    assert_eq!(find(&scan, "needle", false), Found::default());
}

#[test]
fn an_unreadable_session_log_metadata_is_a_problem_and_a_missing_log_is_silent() {
    assert_ne!(
        effective_uid(),
        0,
        "this test must run as a non-root user: root bypasses file modes"
    );
    let home = Home::new();
    let locked = home.session("-a", "s_locked", "/a/one", &["needle"]);
    let missing = home.sessions("-a").join("s_missing");
    fs::create_dir_all(&missing).unwrap();

    set_mode(&locked, 0o000);
    let found = find(&home.scanner("/a/one"), "needle", false);
    set_mode(&locked, 0o755);

    let events = locked.join(EVENTS);
    assert!(found.hits.is_empty());
    assert_eq!(found.problems.len(), 1, "{:?}", found.problems);
    assert!(
        found.problems[0].starts_with(&format!("Could not read: {}: ", events.display())),
        "{:?}",
        found.problems
    );
}

#[test]
fn an_already_cancelled_call_finds_nothing() {
    let home = Home::new();
    home.session("-b", "s_1", "/b/one", &["needle"]);
    // The own project's `sessions/` is a link: reading it would list it.
    fs::create_dir_all(home.path().join("projects").join("-a")).unwrap();
    symlink(home.sessions("-b"), home.sessions("-a")).unwrap();
    let scan = home.scanner("/a/one");
    let cancel = CancelToken::new();
    cancel.cancel();
    for all_projects in [false, true] {
        let found = scan.scan(&query("needle", all_projects, 10), &cancel);
        assert_eq!(found, Found::default(), "all_projects {all_projects}");
    }
    assert_eq!(find(&scan, "needle", false).problems.len(), 1);
}

#[test]
fn a_cancel_between_chunks_of_a_log_stops_the_scan() {
    let home = Home::new();
    let filler = "f".repeat(4096);
    let mut texts = vec![filler.as_str(); 1024];
    texts.push("needle at the end");
    home.session("-a", "s_1", "/a/one", &texts);
    let scan = home.scanner("/a/one");
    // The scan's own checks and the first-line read pass; a later read of
    // the 4 MiB log sees the cancel.
    let found = scan.scan(&query("needle", true, 10), &After::new(5));
    assert_eq!(found, Found::default());
    assert_eq!(find(&scan, "needle", true).total, 1);
}

#[test]
fn a_cancel_during_the_first_line_read_skips_the_session() {
    let home = Home::new();
    let workspace = format!("/a/{}", "w".repeat(4 << 20));
    home.session("-a", "s_1", &workspace, &["needle"]);
    let scan = home.scanner("/a/one");
    // The scan's, the project's and the session's checks pass; the second
    // read of the 4 MiB first line sees the cancel.
    let found = scan.scan(&query("needle", true, 10), &After::new(4));
    assert_eq!(found, Found::default());
    assert_eq!(find(&scan, "needle", true).total, 1);
}

#[test]
fn a_cancel_after_an_artifact_match_and_before_its_nul_admits_no_hit() {
    let home = Home::new();
    let dir = home.session("-a", "s_1", "/a/one", &[]);
    let mut late = b"needle first\n".to_vec();
    late.extend(std::iter::repeat_n(b'a', 1 << 21));
    late.extend(b"\n\0\n");
    fs::write(dir.join("artifacts").join("a.txt"), &late).unwrap();
    raw(
        &dir,
        b"{\"kind\":\"tool_call_completed\",\"session_id\":\"s_1\",\"ts\":1,\"schema_version\":1,\"seq\":1,\"payload\":{\"artifact\":\"artifacts/a.txt\",\"content\":[],\"status\":\"completed\"}}\n",
    );
    let scan = home.scanner("/a/one");
    for after in 0..24 {
        let found = scan.scan(&query("needle", true, 10), &After::new(after));
        assert!(
            found.hits.is_empty(),
            "cancel after {after}: {:?}",
            found.hits
        );
        assert!(
            found.problems.is_empty(),
            "cancel after {after}: {:?}",
            found.problems
        );
    }
}

#[test]
fn an_entry_that_cannot_be_read_is_a_problem() {
    // An entry read error cannot be provoked portably through `read_dir`
    // (a mode on the parent fails the listing itself), so the shared
    // per-entry handling is tested with an error value directly. Both
    // directory listings route through it.
    let mut problems = Vec::new();
    let dir = Path::new("/projects/-a/sessions");
    let error = std::io::Error::other("boom");
    assert!(entry(dir, Err(error), &mut |problem| problems.push(problem)).is_none());
    assert_eq!(problems, ["Could not read: /projects/-a/sessions: boom"]);
    // An entry that reads passes through untouched.
    let home = Home::new();
    fs::write(home.path().join("f"), "").unwrap();
    let mut entries = fs::read_dir(home.path()).unwrap();
    let next = entries.next().unwrap();
    let path = next.as_ref().unwrap().path();
    let entry = entry(dir, next, &mut |problem| problems.push(problem));
    assert_eq!(entry.map(|e| e.path()), Some(path));
    assert_eq!(problems.len(), 1);
}

#[test]
fn unlistable_sessions_and_unopenable_logs_are_problems() {
    assert_ne!(
        effective_uid(),
        0,
        "this test must run as a non-root user: root bypasses file modes"
    );
    let home = Home::new();
    let locked = home.sessions("-b");
    fs::create_dir_all(&locked).unwrap();
    let dir = home.session("-a", "s_1", "/a/one", &["needle"]);
    let log = dir.join("events.jsonl");
    set_mode(&locked, 0o000);
    set_mode(&log, 0o000);
    let found = find(&home.scanner("/a/one"), "needle", true);
    set_mode(&locked, 0o755);
    set_mode(&log, 0o644);
    assert!(found.hits.is_empty());
    assert_eq!(found.problems.len(), 2, "{:?}", found.problems);
    assert!(
        found.problems[0].starts_with(&format!("Could not read: {}: ", log.display())),
        "{:?}",
        found.problems
    );
    assert!(
        found.problems[1].starts_with(&format!("Could not read: {}: ", locked.display())),
        "{:?}",
        found.problems
    );
}

/// The effective user id, read from a file this process creates.
fn effective_uid() -> u32 {
    use std::os::unix::fs::MetadataExt;
    let dir = TempDir::new("log-search-uid");
    let path = dir.path().join("mine");
    fs::write(&path, "").unwrap();
    fs::metadata(&path).unwrap().uid()
}

#[test]
fn linked_projects_sessions_and_logs_are_skipped_as_problems() {
    let home = Home::new();
    let outside = TempDir::new("log-search-outside");
    let elsewhere = Home {
        dir: outside,
        clock: FakeClock::new(),
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let real = elsewhere.session("-x", "s_out", "/a/one", &["needle"]);
    let projects = home.path().join("projects");
    // A project directory that is a link.
    fs::create_dir_all(&projects).unwrap();
    symlink(
        elsewhere.path().join("projects").join("-x"),
        projects.join("-l"),
    )
    .unwrap();
    // A `sessions/` that is a link.
    fs::create_dir_all(projects.join("-s")).unwrap();
    symlink(
        elsewhere.sessions("-x"),
        projects.join("-s").join("sessions"),
    )
    .unwrap();
    // A session directory and a log that are links.
    let sessions = home.sessions("-a");
    fs::create_dir_all(&sessions).unwrap();
    symlink(&real, sessions.join("s_dir")).unwrap();
    let mine = home.session("-a", "s_log", "/a/one", &[]);
    fs::remove_file(mine.join("events.jsonl")).unwrap();
    symlink(real.join("events.jsonl"), mine.join("events.jsonl")).unwrap();
    let found = find(&home.scanner("/a/one"), "needle", true);
    assert!(found.hits.is_empty(), "{:?}", found.hits);
    let link = |path: PathBuf| format!("{} is a link", path.display());
    assert_eq!(
        found.problems,
        [
            link(sessions.join("s_dir")),
            link(mine.join("events.jsonl")),
            link(projects.join("-l")),
            link(projects.join("-s").join("sessions")),
        ]
    );
    // The own project, without `all_projects`, refuses the same links.
    let found = find(&home.scanner("/a/one"), "needle", false);
    assert!(found.hits.is_empty());
    assert_eq!(found.problems.len(), 2, "{:?}", found.problems);
}

#[test]
fn problems_past_twenty_are_counted() {
    let home = Home::new();
    let dir = home.session("-a", "s_1", "/a/one", &[]);
    for _ in 0..25 {
        raw(&dir, b"{\"kind\": needle\n");
    }
    let found = find(&home.scanner("/a/one"), "needle", false);
    assert_eq!(found.problems.len(), 20);
    assert_eq!(found.more_problems, 5);
    assert!(
        found.problems[19].contains(", line 21: "),
        "{}",
        found.problems[19]
    );
}

#[test]
fn a_directory_whose_first_line_is_not_session_started_is_not_a_session() {
    let home = Home::new();
    let dir = home.sessions("-a").join("s_1");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("events.jsonl"),
        "{\"kind\":\"text_completed\",\"session_id\":\"s_1\",\"ts\":1,\"schema_version\":1,\"seq\":0,\"payload\":{\"text\":\"needle\"}}\n",
    )
    .unwrap();
    assert_eq!(
        find(&home.scanner("/a/one"), "needle", true),
        Found::default()
    );
}

/// A scanner whose identity function cancels `cancel` when it is asked
/// about `/a/one`, so the cancel lands while that session is read.
pub(super) fn cancelling_scanner(home: &Home, cancel: &CancelToken) -> SessionScan {
    let fire = cancel.clone();
    let identity: Identity = Arc::new(move |path: &Path| {
        if path == Path::new("/a/one") {
            fire.cancel();
        }
        PathBuf::from("/a")
    });
    SessionScan::new(home.path(), Path::new("/a/main"), identity)
}

#[test]
fn a_cancel_is_seen_before_the_next_session() {
    let home = Home::new();
    let first = home.session("-a", "s_1", "/a/one", &["needle"]);
    // The next session's log is a link: reading on would list it.
    let next = home.session("-a", "s_2", "/a/one", &[]);
    fs::remove_file(next.join("events.jsonl")).unwrap();
    symlink(first.join("events.jsonl"), next.join("events.jsonl")).unwrap();
    let cancel = CancelToken::new();
    // One worker: a second could read the linked log before the cancel.
    let found = cancelling_scanner(&home, &cancel).search(
        &query("needle", false, 10),
        &cancel,
        Ok(NonZeroUsize::MIN),
    );
    assert_eq!(found, Found::default());
}

#[test]
fn a_cancel_is_seen_before_the_next_artifact() {
    let home = Home::new();
    let dir = home.session("-a", "s_1", "/a/one", &["needle"]);
    // The first artifact is a link: reading on would list it.
    symlink(
        dir.join("events.jsonl"),
        dir.join("artifacts").join("a.txt"),
    )
    .unwrap();
    let cancel = CancelToken::new();
    let found = cancelling_scanner(&home, &cancel).scan(&query("needle", false, 10), &cancel);
    assert_eq!(found, Found::default());
}

#[test]
fn entries_that_are_not_sessions_are_passed_over_silently() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/one", &["needle"]);
    let sessions = home.sessions("-a");
    // A file beside the sessions, a directory with no log, and a log that
    // is a directory.
    fs::write(sessions.join("stray"), "needle").unwrap();
    fs::create_dir_all(sessions.join("s_empty")).unwrap();
    fs::create_dir_all(sessions.join("s_odd").join("events.jsonl")).unwrap();
    // A project with no `sessions/`, and one whose `sessions` is a file.
    fs::create_dir_all(home.path().join("projects").join("-n")).unwrap();
    fs::create_dir_all(home.path().join("projects").join("-f")).unwrap();
    fs::write(home.path().join("projects").join("-f").join("sessions"), "").unwrap();
    let found = find(&home.scanner("/a/one"), "needle", true);
    assert_eq!(found.total, 1);
    assert!(found.problems.is_empty(), "{:?}", found.problems);
}

#[test]
fn a_cancel_is_seen_before_the_next_project() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/one", &["needle"]);
    // The next project is a link: reading on would list it.
    symlink(home.path(), home.path().join("projects").join("-b")).unwrap();
    let scan = home.scanner("/a/one");
    let counted = After::new(usize::MAX);
    let found = scan.scan(&query("needle", true, 10), &counted);
    assert_eq!((found.total, found.problems.len()), (1, 1));
    // The scan's check, the one before `-a`, then the one before the link:
    // every project is listed before any session is read.
    let found = scan.scan(&query("needle", true, 10), &After::new(2));
    assert_eq!(found, Found::default());
}
