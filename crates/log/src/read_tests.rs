use super::*;
use crate::open_log;
use contract::SessionId;
use std::io::SeekFrom;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use fakes::Deadline;

const DEADLINE: Duration = Duration::from_secs(10);

fn write(dir: &std::path::Path, bytes: &[u8]) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), bytes).unwrap();
}

fn project(home: &std::path::Path, key: &str, id: &str, logged: bool) {
    let dir = home.join("projects").join(key).join("sessions").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    if logged {
        std::fs::write(
            dir.join("events.jsonl"),
            b"{\"kind\":\"session_started\"}\n",
        )
        .unwrap();
    }
}

#[test]
fn kind_of_reads_the_kind_string() {
    assert_eq!(
        kind_of(br#"{"kind":"fiber_exited"}"#).as_deref(),
        Some("fiber_exited")
    );
    assert_eq!(kind_of(b"not json"), None);
    assert_eq!(kind_of(br#"{"kind":7}"#), None);
    assert_eq!(kind_of(br#"{"ts":1}"#), None);
}

#[test]
fn last_kind_reads_only_a_whole_last_line() {
    let home = fakes::TempDir::new("log-last-kind");
    let dir = home.path().join("s");
    std::fs::create_dir_all(&dir).unwrap();
    // Missing and empty logs give nothing.
    assert_eq!(last_kind(&dir), None);
    write(&dir, b"");
    assert_eq!(last_kind(&dir), None);
    write(&dir, b"{\"kind\":\"a\"}\n{\"kind\":\"fiber_exited\"}\n");
    assert_eq!(last_kind(&dir).as_deref(), Some("fiber_exited"));
    // A final line without a trailing newline still counts.
    write(&dir, b"{\"kind\":\"fiber_exited\"}");
    assert_eq!(last_kind(&dir).as_deref(), Some("fiber_exited"));
    // A torn tail does not parse.
    write(&dir, b"{\"kind\":\"fiber_exited\"}\n{\"kind\":\"tu");
    assert_eq!(last_kind(&dir), None);
    // A long log: only its tail is read.
    let filler = format!("{{\"kind\":\"x\",\"p\":\"{}\"}}\n", "y".repeat(100_000));
    write(
        &dir,
        format!("{filler}{{\"kind\":\"rewound\"}}\n").as_bytes(),
    );
    assert_eq!(last_kind(&dir).as_deref(), Some("rewound"));
    // A last line longer than the tail is neither exit line.
    write(
        &dir,
        format!("{{\"kind\":\"fiber_exited\"}}\n{filler}").as_bytes(),
    );
    assert_eq!(last_kind(&dir), None);
    // Even when the tail of that line happens to parse on its own.
    let inner = |pad: usize| {
        format!(
            "{{\"kind\":\"fiber_exited\",\"p\":\"{}\"}}",
            "y".repeat(pad)
        )
    };
    let pad = usize::try_from(TAIL).unwrap() - 1 - inner(0).len();
    write(&dir, format!("not json {}\n", inner(pad)).as_bytes());
    assert_eq!(last_kind(&dir), None);
    // A last line well inside the tail reads whole.
    let long = format!("{{\"kind\":\"z\",\"p\":\"{}\"}}\n", "y".repeat(2_000));
    write(&dir, format!("{filler}{long}").as_bytes());
    assert_eq!(last_kind(&dir).as_deref(), Some("z"));
}

#[test]
fn complete_len_of_stops_at_the_last_newline() {
    let home = fakes::TempDir::new("log-complete-len");
    let dir = home.path().join("s");
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(complete_len_of(&dir), 0);
    write(&dir, b"");
    assert_eq!(complete_len_of(&dir), 0);
    write(&dir, b"abc");
    assert_eq!(complete_len_of(&dir), 0);
    write(&dir, b"abc\n");
    assert_eq!(complete_len_of(&dir), 4);
    write(&dir, b"ab\ncd\nXY");
    assert_eq!(complete_len_of(&dir), 6);
    write(&dir, b"ab\ncd\n");
    assert_eq!(complete_len_of(&dir), 6);
}

#[test]
fn complete_len_of_scans_back_across_chunk_edges() {
    let home = fakes::TempDir::new("log-complete-edge");
    let dir = home.path().join("s");
    let tail = usize::try_from(TAIL).unwrap();
    // No newline across more than three chunks reads to the start.
    write(&dir, &vec![b'x'; tail * 3 + 7]);
    assert_eq!(complete_len_of(&dir), 0);
    // The last newline's distance past it decides the chunks read:
    // one byte, exactly one chunk, and one past it.
    for suffix in [1, tail, tail + 1] {
        let mut bytes = vec![b'x'; tail * 2];
        bytes.push(b'\n');
        bytes.extend_from_slice(&vec![b'y'; suffix]);
        write(&dir, &bytes);
        assert_eq!(
            complete_len_of(&dir),
            tail as u64 * 2 + 1,
            "suffix {suffix} past the last newline"
        );
    }
    // A torn tail longer than two chunks is dropped: the end stays at
    // the last newline before it.
    let mut bytes = vec![b'x'; 3];
    bytes.push(b'\n');
    bytes.extend_from_slice(&vec![b'y'; tail * 2 + 11]);
    write(&dir, &bytes);
    assert_eq!(complete_len_of(&dir), 4);
}

#[test]
fn complete_len_of_a_long_log_reads_at_most_two_chunks() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting<R> {
        inner: R,
        bytes: AtomicUsize,
    }
    impl<R: std::io::Read> std::io::Read for Counting<R> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.bytes.fetch_add(n, Ordering::SeqCst);
            Ok(n)
        }
    }
    impl<R: std::io::Seek> std::io::Seek for Counting<R> {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }
    let tail = usize::try_from(TAIL).unwrap();
    let mut bytes = vec![b'x'; 8 * 1024 * 1024 - 40];
    bytes.extend_from_slice(b"{\"kind\":\"fiber_exited\"}\n");
    bytes.extend_from_slice(b"torn");
    let cursor = std::io::Cursor::new(bytes.clone());
    let len = bytes.len() as u64;
    let mut counting = Counting {
        inner: cursor,
        bytes: AtomicUsize::new(0),
    };
    let end = complete_len_in(&mut counting, len).unwrap();
    assert_eq!(end, len - 4);
    assert!(
        counting.bytes.load(Ordering::SeqCst) <= tail * 2,
        "read {} bytes, at most two chunks",
        counting.bytes.load(Ordering::SeqCst)
    );
}

#[test]
fn tail_lines_holds_the_window_complete_lines() {
    let home = fakes::TempDir::new("log-tail-lines");
    let dir = home.path().join("s");
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(tail_lines(&dir), Vec::<Vec<u8>>::new());
    let turn = |ts: u64| format!("{{\"kind\":\"turn_completed\",\"ts\":{ts}}}");
    write(&dir, format!("{}\n{}\n", turn(1), turn(2)).as_bytes());
    assert_eq!(
        tail_lines(&dir),
        vec![turn(1).into_bytes(), turn(2).into_bytes()]
    );
    // A torn last line is dropped.
    write(&dir, format!("{}\npartial", turn(3)).as_bytes());
    assert_eq!(tail_lines(&dir), vec![turn(3).into_bytes()]);
    // Empty lines are dropped.
    write(&dir, format!("\n{}\n\n", turn(4)).as_bytes());
    assert_eq!(tail_lines(&dir), vec![turn(4).into_bytes()]);
}

#[test]
fn tail_lines_at_a_window_edge_counts_a_whole_line_but_not_a_partial_one() {
    let home = fakes::TempDir::new("log-tail-edge");
    let dir = home.path().join("s");
    let tail = usize::try_from(TAIL).unwrap();
    let target = b"{\"kind\":\"turn_completed\",\"ts\":7}\n";
    let filler = b"{\"kind\":\"x\",\"ts\":1}\n";
    let pad = |n: usize| {
        let mut out = filler.repeat(n / filler.len());
        out.extend_from_slice(&vec![b'\n'; n % filler.len()]);
        out
    };
    let suffix = pad(tail - target.len());
    assert_eq!(target.len() + suffix.len(), tail);
    std::fs::create_dir_all(&dir).unwrap();
    let mut bytes = filler.to_vec();
    bytes.extend_from_slice(target);
    bytes.extend_from_slice(&suffix);
    write(&dir, &bytes);
    assert!(
        tail_lines(&dir)
            .iter()
            .any(|line| line == &target[..target.len() - 1])
    );
    // The window starts inside a trash line: its remainder is partial.
    let mut bytes = filler.to_vec();
    bytes.extend_from_slice(b"TRASH");
    bytes.extend_from_slice(target);
    bytes.extend_from_slice(&suffix);
    write(&dir, &bytes);
    assert!(
        !tail_lines(&dir)
            .iter()
            .any(|line| line == &target[..target.len() - 1])
    );
}

#[test]
fn lines_from_hands_complete_lines_from_an_offset() {
    let home = fakes::TempDir::new("log-lines-from");
    let dir = home.path().join("s");
    // A missing log is None.
    std::fs::create_dir_all(&dir).unwrap();
    assert!(lines_from(&dir, 0, |_| {}).is_none());
    write(&dir, b"");
    let scan = lines_from(&dir, 0, |_| panic!("empty hands nothing")).unwrap();
    assert_eq!((scan.end, scan.restarted), (0, false));
    write(&dir, b"{\"kind\":\"a\"}\n{\"kind\":\"b\"}\n");
    let mut seen = Vec::new();
    let scan = lines_from(&dir, 0, |line| seen.push(line.to_vec())).unwrap();
    assert_eq!(
        seen,
        vec![b"{\"kind\":\"a\"}".to_vec(), b"{\"kind\":\"b\"}".to_vec()]
    );
    assert_eq!((scan.end, scan.restarted), (26, false));
    // Mid-file at a line start.
    let mut seen = Vec::new();
    let scan = lines_from(&dir, 13, |line| seen.push(line.to_vec())).unwrap();
    assert_eq!(seen, vec![b"{\"kind\":\"b\"}".to_vec()]);
    assert_eq!((scan.end, scan.restarted), (26, false));
    // At the end hands nothing.
    let scan = lines_from(&dir, 26, |_| panic!("at end hands nothing")).unwrap();
    assert_eq!((scan.end, scan.restarted), (26, false));
    // Past the end restarts from zero.
    let mut seen = Vec::new();
    let scan = lines_from(&dir, 99, |line| seen.push(line.to_vec())).unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!((scan.end, scan.restarted), (26, true));
    // A partial tail is left unread, then read once it completes.
    write(&dir, b"{\"kind\":\"a\"}\npartial");
    let mut seen = Vec::new();
    let scan = lines_from(&dir, 0, |line| seen.push(line.to_vec())).unwrap();
    assert_eq!(seen, vec![b"{\"kind\":\"a\"}".to_vec()]);
    assert_eq!(scan.end, 13);
    write(&dir, b"{\"kind\":\"a\"}\npartial\n");
    let mut seen = Vec::new();
    let scan = lines_from(&dir, scan.end, |line| seen.push(line.to_vec())).unwrap();
    assert_eq!(seen, vec![b"partial".to_vec()]);
    assert_eq!(scan.end, 21);
}

#[test]
fn find_session_returns_the_first_project_in_key_order() {
    let home = fakes::TempDir::new("log-find-session");
    let id = SessionId("s_aaaaaaaaaaaaaa".to_owned());
    assert_eq!(find_session(home.path(), &id), None);
    project(home.path(), "b", &id.0, false);
    let (key, dir) = find_session(home.path(), &id).unwrap();
    assert_eq!(key, "b");
    assert_eq!(
        dir,
        home.path()
            .join("projects")
            .join("b")
            .join("sessions")
            .join(&id.0)
    );
    // Both hold it: the earlier key wins.
    project(home.path(), "a", &id.0, false);
    let (key, _) = find_session(home.path(), &id).unwrap();
    assert_eq!(key, "a");
    // A directory without a log is still found.
    assert!(find_session(home.path(), &id).is_some());
    // Bad ids give nothing.
    for bad in ["", "..", ".", "a/b", "a\\b"] {
        assert_eq!(
            find_session(home.path(), &SessionId(bad.to_owned())),
            None,
            "{bad}"
        );
    }
}

#[test]
fn find_logged_session_skips_a_directory_without_a_log() {
    let home = fakes::TempDir::new("log-find-logged");
    let id = SessionId("s_aaaaaaaaaaaaaa".to_owned());
    project(home.path(), "a", &id.0, false);
    project(home.path(), "b", &id.0, true);
    // `find_session` sees the earlier directory; the logged lookup
    // skips it for the later log.
    assert_eq!(find_session(home.path(), &id).unwrap().0, "a");
    assert_eq!(find_logged_session(home.path(), &id).unwrap().0, "b");
    // Both hold logs: the earlier key wins.
    project(home.path(), "a2", &id.0, true);
    std::fs::write(
        home.path()
            .join("projects")
            .join("a")
            .join("sessions")
            .join(&id.0)
            .join("events.jsonl"),
        b"x\n",
    )
    .unwrap();
    assert_eq!(find_logged_session(home.path(), &id).unwrap().0, "a");
    // A directory named events.jsonl does not count.
    let home2 = fakes::TempDir::new("log-find-logged-dir");
    let dir = home2
        .path()
        .join("projects")
        .join("a")
        .join("sessions")
        .join(&id.0);
    std::fs::create_dir_all(dir.join("events.jsonl")).unwrap();
    assert_eq!(find_logged_session(home2.path(), &id), None);
    assert_eq!(
        find_logged_session(home.path(), &SessionId("s_missing".to_owned())),
        None
    );
    for bad in ["", "..", ".", "a/b"] {
        assert_eq!(
            find_logged_session(home.path(), &SessionId(bad.to_owned())),
            None,
            "{bad}"
        );
    }
}

fn line() -> Envelope {
    Envelope {
        kind: "x".into(),
        session_id: SessionId("s".into()),
        ts: 0,
        schema_version: 1,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: Default::default(),
    }
}

#[test]
fn open_log_on_a_missing_log_is_not_found_naming_the_session_directory() {
    let home = fakes::TempDir::new("log-open-log-missing");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    std::fs::create_dir_all(&dir).unwrap();
    let Err(Error::NotFound(path)) = open_log(&dir) else {
        panic!("a missing log opens");
    };
    assert_eq!(path, dir);
}

#[test]
fn open_log_under_a_regular_file_is_io_naming_the_log() {
    // The session path is a file, so the log under it cannot open for a
    // reason other than a missing log (`ENOTDIR`).
    let home = fakes::TempDir::new("log-open-log-file");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    std::fs::write(&dir, b"not a directory").unwrap();
    let Err(Error::Io { path, .. }) = open_log(&dir) else {
        panic!("a log under a file opens");
    };
    assert_eq!(path, dir.join("events.jsonl"));
}

#[test]
fn open_log_on_an_existing_log_opens_it_for_reading() {
    use std::io::Read as _;
    let home = fakes::TempDir::new("log-open-log-present");
    let dir = home.path().join("s_aaaaaaaaaaaaaaaa");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), b"{\"kind\":\"x\"}\n").unwrap();
    let mut file = open_log(&dir).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"{\"kind\":\"x\"}\n");
}

#[test]
fn pending_is_false_for_an_empty_open_queue() {
    let state = State::default();
    assert!(!pending(&state));
}

#[test]
fn pending_is_true_with_a_queued_line() {
    let state = State {
        queue: VecDeque::from([line()]),
        ..Default::default()
    };
    assert!(pending(&state));
}

#[test]
fn pending_is_true_when_lagged() {
    let state = State {
        lagged: true,
        ..Default::default()
    };
    assert!(pending(&state));
}

#[test]
fn pending_is_true_when_closed() {
    let state = State {
        end: End::Closed,
        ..Default::default()
    };
    assert!(pending(&state));
}

#[test]
fn pending_is_true_when_failed() {
    let state = State {
        end: End::Failed {
            session: "s".into(),
            cause: "c".into(),
        },
        ..Default::default()
    };
    assert!(pending(&state));
}

fn kept_line(kind: &str) -> Envelope {
    serde_json::from_str(&crate::fixtures::line(
        kind,
        "s_1",
        1,
        0,
        &serde_json::json!({}),
    ))
    .unwrap()
}

fn ended_watcher() -> (fakes::TempDir, Watcher) {
    let sessions = fakes::TempDir::new("log-unit-after-end");
    let log = crate::Log::create(
        sessions.path(),
        SessionId("s_1".into()),
        fakes::clock::FakeClock::new(),
    )
    .unwrap();
    let mut watcher = log.watch();
    drop(log);
    assert!(
        watcher
            .recv_timeout(DEADLINE)
            .expect("dropping the log ends its watchers")
            .unwrap()
            .is_none(),
        "the log's end"
    );
    (sessions, watcher)
}

#[test]
fn recv_after_end_returns_a_kept_line_pushed_after_the_end() {
    let (_sessions, mut watcher) = ended_watcher();
    watcher.injector().push_kept(kept_line("late"));
    assert_eq!(watcher.recv_after_end().kind, "late");
}

#[test]
fn recv_after_end_waits_while_nothing_is_queued() {
    let (_sessions, mut watcher) = ended_watcher();
    let injector = watcher.injector();
    let (tx, rx) = mpsc::channel();
    thread::spawn(
        move || {
            if let Ok(()) = tx.send(watcher.recv_after_end().kind) {}
        },
    );
    assert!(
        Deadline::after(Duration::from_millis(200))
            .recv(&rx)
            .is_err(),
        "nothing queued: the call waits instead of returning the end"
    );
    injector.push_kept(kept_line("late"));
    assert_eq!(
        Deadline::after(Duration::from_secs(10))
            .recv(&rx)
            .expect("the pushed line wakes the wait"),
        "late"
    );
}
