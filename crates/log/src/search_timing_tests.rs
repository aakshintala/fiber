//! `session_search.scan` timing: one line per scan request through the
//! installed timing, spanning the whole request.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::Duration;

use contract::clock::Clock;
use contract::session_search::Scan;
use fakes::CancelToken;
use fakes::clock::FakeClock;

use super::tests::{After, Home, query};
use super::{Identity, SessionScan};
use crate::diag::{Diag, Level, Process};

/// A timed scanner over `home` for a session in `/a/main`: the identity
/// advances `clock` by `step` on each call and counts calls. Nothing runs
/// before `scan`, so the own-project call lands inside the scan.
fn timed(
    home: &Home,
    clock: &Arc<FakeClock>,
    diag: &Diag,
    step: Duration,
    calls: &Arc<AtomicUsize>,
) -> SessionScan {
    let clock = Arc::clone(clock);
    let calls = Arc::clone(calls);
    let identity: Identity = Arc::new(move |path: &Path| {
        calls.fetch_add(1, AtomicOrdering::SeqCst);
        // No code under test waits on the clock here; it stamps elapsed time (ruling 6179406086).
        #[allow(
            clippy::disallowed_methods,
            reason = "no code under test waits on the clock (ruling 6179406086)"
        )]
        clock.advance(step);
        for project in ["/b", "/c"] {
            if path.starts_with(project) {
                return PathBuf::from(project);
            }
        }
        PathBuf::from("/a")
    });
    SessionScan::new(home.path(), Path::new("/a/main"), identity).with_timing(diag.timings())
}

/// The `op_timing` lines in the home's `hub.log`, parsed.
fn timing_lines(home: &Home) -> Vec<serde_json::Value> {
    let path = home.path().join("logs").join("hub.log");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// Asserts one `session_search.scan` line with exactly `op` and `ms`.
fn assert_one_scan_line(home: &Home) -> u64 {
    let lines = timing_lines(home);
    assert_eq!(lines.len(), 1, "one line per scan: {lines:?}");
    let line = &lines[0];
    assert_eq!(line["code"], "op_timing");
    let data = line["data"].as_object().expect("data is an object");
    let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["ms", "op"], "closed data keys: {line}");
    assert_eq!(data["op"], "session_search.scan");
    data["ms"].as_u64().expect("ms is a number")
}

fn debug_diag(home: &Home, clock: Arc<FakeClock>) -> Diag {
    Diag::new(home.path(), Process::Hub, Level::Debug, clock)
}

#[test]
fn an_own_project_hit_writes_one_scan_line() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/main", &["needle own"]);
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let found = scan.scan(&query("needle", false, 100), &CancelToken::new());
    assert_eq!(found.total, 1);
    assert_one_scan_line(&home);
}

#[test]
fn an_all_projects_hit_writes_one_scan_line() {
    let home = Home::new();
    home.session("-b", "s_far", "/b/far", &["needle far"]);
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let found = scan.scan(&query("needle", true, 100), &CancelToken::new());
    assert_eq!(found.total, 1);
    assert_one_scan_line(&home);
}

#[test]
fn a_scan_without_hits_writes_one_scan_line() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/main", &["something else"]);
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let found = scan.scan(&query("needle", false, 100), &CancelToken::new());
    assert_eq!(found.total, 0);
    assert_one_scan_line(&home);
}

#[test]
fn an_empty_text_scan_writes_one_scan_line() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/main", &["needle"]);
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let found = scan.scan(&query("", false, 100), &CancelToken::new());
    assert_eq!(found.total, 0);
    assert_one_scan_line(&home);
}

#[test]
fn a_pre_cancelled_scan_writes_one_scan_line() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/main", &["needle"]);
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let cancel = CancelToken::new();
    cancel.cancel();
    let found = scan.scan(&query("needle", false, 100), &cancel);
    assert_eq!(found.total, 0);
    assert_one_scan_line(&home);
}

#[test]
fn a_cancel_mid_scan_writes_one_scan_line() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/main", &["needle"]);
    home.session("-a", "s_2", "/a/main", &["needle"]);
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let found = scan.scan(&query("needle", false, 100), &After::new(1));
    assert!(found.total <= 2, "{found:?}");
    assert_one_scan_line(&home);
}

#[test]
fn a_problem_only_scan_writes_one_scan_line() {
    use std::os::unix::fs::symlink;
    let home = Home::new();
    home.session("-a", "s_1", "/a/other", &["something else"]);
    symlink(home.path(), home.path().join("projects").join("-b")).unwrap();
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let found = scan.scan(&query("needle", true, 100), &CancelToken::new());
    assert_eq!(found.total, 0);
    assert!(!found.problems.is_empty(), "{found:?}");
    assert_one_scan_line(&home);
}

#[test]
fn elapsed_spans_the_whole_request() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/one", &["needle"]);
    home.session("-a", "s_2", "/a/two", &["needle"]);
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let step = Duration::from_millis(3);
    // A fresh scanner, nothing called before `scan`, so every identity
    // call below lands inside the scan.
    let scan = timed(&home, &clock, &diag, step, &calls);
    let found = scan.scan(&query("needle", false, 100), &CancelToken::new());
    assert_eq!(found.total, 2);
    let calls = calls.load(AtomicOrdering::SeqCst);
    // The own workspace plus one per distinct workspace.
    assert_eq!(calls, 3, "the whole request runs under the guard");
    let ms = assert_one_scan_line(&home);
    assert_eq!(ms, 3 * calls as u64, "one step per identity call");
}

#[test]
fn hit_caps_match_an_untimed_scanner_with_one_line_each() {
    for (texts, hits) in [
        (vec!["needle 1", "needle 2"], 2_usize),
        (vec!["needle 1", "needle 2", "needle 3"], 3_usize),
        (
            vec!["needle 1", "needle 2", "needle 3", "needle 4"],
            3_usize,
        ),
    ] {
        let home = Home::new();
        home.session("-a", "s_1", "/a/one", &texts);
        let clock = FakeClock::new();
        let diag = debug_diag(&home, Arc::clone(&clock));
        let calls = Arc::new(AtomicUsize::new(0));
        let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
        let found = scan.scan(&query("needle", false, 3), &CancelToken::new());
        let plain = home
            .scanner("/a/main")
            .scan(&query("needle", false, 3), &CancelToken::new());
        assert_eq!(found, plain, "timing changes no hit");
        assert_eq!(found.hits.len(), hits);
        assert_one_scan_line(&home);
    }
}

#[test]
fn problem_caps_match_an_untimed_scanner_with_one_line_each() {
    use super::tests::raw;
    for (corrupt, problems, more) in [(19, 19, 0), (20, 20, 0), (21, 20, 1)] {
        let home = Home::new();
        let dir = home.session("-a", "s_1", "/a/one", &[]);
        for _ in 0..corrupt {
            raw(&dir, b"{\"kind\": needle\n");
        }
        let clock = FakeClock::new();
        let diag = debug_diag(&home, Arc::clone(&clock));
        let calls = Arc::new(AtomicUsize::new(0));
        let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
        let found = scan.scan(&query("needle", false, 100), &CancelToken::new());
        let plain = home
            .scanner("/a/one")
            .scan(&query("needle", false, 100), &CancelToken::new());
        assert_eq!(found, plain, "timing changes no problem");
        assert_eq!(found.problems.len(), problems, "{found:?}");
        assert_eq!(found.more_problems, more, "{found:?}");
        assert_one_scan_line(&home);
    }
}

#[test]
fn an_untimed_scanner_writes_nothing() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/main", &["needle"]);
    let found = home
        .scanner("/a/main")
        .scan(&query("needle", false, 100), &CancelToken::new());
    assert_eq!(found.total, 1);
    assert!(
        !home.path().join("logs").exists(),
        "no diagnostic file without a timing"
    );
}

#[test]
fn a_scanner_timed_by_an_info_diag_writes_nothing() {
    let home = Home::new();
    home.session("-a", "s_1", "/a/main", &["needle"]);
    let clock: Arc<dyn Clock> = FakeClock::new();
    let diag = Diag::new(home.path(), Process::Hub, Level::Info, clock);
    let scan = SessionScan::new(
        home.path(),
        Path::new("/a/main"),
        Arc::new(|path: &Path| {
            for project in ["/b", "/c"] {
                if path.starts_with(project) {
                    return PathBuf::from(project);
                }
            }
            PathBuf::from("/a")
        }),
    )
    .with_timing(diag.timings());
    let found = scan.scan(&query("needle", false, 100), &CancelToken::new());
    assert_eq!(found.total, 1);
    assert!(
        !home.path().join("logs").exists(),
        "no diagnostic file at info"
    );
}

#[test]
fn a_scan_line_carries_no_query_snippet_path_or_content() {
    let home = Home::new();
    let sentinel = "sentinel-q-8f3a-needle";
    home.session("-a", "s_1", "/a/main", &[sentinel]);
    let dir = home.sessions("-a").join("s_1");
    std::fs::create_dir_all(dir.join("artifacts")).unwrap();
    std::fs::write(dir.join("artifacts").join("a.txt"), sentinel).unwrap();
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let found = scan.scan(&query(sentinel, false, 100), &CancelToken::new());
    assert!(found.total >= 1, "{found:?}");
    let text = std::fs::read_to_string(home.path().join("logs").join("hub.log")).unwrap();
    assert!(!text.contains(sentinel), "{text}");
    assert!(!text.contains(&home.path().display().to_string()), "{text}");
    assert!(!text.contains("a.txt"), "{text}");
    assert_one_scan_line(&home);
}

#[cfg(target_os = "linux")]
#[test]
fn a_non_utf8_session_name_still_writes_only_op_and_ms() {
    use std::os::unix::ffi::OsStrExt as _;
    let home = Home::new();
    home.session("-a", "s_1", "/a/main", &["needle"]);
    let sessions = home.sessions("-a");
    let odd = sessions.join(std::ffi::OsStr::from_bytes(b"s_\xff"));
    std::fs::create_dir_all(&odd).unwrap();
    std::fs::write(
        odd.join("events.jsonl"),
        "{\"kind\":\"session_started\",\"session_id\":\"s_\\u00ff\",\"ts\":1,\"schema_version\":1,\"seq\":0,\"payload\":{\"workspace\":\"/a/main\",\"variables\":{\"path\":\"/bin\",\"names\":[],\"source\":\"inherited\"}}}\n",
    )
    .unwrap();
    let clock = FakeClock::new();
    let diag = debug_diag(&home, Arc::clone(&clock));
    let calls = Arc::new(AtomicUsize::new(0));
    let scan = timed(&home, &clock, &diag, Duration::from_millis(0), &calls);
    let found = scan.scan(&query("needle", false, 100), &CancelToken::new());
    assert!(found.total >= 1, "{found:?}");
    assert_one_scan_line(&home);
}
