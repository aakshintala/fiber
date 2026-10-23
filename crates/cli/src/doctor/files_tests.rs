//! The log scan, crash directory, ages and cleaner: planted fixtures,
//! window caps and forced descriptor replacements.

#![allow(clippy::unwrap_used, reason = "test helpers; a failure is the test's")]

use std::fs::{self, File};
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

use super::{
    CHUNK, WINDOW, age, clean, cut_display, lines_after, newest_crash, newest_error, seam,
};

/// One wait for every worker in this file.
const HANG: Duration = Duration::from_secs(10);

/// How long a second chunk is waited for when none should follow.
const ABSENT: Duration = Duration::from_secs(2);

/// A home with `logs/` and `crashes/` under it.
struct Setup {
    root: fakes::TempDir,
}

impl Setup {
    fn new() -> Self {
        let root = fakes::TempDir::new("fiber-doctor-files");
        fs::create_dir_all(root.path().join("home/logs")).unwrap();
        fs::create_dir_all(root.path().join("home/crashes")).unwrap();
        Self { root }
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("home")
    }

    fn logs(&self) -> PathBuf {
        self.home().join("logs")
    }

    fn crashes(&self) -> PathBuf {
        self.home().join("crashes")
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let path = self.home().join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }
}

/// An error line with this `ts`, code and message.
fn error(ts: u64, code: &str, message: &str) -> String {
    format!(
        "{{\"ts\":{ts},\"level\":\"error\",\"process\":\"hub\",\"code\":\"{code}\",\"message\":\"{message}\"}}\n"
    )
}

#[test]
fn age_renders_the_largest_whole_unit() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let cases = [
        (0, "0s"),
        (59, "59s"),
        (60, "1m"),
        (3_599, "59m"),
        (3_600, "1h"),
        (86_399, "23h"),
        (86_400, "1d"),
    ];
    for (secs, want) in cases {
        assert_eq!(age(now, now - Duration::from_secs(secs)), want);
    }
    assert_eq!(age(now, now + Duration::from_secs(60)), "0s");
}

#[test]
fn clean_replaces_every_control_with_a_space() {
    assert_eq!(clean("a\u{1b}[31mb\u{9b}c\u{7f}d\ne"), "a [31mb c d e");
}

#[test]
fn an_error_message_with_escapes_cleans_to_spaces() {
    // The report prints the cleaned message: controls from a log line
    // never reach the terminal raw.
    let setup = Setup::new();
    setup.write(
        "logs/hub.log",
        "{\"ts\":1699000000001,\"level\":\"error\",\"code\":\"e\",\"message\":\"\\u001b[31mred\\u009b\"}\n"
            .as_bytes(),
    );
    let found = newest_error(&setup.logs()).unwrap();
    assert_eq!(clean(&found.3), " [31mred ");
}

#[test]
fn newest_error_picks_the_newest_error_line() {
    let setup = Setup::new();
    setup.write(
        "logs/hub.log",
        error(1_699_000_000_000, "old", "older").as_bytes(),
    );
    setup.write(
        "logs/session-123.log",
        format!(
            "{}{{\"ts\":1699000000001,\"level\":\"warn\",\"code\":\"w\",\"message\":\"newer\"}}\nnot json\n",
            error(1_699_000_000_001, "new", "newest")
        )
        .as_bytes(),
    );
    setup.write("logs/binary.log", &[0xff, 0xfe, b'\n']);
    setup.write(
        "logs/string-ts.log",
        b"{\"ts\":\"1699000000002\",\"level\":\"error\",\"code\":\"s\",\"message\":\"string\"}\n",
    );
    setup.write(
        "logs/real-error.log",
        error(1_699_000_000_000, "tied", "older file").as_bytes(),
    );
    // A symbolic link to a file holding a newer error is skipped: the
    // target lives outside `logs/`, so only the link is listed.
    setup.write(
        "outside.log",
        error(1_699_999_999_999, "link", "linked").as_bytes(),
    );
    symlink(
        setup.home().join("outside.log"),
        setup.logs().join("zzz-link.log"),
    )
    .unwrap();
    // Equal `ts` errors in two files: the later name wins.
    setup.write(
        "logs/aaa.log",
        error(1_699_000_000_001, "tied", "earlier").as_bytes(),
    );
    let found = newest_error(&setup.logs()).unwrap();
    assert_eq!(found.0, "session-123.log");
    assert_eq!(found.1, 1_699_000_000_001);
    assert_eq!((found.2.as_str(), found.3.as_str()), ("new", "newest"));
}

#[test]
fn newest_error_is_none_without_logs() {
    let setup = Setup::new();
    fs::remove_dir_all(setup.logs()).unwrap();
    assert_eq!(newest_error(&setup.logs()), None);
    fs::write(setup.logs(), "not a directory").unwrap();
    assert_eq!(newest_error(&setup.logs()), None);
}

#[test]
fn newest_error_reads_only_the_last_10_mib() {
    let setup = Setup::new();
    // About 10 MiB past an old error: the window starts inside the
    // padding, so the newest line straddling its start is not parsed and
    // the complete older error in the window wins.
    let mut big = error(1_699_000_000_000, "early", "too early").into_bytes();
    big.extend(std::iter::repeat_n(b'p', 10 * 1024 * 1024));
    big.extend_from_slice(error(1_699_000_000_009, "newest", "straddles").as_bytes());
    big.extend_from_slice(error(1_699_000_000_005, "late", "in window").as_bytes());
    setup.write("logs/big.log", &big);
    setup.write(
        "logs/small.log",
        error(1_699_000_000_003, "small", "other").as_bytes(),
    );
    drop(seam::take_chunks_for("big.log"));
    let found = newest_error(&setup.logs()).unwrap();
    assert_eq!(found.0, "big.log");
    assert_eq!((found.2.as_str(), found.3.as_str()), ("late", "in window"));
    let chunks = seam::take_chunks_for("big.log");
    // The window starts at the file's end minus 10 MiB, and never more
    // than 10 MiB is read from the file.
    assert_eq!(
        chunks.first().unwrap().0,
        big.len() as u64 - 10 * 1024 * 1024
    );
    let read: usize = chunks.iter().map(|(_, len)| len).sum();
    assert!(read <= 10 * 1024 * 1024, "{read}");
}

#[test]
fn newest_error_stops_at_the_open_time_length() {
    let setup = Setup::new();
    // Four chunks at open: the error, then padding past one chunk.
    let mut grown = error(1_699_000_000_000, "first", "open-time").into_bytes();
    grown.extend(std::iter::repeat_n(b'q', 200_000));
    let open_length = grown.len();
    setup.write("logs/grow.log", &grown);
    let (arrived, release, _pause) = seam::arm_before_chunk("grow.log", 0);
    drop(seam::take_chunks_for("grow.log"));
    let logs = setup.logs();
    let (done_tx, done_rx) = mpsc::channel();
    let deadline = fakes::Deadline::after(HANG);
    let worker_logs = logs.clone();
    let worker = std::thread::spawn(move || {
        drop(done_tx.send(newest_error(&worker_logs)));
    });
    deadline.recv(&arrived).unwrap();
    // The scan holds the open-time length: bytes appended now are never
    // read, so their newer error is never a candidate.
    let mut longer = grown.clone();
    longer.extend_from_slice(error(1_699_999_999_999, "appended", "grown").as_bytes());
    fs::write(logs.join("grow.log"), longer).unwrap();
    drop(release);
    let found = deadline.recv(&done_rx).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!((found.2.as_str(), found.3.as_str()), ("first", "open-time"));
    let chunks = seam::take_chunks_for("grow.log");
    let read: usize = chunks.iter().map(|(_, len)| len).sum();
    assert_eq!(read, open_length);
    assert_eq!(chunks.first().unwrap().0, 0);
}

#[test]
fn newest_error_holds_the_open_time_length_past_ten_mib() {
    let setup = Setup::new();
    // A window-sized file plus one error: the window starts past byte 0,
    // so the remaining count and the length-plus-start sum differ.
    let mut big = error(1_699_000_000_000, "open", "open-time").into_bytes();
    big.extend(std::iter::repeat_n(b'p', usize::try_from(WINDOW).unwrap()));
    big.extend_from_slice(b"\n");
    big.extend_from_slice(error(1_699_000_000_005, "late", "in window").as_bytes());
    setup.write("logs/growbig.log", &big);
    let start = big.len() as u64 - WINDOW;
    assert!(start > 0, "the window starts past byte 0");
    let (arrived, release, _pause) = seam::arm_before_chunk("growbig.log", start);
    drop(seam::take_chunks_for("growbig.log"));
    let logs = setup.logs();
    let (done_tx, done_rx) = mpsc::channel();
    let deadline = fakes::Deadline::after(HANG);
    let worker = std::thread::spawn({
        let worker_logs = logs.clone();
        move || drop(done_tx.send(newest_error(&worker_logs)))
    });
    deadline.recv(&arrived).unwrap();
    // Grown past its open-time length while the scan waits: the appended
    // newer error is never read, so the open-time one still wins.
    let mut longer = big.clone();
    longer.extend_from_slice(error(1_699_999_999_999, "appended", "grown").as_bytes());
    fs::write(logs.join("growbig.log"), longer).unwrap();
    drop(release);
    let found = deadline.recv(&done_rx).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!((found.2.as_str(), found.3.as_str()), ("late", "in window"));
    let chunks = seam::take_chunks_for("growbig.log");
    let read: usize = chunks.iter().map(|(_, len)| len).sum();
    assert_eq!(
        u64::try_from(read).unwrap(),
        WINDOW,
        "never past the open-time length"
    );
    assert!(
        chunks.iter().all(|(_, len)| *len == CHUNK),
        "one 64 KiB read at a time: {chunks:?}"
    );
}

#[test]
fn newest_error_stops_when_the_file_shrinks_under_the_scan() {
    let setup = Setup::new();
    // A long line: truncating to 10 bytes leaves a short first read.
    let old = error(1_699_000_000_000, "old", &"o".repeat(200));
    setup.write("logs/shrink.log", old.as_bytes());
    let (arrived_first, release_first, _pause_first) = seam::arm_before_chunk("shrink.log", 0);
    // Armed but never fired: a short read ends the file, so no second
    // chunk follows.
    let (arrived_second, release_second, _pause_second) = seam::arm_before_chunk("shrink.log", 10);
    drop(seam::take_chunks_for("shrink.log"));
    let logs = setup.logs();
    let (done_tx, done_rx) = mpsc::channel();
    let deadline = fakes::Deadline::after(HANG);
    let worker = std::thread::spawn({
        let worker_logs = logs.clone();
        move || drop(done_tx.send(newest_error(&worker_logs)))
    });
    deadline.recv(&arrived_first).unwrap();
    // Truncated to a fragment while the scan waits: the first read is
    // short, which ends the file for the scan.
    fs::write(logs.join("shrink.log"), &old.as_bytes()[..10]).unwrap();
    drop(release_first);
    // No second chunk follows a short read: a two-second absence proves
    // the scan stopped instead of reading on.
    let stopped = fakes::Deadline::after(ABSENT)
        .recv(&arrived_second)
        .is_err();
    drop(release_second);
    let found = deadline.recv(&done_rx).unwrap();
    worker.join().unwrap();
    assert!(stopped, "a short read ends the file");
    assert_eq!(found, None);
    assert_eq!(seam::take_chunks_for("shrink.log"), [(0, 10)]);
}

#[test]
fn newest_error_cuts_an_over_long_line_for_display_only() {
    let setup = Setup::new();
    // Exactly 64 KiB of `code: message` prints whole.
    setup.write(
        "logs/exact.log",
        format!(
            "{{\"ts\":1699000000001,\"level\":\"error\",\"code\":\"e\",\"message\":\"{}\"}}\n",
            "m".repeat(64 * 1024 - 3)
        )
        .as_bytes(),
    );
    let found = newest_error(&setup.logs()).unwrap();
    assert_eq!(found.0, "exact.log");
    assert_eq!(found.2.len() + 2 + found.3.len(), 64 * 1024);
    assert_eq!(found.3.len(), 64 * 1024 - 3);
    // An over-long line with the newest `ts` wins, cut at a char boundary;
    // the same line with an older `ts` loses to the newer short one. The
    // five-byte code leaves an odd cut, which would split the last `é`.
    let long = "é".repeat(40 * 1024);
    setup.write(
        "logs/long-new.log",
        format!(
            "{{\"ts\":1699000000002,\"level\":\"error\",\"code\":\"long!\",\"message\":\"{long}\"}}\n"
        )
        .as_bytes(),
    );
    setup.write(
        "logs/long-old.log",
        format!(
            "{{\"ts\":1699000000000,\"level\":\"error\",\"code\":\"long!\",\"message\":\"{long}\"}}\n"
        )
        .as_bytes(),
    );
    let found = newest_error(&setup.logs()).unwrap();
    assert_eq!(found.0, "long-new.log");
    assert_eq!(found.2.len() + 2 + found.3.len(), 64 * 1024 - 1);
    assert!(found.3.ends_with('é'), "cut at a char boundary");
    fs::remove_file(setup.logs().join("long-new.log")).unwrap();
    let found = newest_error(&setup.logs()).unwrap();
    assert_eq!(found.0, "exact.log");
}

#[test]
fn lines_after_drops_the_straddling_line_only_past_byte_zero() {
    let bytes = b"head\nkept\ntail";
    let all: Vec<&[u8]> = lines_after(bytes, 0).collect();
    assert_eq!(all, [&b"head"[..], &b"kept"[..], &b"tail"[..]]);
    // Past byte 0 the first piece began before the window and is dropped.
    let windowed: Vec<&[u8]> = lines_after(bytes, 1).collect();
    assert_eq!(windowed, [&b"kept"[..], &b"tail"[..]]);
    let windowed: Vec<&[u8]> = lines_after(b"head\nkept\n", 5).collect();
    assert_eq!(windowed, [&b"kept"[..], &b""[..]]);
}

#[test]
fn cut_display_holds_exactly_64_kib_and_cuts_each_side_of_it() {
    const CAP: usize = 64 * 1024;
    // Exactly 64 KiB of `code: message` prints whole.
    let (code, message) = cut_display("e", &"m".repeat(CAP - 3));
    assert_eq!(code, "e");
    assert_eq!(message.len(), CAP - 3);
    // One byte over with an empty code cuts the message to CAP - 2.
    let (code, message) = cut_display("", &"m".repeat(CAP - 1));
    assert_eq!(code, "");
    assert_eq!(message.len(), CAP - 2);
    // One byte over with a one-byte message cuts the message whole.
    let (code, message) = cut_display(&"c".repeat(CAP - 2), "x");
    assert_eq!(code.len(), CAP - 2);
    assert!(message.is_empty(), "cut to nothing: {message:?}");
    // One byte under on each side prints whole.
    let (_, message) = cut_display("", &"m".repeat(CAP - 2));
    assert_eq!(message.len(), CAP - 2);
    let (code, message) = cut_display(&"c".repeat(CAP - 3), "x");
    assert_eq!(code.len(), CAP - 3);
    assert_eq!(message, "x");
}

#[test]
fn cut_display_stops_at_a_char_boundary_within_a_bound() {
    // The over-long cut splits the last `é`: the boundary walk steps back
    // one byte at a time and must finish. A worker with a wall guard
    // proves the bound: a hang fails the test instead of the suite.
    let long = "é".repeat(40 * 1024);
    let (done_tx, done_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        done_tx.send(cut_display("long!", &long)).unwrap_or(());
    });
    let (code, message) = fakes::Deadline::after(HANG)
        .recv(&done_rx)
        .expect("cut_display returns within its bound");
    worker.join().unwrap();
    assert_eq!(code, "long!");
    assert!(message.ends_with('é'), "cut at a char boundary");
    assert_eq!(code.len() + 2 + message.len(), 64 * 1024 - 1);
}

#[test]
fn newest_error_takes_a_final_line_without_a_newline() {
    let setup = Setup::new();
    let mut line = error(1_699_000_000_007, "last", "no newline").into_bytes();
    line.pop();
    setup.write("logs/tail.log", &line);
    let found = newest_error(&setup.logs()).unwrap();
    assert_eq!((found.2.as_str(), found.3.as_str()), ("last", "no newline"));
}

#[test]
fn newest_error_skips_a_fifo_swapped_in_after_the_listing() {
    let setup = Setup::new();
    setup.write(
        "logs/real.log",
        error(1_699_000_000_000, "real", "stays").as_bytes(),
    );
    setup.write(
        "logs/victim-fifo.log",
        error(1_699_000_000_001, "old", "replaced").as_bytes(),
    );
    let (arrived, release, _pause) = seam::arm_before_file("victim-fifo.log");
    drop(seam::take_chunks_for("victim-fifo.log"));
    let logs = setup.logs();
    let (done_tx, done_rx) = mpsc::channel();
    let deadline = fakes::Deadline::after(HANG);
    let worker = std::thread::spawn({
        let worker_logs = logs.clone();
        move || done_tx.send(newest_error(&worker_logs)).unwrap()
    });
    deadline.recv(&arrived).unwrap();
    fs::remove_file(logs.join("victim-fifo.log")).unwrap();
    fakes::fifo(logs.join("victim-fifo.log"));
    drop(release);
    let found = deadline.recv(&done_rx).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!((found.2.as_str(), found.3.as_str()), ("real", "stays"));
    assert!(
        seam::take_chunks_for("victim-fifo.log").is_empty(),
        "a rejected descriptor is never read"
    );
}

#[test]
fn newest_error_skips_a_link_swapped_in_after_the_listing() {
    let setup = Setup::new();
    setup.write(
        "logs/real.log",
        error(1_699_000_000_000, "real", "stays").as_bytes(),
    );
    setup.write(
        "logs/victim-link.log",
        error(1_699_000_000_001, "old", "replaced").as_bytes(),
    );
    setup.write(
        "outside.log",
        error(1_699_999_999_999, "link", "linked").as_bytes(),
    );
    let (arrived, release, _pause) = seam::arm_before_file("victim-link.log");
    drop(seam::take_chunks_for("victim-link.log"));
    let logs = setup.logs();
    let (done_tx, done_rx) = mpsc::channel();
    let deadline = fakes::Deadline::after(HANG);
    let outside = setup.home().join("outside.log");
    let worker = std::thread::spawn({
        let worker_logs = logs.clone();
        move || done_tx.send(newest_error(&worker_logs)).unwrap()
    });
    deadline.recv(&arrived).unwrap();
    fs::remove_file(logs.join("victim-link.log")).unwrap();
    symlink(outside, logs.join("victim-link.log")).unwrap();
    drop(release);
    let found = deadline.recv(&done_rx).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!((found.2.as_str(), found.3.as_str()), ("real", "stays"));
    assert!(
        seam::take_chunks_for("victim-link.log").is_empty(),
        "a rejected descriptor is never read"
    );
}

#[test]
fn newest_error_skips_a_directory_swapped_in_after_the_listing() {
    let setup = Setup::new();
    setup.write(
        "logs/real.log",
        error(1_699_000_000_000, "real", "stays").as_bytes(),
    );
    setup.write(
        "logs/victim-dir.log",
        error(1_699_000_000_001, "old", "replaced").as_bytes(),
    );
    let (arrived, release, _pause) = seam::arm_before_file("victim-dir.log");
    drop(seam::take_chunks_for("victim-dir.log"));
    let logs = setup.logs();
    let (done_tx, done_rx) = mpsc::channel();
    let deadline = fakes::Deadline::after(HANG);
    let worker = std::thread::spawn({
        let worker_logs = logs.clone();
        move || done_tx.send(newest_error(&worker_logs)).unwrap()
    });
    deadline.recv(&arrived).unwrap();
    fs::remove_file(logs.join("victim-dir.log")).unwrap();
    fs::create_dir(logs.join("victim-dir.log")).unwrap();
    drop(release);
    let found = deadline.recv(&done_rx).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!((found.2.as_str(), found.3.as_str()), ("real", "stays"));
    assert!(
        seam::take_chunks_for("victim-dir.log").is_empty(),
        "a rejected descriptor is never read"
    );
}

#[test]
fn newest_error_never_opens_a_fifo_at_listing_time() {
    let setup = Setup::new();
    fakes::fifo(setup.logs().join("pipe.log"));
    setup.write(
        "logs/real.log",
        error(1_699_000_000_001, "e", "m").as_bytes(),
    );
    drop(seam::take_chunks_for("pipe.log"));
    drop(seam::take_chunks_for("real.log"));
    drop(seam::take_opens());
    let found = newest_error(&setup.logs()).unwrap();
    assert_eq!(found.0, "real.log");
    // The listing filtered the FIFO out, so it was never opened; the
    // real file was.
    let opens = seam::take_opens();
    assert!(
        !opens.iter().any(|name| name.as_os_str() == "pipe.log"),
        "{opens:?}"
    );
    assert!(
        opens.iter().any(|name| name.as_os_str() == "real.log"),
        "{opens:?}"
    );
    // The real file was read; the FIFO has no chunk: the read observer
    // stays uncalled for the rejected descriptor.
    let real = seam::take_chunks_for("real.log");
    assert!(!real.is_empty(), "the real file was read");
    assert!(
        seam::take_chunks_for("pipe.log").is_empty(),
        "the FIFO was never read"
    );
}

#[test]
fn newest_error_ignores_a_linked_logs_directory() {
    let setup = Setup::new();
    setup.write(
        "credentials/fake/default",
        error(1_699_999_999_999, "sentinel", "must never print").as_bytes(),
    );
    let (arrived, release, _pause) = seam::arm_before_dir(&setup.logs());
    let logs = setup.logs();
    let (done_tx, done_rx) = mpsc::channel();
    let deadline = fakes::Deadline::after(HANG);
    let worker = std::thread::spawn({
        let worker_logs = logs.clone();
        move || done_tx.send(newest_error(&worker_logs)).unwrap()
    });
    deadline.recv(&arrived).unwrap();
    fs::remove_dir_all(&logs).unwrap();
    symlink(setup.home().join("credentials/fake"), &logs).unwrap();
    drop(release);
    let found = deadline.recv(&done_rx).unwrap();
    worker.join().unwrap();
    assert_eq!(found, None);
}

#[test]
fn newest_error_ignores_a_logs_link_from_the_start() {
    let setup = Setup::new();
    setup.write(
        "credentials/fake/default",
        error(1_699_999_999_999, "sentinel", "must never print").as_bytes(),
    );
    fs::remove_dir_all(setup.logs()).unwrap();
    symlink(setup.home().join("credentials/fake"), setup.logs()).unwrap();
    assert_eq!(newest_error(&setup.logs()), None);
}

#[test]
fn newest_error_reads_the_original_directory_after_a_rename() {
    let setup = Setup::new();
    setup.write(
        "logs/real.log",
        error(1_699_000_000_000, "real", "stays").as_bytes(),
    );
    setup.write(
        "credentials/fake/default",
        error(1_699_999_999_999, "sentinel", "must never print").as_bytes(),
    );
    let (arrived, release, _pause) = seam::arm_after_dir(&setup.logs());
    let logs = setup.logs();
    let (done_tx, done_rx) = mpsc::channel();
    let deadline = fakes::Deadline::after(HANG);
    let worker = std::thread::spawn({
        let worker_logs = logs.clone();
        move || done_tx.send(newest_error(&worker_logs)).unwrap()
    });
    deadline.recv(&arrived).unwrap();
    fs::rename(&logs, logs.with_extension("moved")).unwrap();
    symlink(setup.home().join("credentials/fake"), &logs).unwrap();
    drop(release);
    let found = deadline.recv(&done_rx).unwrap().unwrap();
    worker.join().unwrap();
    assert_eq!((found.2.as_str(), found.3.as_str()), ("real", "stays"));
}

#[test]
fn newest_crash_picks_the_latest_mtime() {
    let setup = Setup::new();
    let old = setup.crashes().join("s_old-1699990000000.txt");
    let new = setup.crashes().join("s_new-1699990000000.txt");
    fs::write(&old, "old").unwrap();
    fs::write(&new, "new").unwrap();
    File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_990_000))
        .unwrap();
    File::options()
        .write(true)
        .open(&new)
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_998_000))
        .unwrap();
    fs::create_dir(setup.crashes().join("dir")).unwrap();
    symlink(&new, setup.crashes().join("link")).unwrap();
    let (path, mtime) = newest_crash(&setup.crashes()).unwrap();
    assert_eq!(path, new);
    assert_eq!(
        mtime,
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_998_000)
    );
}

#[test]
fn newest_crash_breaks_mtime_ties_by_name() {
    let setup = Setup::new();
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_699_990_000);
    for name in ["s_b-1.txt", "s_a-1.txt"] {
        let path = setup.crashes().join(name);
        fs::write(&path, "x").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }
    let (path, _) = newest_crash(&setup.crashes()).unwrap();
    assert_eq!(path, setup.crashes().join("s_b-1.txt"));
}

#[test]
fn newest_crash_compares_nanoseconds_within_one_second() {
    // Same second, different nanoseconds: the earlier name is newer,
    // so name order must not decide.
    let setup = Setup::new();
    let older = setup.crashes().join("s_b-1.txt");
    let newer = setup.crashes().join("s_a-1.txt");
    for (path, nanos) in [(&older, 100), (&newer, 900)] {
        fs::write(path, "x").unwrap();
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::new(1_699_990_000, nanos))
            .unwrap();
    }
    let (path, mtime) = newest_crash(&setup.crashes()).unwrap();
    assert_eq!(path, newer);
    assert_eq!(
        mtime,
        SystemTime::UNIX_EPOCH + Duration::new(1_699_990_000, 900)
    );
}

#[test]
fn newest_crash_is_none_when_missing_empty_or_linked() {
    let setup = Setup::new();
    assert_eq!(newest_crash(&setup.crashes()), None);
    fs::remove_dir_all(setup.crashes()).unwrap();
    assert_eq!(newest_crash(&setup.crashes()), None);
    fs::create_dir_all(setup.crashes()).unwrap();
    fs::create_dir(setup.home().join("elsewhere")).unwrap();
    fs::write(setup.home().join("elsewhere/s_x-1.txt"), "x").unwrap();
    fs::remove_dir_all(setup.crashes()).unwrap();
    symlink(setup.home().join("elsewhere"), setup.crashes()).unwrap();
    assert_eq!(newest_crash(&setup.crashes()), None);
}
