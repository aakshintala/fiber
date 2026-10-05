//! Receipt text, the end mapping, and `ps` rows.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use contract::clock::{Clock, Wake};
use contract::events::{JobStarted, Outcome};
use contract::jobs::Stop;
use contract::shapes::ContentPart;
use contract::tool::Cancel;
use contract::{ErrorCode, JobId};
use fakes::clock::FakeClock;

use super::super::command::{Finished, MoveReason, StopKind};
use super::{
    JobCancel, PS_BOUND, align_char_boundary, description_of, format_rows, group_members,
    output_tail, parse_ps, receipt, shell_sentence, to_completed,
};

fn finished(
    status: Option<std::process::ExitStatus>,
    stop: Option<StopKind>,
    indeterminate: bool,
    sent_signal: bool,
    held_open: bool,
) -> Finished {
    Finished {
        output: Vec::new(),
        status,
        stop,
        indeterminate,
        held_open,
        sent_signal,
    }
}

fn status_of(command: &str) -> std::process::ExitStatus {
    Command::new("sh").args(["-c", command]).status().unwrap()
}

#[test]
fn the_description_is_the_first_line_cut_to_eighty_characters() {
    assert_eq!(description_of("echo hi"), "echo hi");
    assert_eq!(description_of("echo hi\nsleep 30"), "echo hi");
    assert_eq!(description_of(""), "");
    let eighty = "a".repeat(80);
    assert_eq!(description_of(&eighty), eighty);
    let eighty_one = "a".repeat(81);
    let cut = description_of(&eighty_one);
    assert_eq!(cut.chars().count(), 80);
    assert!(cut.ends_with('\u{2026}'));
    assert_eq!(cut, format!("{}\u{2026}", "a".repeat(79)));
    // 79 ascii characters plus é is 80 characters and 81 bytes.
    let whole = format!("{}é", "a".repeat(79));
    assert_eq!(whole.chars().count(), 80);
    assert!(whole.len() > 80);
    assert_eq!(description_of(&whole), whole);
    let split = format!("{}éZ", "a".repeat(79));
    let cut = description_of(&split);
    assert_eq!(cut.chars().count(), 80);
    assert!(cut.ends_with('\u{2026}'));
    assert!(!cut.contains('é'));
}

#[test]
fn the_receipt_names_the_reason_the_job_and_the_file() {
    let started = JobStarted {
        job_id: JobId("j_1".into()),
        tool: Some("shell".into()),
        extension: None,
        description: "echo hi".into(),
        output_path: "j_1.log".into(),
    };
    let path = Path::new("/tmp/j_1.log");
    let output = receipt(&started, path, &MoveReason::StartedInBackground, None);
    assert!(output.error.is_none());
    assert!(output.process.is_none());
    assert_eq!(
        output.jobs,
        vec![contract::jobs::JobRecord::Started(started.clone())]
    );
    let ContentPart::Text { text } = &output.content[0] else {
        panic!("receipt text");
    };
    assert_eq!(
        text,
        "Started in the background.\nJob j_1. Output: /tmp/j_1.log. Read it with `read`; `jobs wait` waits for it.\n"
    );
    let later = receipt(&started, path, &MoveReason::AfterThirtySeconds, None);
    let ContentPart::Text { text } = &later.content[0] else {
        panic!("receipt text");
    };
    assert_eq!(
        text,
        "Still running after 30 seconds, so it moved to the background.\nJob j_1. Output: /tmp/j_1.log. Read it with `read`; `jobs wait` waits for it.\n"
    );
}

#[test]
fn ps_rows_keep_the_group_and_a_name_with_spaces() {
    let text = "\
  12  99 sleep\n\
\n\
PID PGID COMMAND\n\
  34  99 my cmd\n\
  50   7 other\n\
  60  99\n\
not a row\n";
    assert_eq!(
        parse_ps(text, 99),
        vec![(12, "sleep".to_owned()), (34, "my cmd".to_owned())]
    );
    assert!(parse_ps(text, 8).is_empty());
    assert_eq!(
        format_rows(&[(12, "sleep".to_owned()), (34, "my cmd".to_owned())]),
        "sleep (12), my cmd (34)"
    );
}

#[test]
fn a_shell_exit_names_members_or_says_the_group_is_still_occupied() {
    let named = shell_sentence(3, Some("sleep (12), nap (34)"));
    assert_eq!(
        named,
        "The shell exited with code 3, leaving sleep (12), nap (34) running, so it moved to the background."
    );
    let unnamed = shell_sentence(3, None);
    assert_eq!(
        unnamed,
        "The shell exited with code 3, so it moved to the background. Processes are still running in its group."
    );
    assert!(!unnamed.contains("leaving"), "{unnamed}");
}

const DEADLINE: Duration = Duration::from_secs(10);

/// A `ps` that prints `rows` and exits, or never exits when `rows` is `None`.
fn fake_ps(dir: &Path, rows: Option<&str>) -> std::path::PathBuf {
    let path = dir.join("ps");
    let body = match rows {
        Some(rows) => format!("#!/bin/sh\nprintf '{rows}'\n"),
        None => "#!/bin/sh\nexec sleep 1000\n".to_owned(),
    };
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Lists group `pgid` with `ps` on a thread of its own, so a listing that
/// never returns fails the test at its deadline.
fn list(ps: &Path, pgid: u32, clock: &Arc<FakeClock>) -> mpsc::Receiver<Option<String>> {
    let ps = ps.to_path_buf();
    let clock = Arc::clone(clock);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(group_members(&ps, pgid, clock.as_ref())).unwrap();
    });
    rx
}

#[test]
fn the_group_is_listed_from_ps() {
    let dir = fakes::TempDir::new("fiber-shell-ps");
    let clock = FakeClock::new();
    let ps = fake_ps(
        dir.path(),
        Some("  12  99 sleep\\n  50   7 other\\n  34  99 my cmd\\n"),
    );
    let listed = |pgid| list(&ps, pgid, &clock).recv_timeout(DEADLINE).unwrap();
    assert_eq!(listed(99).as_deref(), Some("sleep (12), my cmd (34)"));
    assert_eq!(listed(8), None);
    let missing = list(&dir.path().join("missing"), 99, &clock);
    assert_eq!(missing.recv_timeout(DEADLINE).unwrap(), None);
}

#[test]
fn a_ps_that_does_not_finish_is_killed_at_the_bound() {
    let dir = fakes::TempDir::new("fiber-shell-ps-hangs");
    let clock = FakeClock::new();
    let ps = fake_ps(dir.path(), None);
    let bound = clock.now() + PS_BOUND;
    let rx = list(&ps, 99, &clock);
    assert!(
        clock.await_parked(bound, DEADLINE),
        "the listing did not park at its bound"
    );
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the listing returned before its bound"
    );
    clock.advance(PS_BOUND);
    assert_eq!(
        rx.recv_timeout(DEADLINE).expect("the bound to end it"),
        None
    );
}

#[test]
fn a_cut_inside_a_character_starts_at_the_next_one() {
    assert_eq!(align_char_boundary(&[0xA9, b'y']), b"y");
    assert_eq!(align_char_boundary(&[0x9F, 0x98, 0x80, b'y']), b"y");
    assert_eq!(align_char_boundary("éy".as_bytes()), "éy".as_bytes());
    assert_eq!(align_char_boundary(&[0xA9, 0xA9]), b"");
    assert_eq!(align_char_boundary(b""), b"");
}

#[test]
fn output_tail_starts_on_a_character_boundary() {
    let dir = fakes::TempDir::new("fiber-shell-tail");
    let missing = dir.path().join("missing");
    assert!(output_tail(&missing).is_none());
    let empty = dir.path().join("empty");
    std::fs::write(&empty, "").unwrap();
    assert!(output_tail(&empty).is_none());
    let short = dir.path().join("short");
    std::fs::write(&short, "hi").unwrap();
    assert_eq!(output_tail(&short).as_deref(), Some("hi"));

    // The 2048-byte window starts on the second byte of é.
    let mut bytes = vec![b'x'; 10];
    bytes.extend([0xC3, 0xA9]);
    bytes.extend(std::iter::repeat_n(b'y', 2047));
    assert_eq!(bytes.len() - 2048, 11);
    assert_eq!(bytes[11], 0xA9);
    let cut = dir.path().join("cut");
    std::fs::write(&cut, &bytes).unwrap();
    let tail = output_tail(&cut).unwrap();
    assert!(tail.starts_with('y'), "{tail:?}");
    assert!(!tail.starts_with('\u{FFFD}'), "{tail:?}");
    assert!(!tail.contains('é'), "{tail:?}");
    assert!(!tail.contains('x'), "{tail:?}");
    assert_eq!(tail.chars().count(), 2047);

    let mut exact = vec![b'x'; 10];
    exact.push(b'Z');
    exact.extend(std::iter::repeat_n(b'y', 2047));
    assert_eq!(exact.len() - 2048, 10);
    assert_eq!(exact[10], b'Z');
    let marked = dir.path().join("marked");
    std::fs::write(&marked, &exact).unwrap();
    let marked_tail = output_tail(&marked).unwrap();
    assert!(marked_tail.starts_with('Z'), "{marked_tail:?}");
    assert_eq!(marked_tail.chars().count(), 2048);
    assert!(!marked_tail.contains('x'), "{marked_tail:?}");

    let partial = dir.path().join("partial-char");
    std::fs::write(&partial, [0xA9]).unwrap();
    assert!(output_tail(&partial).is_none());
}

#[test]
fn the_end_maps_like_a_foreground_result_and_tails_only_failures() {
    let dir = fakes::TempDir::new("fiber-shell-end");
    let path = dir.path().join("out");
    std::fs::write(&path, "partial\n").unwrap();
    let id = JobId("j_1".into());

    let end =
        |finished: Finished, timeout_ms| to_completed(id.clone(), &path, finished, timeout_ms);

    let unreaped = end(finished(None, None, false, false, false), 1);
    assert_eq!(unreaped.status, Outcome::Failed);
    assert_eq!(unreaped.error.unwrap().code, ErrorCode::ToolError);
    assert!(unreaped.process.is_none());
    assert_eq!(unreaped.output_tail.as_deref(), Some("partial\n"));

    let ok = end(
        finished(Some(status_of("exit 0")), None, false, false, true),
        1,
    );
    assert_eq!(ok.status, Outcome::Completed);
    assert!(ok.error.is_none());
    let process = ok.process.unwrap();
    assert_eq!(process.exit_code, Some(0));
    assert!(!process.timed_out);
    assert!(ok.output_tail.is_none());

    let nonzero = end(
        finished(Some(status_of("exit 3")), None, false, false, true),
        1,
    );
    assert_eq!(nonzero.status, Outcome::Failed);
    assert_eq!(nonzero.error.unwrap().code, ErrorCode::NonzeroExit);
    assert_eq!(nonzero.process.unwrap().exit_code, Some(3));

    let signaled = end(
        finished(Some(status_of("kill -SEGV $$")), None, false, false, false),
        1,
    );
    assert_eq!(signaled.status, Outcome::Failed);
    assert_eq!(signaled.error.unwrap().code, ErrorCode::Signal);
    assert_eq!(signaled.process.unwrap().signal.as_deref(), Some("SIGSEGV"));

    let timeout = end(
        finished(None, Some(StopKind::Timeout), false, true, false),
        5_000,
    );
    assert_eq!(timeout.status, Outcome::Failed);
    let error = timeout.error.unwrap();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(error.message.contains("5000"));
    assert!(timeout.process.unwrap().timed_out);
    assert_eq!(timeout.output_tail.as_deref(), Some("partial\n"));

    let stopped = end(
        finished(
            Some(status_of("exit 0")),
            Some(StopKind::Cancel),
            false,
            true,
            false,
        ),
        1,
    );
    assert_eq!(stopped.status, Outcome::Cancelled);
    assert!(stopped.error.is_none());
    assert!(stopped.output_tail.is_none());

    let unknown = end(finished(None, Some(StopKind::Cancel), true, true, false), 1);
    assert_eq!(unknown.status, Outcome::Failed);
    assert_eq!(unknown.error.unwrap().code, ErrorCode::Indeterminate);
    assert!(!unknown.process.unwrap().timed_out);

    let ours = end(
        finished(Some(status_of("kill -TERM $$")), None, false, true, false),
        1,
    );
    assert_eq!(ours.status, Outcome::Completed);
    assert!(ours.error.is_none());

    let timed_unknown = end(
        finished(None, Some(StopKind::Timeout), true, true, false),
        1,
    );
    assert_eq!(timed_unknown.status, Outcome::Failed);
    assert_eq!(timed_unknown.error.unwrap().code, ErrorCode::Indeterminate);
    assert!(timed_unknown.process.unwrap().timed_out);
}

#[test]
fn the_jobs_stop_cancels_once_and_wakes_subscribers() {
    let cancel = JobCancel::new();
    assert!(!cancel.is_cancelled());
    let hits = Arc::new(Hits(AtomicUsize::new(0)));
    let wake: Arc<dyn Wake> = hits.clone();
    cancel.subscribe(Arc::downgrade(&wake));
    let stop: Stop = cancel.stop();
    (stop.0)();
    assert!(cancel.is_cancelled());
    assert_eq!(hits.0.load(Ordering::SeqCst), 1);
    (stop.0)();
    assert_eq!(hits.0.load(Ordering::SeqCst), 1);
    let late = Arc::new(Hits(AtomicUsize::new(0)));
    let late_wake: Arc<dyn Wake> = late.clone();
    cancel.subscribe(Arc::downgrade(&late_wake));
    assert_eq!(late.0.load(Ordering::SeqCst), 0);
    assert!(Cancel::is_cancelled(cancel.as_ref()));
}

struct Hits(AtomicUsize);

impl Wake for Hits {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
