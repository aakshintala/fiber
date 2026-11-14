//! Receipt text, the end mapping, and `ps` rows.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, SystemTime};

use contract::clock::{Clock, Wake};
use contract::events::{JobStarted, Outcome};
use contract::jobs::{Jobs as _, Stop};
use contract::shapes::ContentPart;
use contract::tool::Cancel;
use contract::{ErrorCode, JobId};
use fakes::Deadline;
use fakes::children::Ready;
use fakes::clock::FakeClock;
use fakes::kill_pid;
use fakes::{CancelToken, Recorder};
use support::cancel::Token;

use super::super::Limit;
use super::super::command::{Finished, Kept, MoveReason, StopKind};
use super::super::drive::{Phase, Run};
use super::super::moved::Moved;
use super::super::output::{OUTPUT_CAP, Shared, align_char_boundary, lock, note_eof};
use super::{
    GroupMembers, MoveAsk, PS_BOUND, deadline_note, description_of, format_rows, group_members,
    leftover_line, output_tail, output_tail_from, parse_ps, receipt, shell_sentence, stop_of, take,
    terminal_receipt, to_completed, with_deadline, with_spill_line,
};

#[cfg(unix)]
#[test]
fn detached_reader_rejects_a_regular_file_with_a_different_opened_identity() {
    let dir = fakes::TempDir::new("fiber-detached-open-identity");
    let admitted = dir.path().join("admitted.log");
    let substituted = dir.path().join("substituted.log");
    std::fs::write(&admitted, b"first").unwrap();
    std::fs::write(&substituted, b"second").unwrap();
    let before_open = std::fs::symlink_metadata(&admitted).unwrap();
    let after_open = std::fs::metadata(&substituted).unwrap();
    assert!(after_open.file_type().is_file());
    assert!(
        super::opened_file_is_rejected(&before_open, &after_open),
        "a regular replacement with a different inode is rejected"
    );
}

#[cfg(unix)]
#[test]
fn detached_output_tail_rejects_nonregular_links_and_paths_outside_root() {
    let dir = fakes::TempDir::new("fiber-detached-output-tail-guard");
    let root = dir.path().join("detached");
    std::fs::create_dir(&root).unwrap();
    let regular = root.join("regular.log");
    std::fs::write(&regular, b"safe").unwrap();
    assert_eq!(
        output_tail_from(&regular, Some(&root)).as_deref(),
        Some("safe")
    );

    let fifo = root.join("fifo.log");
    fakes::fifo(&fifo);
    assert!(output_tail_from(&fifo, Some(&root)).is_none());

    let external = dir.path().join("external.log");
    std::fs::write(&external, b"outside").unwrap();
    assert!(output_tail_from(&external, Some(&root)).is_none());
    let nested = root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    let nested_file = nested.join("file.log");
    std::fs::write(&nested_file, b"nested").unwrap();
    assert!(output_tail_from(&nested_file, Some(&root)).is_none());

    #[cfg(target_os = "linux")]
    {
        let link = root.join("link.log");
        std::os::unix::fs::symlink(&external, &link).unwrap();
        assert!(output_tail_from(&link, Some(&root)).is_none());
        assert_eq!(std::fs::read(&external).unwrap(), b"outside");
    }
}

fn finished(
    status: Option<std::process::ExitStatus>,
    stop: Option<StopKind>,
    indeterminate: bool,
    sent_signal: bool,
    held_open: bool,
) -> Finished {
    Finished {
        output: Kept::Whole(Vec::new()),
        status,
        stop,
        indeterminate,
        held_open,
        sent_signal,
        capped: false,
        flooded: false,
    }
}

fn status_of(command: &str) -> std::process::ExitStatus {
    Command::new("sh").args(["-c", command]).status().unwrap()
}

#[test]
fn a_job_completion_keeps_changed_path_reports_and_omits_empty_ones() {
    use std::os::unix::process::ExitStatusExt as _;

    use contract::shapes::{ProtectedPathFailure, ProtectedPaths};

    for (report, expected) in [
        (None, None),
        (
            Some(ProtectedPaths {
                restored: Vec::new(),
                failed: Vec::new(),
            }),
            None,
        ),
        (
            Some(ProtectedPaths {
                restored: vec![".fiber".to_owned()],
                failed: Vec::new(),
            }),
            Some(serde_json::json!({"restored": [".fiber"], "failed": []})),
        ),
        (
            Some(ProtectedPaths {
                restored: Vec::new(),
                failed: vec![ProtectedPathFailure {
                    path: ".git/config".to_owned(),
                    message: "Permission denied".to_owned(),
                }],
            }),
            Some(serde_json::json!({
                "restored": [],
                "failed": [{"path": ".git/config", "message": "Permission denied"}],
            })),
        ),
    ] {
        let end = super::to_completed_reported(
            JobId("j_protected".to_owned()),
            Path::new("unused-success-output"),
            finished(
                Some(std::process::ExitStatus::from_raw(0)),
                None,
                false,
                false,
                false,
            ),
            Some(Limit {
                ms: 60_000,
                monitor: false,
                // No test command at this site holds a `|`.
                has_pipe: false,
            }),
            report,
        );
        assert_eq!(end.status, Outcome::Completed);
        assert_eq!(end.process.as_ref().and_then(|p| p.exit_code), Some(0));
        assert!(end.error.is_none());
        let event = serde_json::to_value(end).expect("completion serializes");
        assert_eq!(event.get("protected_paths"), expected.as_ref());
    }
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

        pgid: None,
        start_time: None,
        kind: None,
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
        ["sleep (12)", "my cmd (34)"]
    );
}

fn rows(count: u32) -> GroupMembers {
    let rows: Vec<(u32, String)> = (1..=count).map(|pid| (pid, "sleep".to_owned())).collect();
    GroupMembers::Members(format_rows(&rows))
}

const SAVED: &str = "Output written after the call returns is in /a/s_1.txt.";

#[test]
fn leftover_line_counts_rows_not_a_delimiter_inside_a_name() {
    let members = GroupMembers::Members(format_rows(&[(7, "nap), extra".to_owned())]));
    let line = leftover_line(Some(&members), SAVED);
    assert!(
        line.starts_with("1 process left running (first: nap), extra (7))."),
        "{line}"
    );
}

#[test]
fn leftover_line_for_one_process_is_singular() {
    assert_eq!(
        leftover_line(Some(&rows(1)), SAVED),
        format!(
            "1 process left running (first: sleep (1)). {SAVED} {}",
            super::super::MOVED_JOB_HINT
        )
    );
}

#[test]
fn leftover_line_names_all_of_three() {
    assert_eq!(
        leftover_line(Some(&rows(3)), SAVED),
        format!(
            "3 processes left running (first: sleep (1), sleep (2), sleep (3)). {SAVED} {}",
            super::super::MOVED_JOB_HINT
        )
    );
    assert_eq!(
        leftover_line(Some(&rows(2)), SAVED),
        format!(
            "2 processes left running (first: sleep (1), sleep (2)). {SAVED} {}",
            super::super::MOVED_JOB_HINT
        )
    );
}

#[test]
fn leftover_line_counts_ten_and_names_three() {
    assert_eq!(
        leftover_line(Some(&rows(10)), SAVED),
        format!(
            "10 processes left running (first: sleep (1), sleep (2), sleep (3)). {SAVED} {}",
            super::super::MOVED_JOB_HINT
        )
    );
    assert_eq!(
        leftover_line(Some(&rows(4)), SAVED),
        format!(
            "4 processes left running (first: sleep (1), sleep (2), sleep (3)). {SAVED} {}",
            super::super::MOVED_JOB_HINT
        )
    );
}

#[test]
fn leftover_line_for_an_unanswered_listing_gives_no_count() {
    let line = leftover_line(Some(&GroupMembers::Unavailable), SAVED);
    assert_eq!(
        line,
        format!(
            "Processes are still running in its group. {SAVED} {}",
            super::super::MOVED_JOB_HINT
        )
    );
    assert!(!line.contains("left running"), "{line}");
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

#[test]
fn members_listing_moves_at_once_and_names_the_receipt() {
    let dir = fakes::TempDir::new("fiber-shell-members-receipt");
    let (text, job) = hand_off_with_timeout(
        dir.path(),
        3,
        GroupMembers::Members(vec!["sleep (12)".to_owned(), "my cmd (34)".to_owned()]),
        None,
        Some(Limit {
            ms: 5_000,
            monitor: false,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
        false,
    );
    let path = dir.path().join("j_0000000000000001.log");
    assert_eq!(
        text,
        format!(
            "The shell exited with code 3, leaving sleep (12), my cmd (34) running, so it moved to the background.\nJob {job}. Output: {path}. Read it with `read`; `jobs wait` waits for it.\n",
            path = path.display(),
        )
    );
}

#[test]
fn unavailable_listing_moves_with_unnamed_receipt() {
    let dir = fakes::TempDir::new("fiber-shell-unavailable-receipt");
    let (text, job) = hand_off_with_timeout(
        dir.path(),
        3,
        GroupMembers::Unavailable,
        None,
        Some(Limit {
            ms: 5_000,
            monitor: false,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
        false,
    );
    let path = dir.path().join("j_0000000000000001.log");
    assert_eq!(
        text,
        format!(
            "The shell exited with code 3, so it moved to the background. Processes are still running in its group.\nJob {job}. Output: {path}. Read it with `read`; `jobs wait` waits for it.\n",
            path = path.display(),
        )
    );
}

/// Hands off a move whose saved listing names members, with the drive's
/// timeout deadline `lead` past the move when given, and `limit` as the
/// call's limit: the receipt names when the timeout fires. The job's own
/// drive is already settled, so its thread ends on its own.
fn hand_off_with_timeout(
    dir: &std::path::Path,
    code: i32,
    members: GroupMembers,
    lead: Option<Duration>,
    limit: Option<Limit>,
    terminal: bool,
) -> (String, String) {
    let clock = FakeClock::new();
    let timeout_at = lead.and_then(|lead| clock.now().checked_add(lead));
    let shared = Arc::new(Shared::default());
    note_eof(&shared);
    {
        let mut inner = lock(&shared.inner);
        inner.reaped = true;
        inner.status = Some(status_of(&format!("exit {code}")));
    }
    let moved = Moved {
        reason: MoveReason::ShellExited { code },
        bridge: None,
        progress: Run {
            phase: Phase::Draining {
                until: clock.origin(),
            },
            stop: None,
            sent_signal: false,
            seen_empty: true,
            stream_carry: Vec::new(),
            timeout_at,
            move_at: None,
            members: Some(members),
            identity: None,
            pgid: 99,
            hosted: None,
            listing: None,
            shared,
            ask: None,
            job: None,
            paced: None,
            feed: None,
        },
        cap: OUTPUT_CAP,
        spill: None,
        protection: None,
    };
    let jobs: Arc<dyn contract::jobs::Jobs> = fakes::jobs::FakeJobs::new(dir);
    let clock: Arc<dyn contract::clock::Clock> = clock;
    let output = take(
        moved,
        jobs,
        clock,
        "true",
        limit,
        terminal,
        &CancelToken::new(),
        &Recorder::default(),
        None,
        None,
    );
    let ContentPart::Text { text } = &output.content[0] else {
        panic!("receipt text");
    };
    let contract::jobs::JobRecord::Started(started) = &output.jobs[0] else {
        panic!("the receipt records the job");
    };
    (text.clone(), started.job_id.0.clone())
}

#[test]
fn moved_receipt_names_the_stop_time_and_timeout() {
    // The fake clock's wall starts at 2023-11-14T22:13:20Z, so a deadline
    // 30 s past the move stops the job at 2023-11-14T22:13:50Z.
    let dir = fakes::TempDir::new("fiber-shell-moved-receipt-deadline");
    let (text, job) = hand_off_with_timeout(
        dir.path(),
        3,
        GroupMembers::Members(vec!["sleep (12)".to_owned()]),
        Some(Duration::from_secs(30)),
        Some(Limit {
            ms: 60_000,
            monitor: false,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
        false,
    );
    let path = dir.path().join("j_0000000000000001.log");
    assert_eq!(
        text,
        format!(
            "The shell exited with code 3, leaving sleep (12) running, so it moved to the background.\nJob {job}. Output: {path}. Read it with `read`; `jobs wait` waits for it.\nFiber stops it at 2023-11-14T22:13:50Z (timeout_ms 60000). To keep a long-running process such as a server, start it with `run_in_background` and no timeout, or with `detach`.\n",
            path = path.display(),
        )
    );
}

#[test]
fn moved_timeout_error_carries_the_hint() {
    let dir = fakes::TempDir::new("fiber-shell-moved-timeout-hint");
    let path = dir.path().join("out");
    std::fs::write(&path, "partial\n").unwrap();
    let end = to_completed(
        JobId("j_1".into()),
        &path,
        finished(None, Some(StopKind::Timeout), false, true, false),
        Some(Limit {
            ms: 5_000,
            monitor: false,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
    );
    assert_eq!(end.status, Outcome::Failed);
    let error = end.error.unwrap();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert_eq!(
        error.message,
        "Timed out after 5000 ms; Fiber stopped the whole process group. To keep a long-running process such as a server, start it with `run_in_background` and no timeout, or with `detach`."
    );
}

#[test]
fn monitor_timeout_names_no_hint() {
    let dir = fakes::TempDir::new("fiber-shell-monitor-timeout-no-hint");
    let path = dir.path().join("out");
    std::fs::write(&path, "partial\n").unwrap();
    let end = to_completed(
        JobId("j_1".into()),
        &path,
        finished(None, Some(StopKind::Timeout), false, true, false),
        Some(Limit {
            ms: 5_000,
            monitor: true,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
    );
    assert_eq!(end.status, Outcome::Failed);
    let error = end.error.unwrap();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert_eq!(
        error.message,
        "The monitor's deadline of 5000 ms passed; start it again if you still need it."
    );
}

#[test]
fn deadline_note_names_the_stop_wall_and_timeout() {
    // A wall a minute past the fake clock's 2023-11-14T22:13:20Z origin.
    let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_060);
    assert_eq!(
        deadline_note(60_000, wall),
        "Fiber stops it at 2023-11-14T22:14:20Z (timeout_ms 60000). To keep a long-running process such as a server, start it with `run_in_background` and no timeout, or with `detach`."
    );
}

#[test]
fn terminal_receipt_with_a_timeout_names_the_stop_time() {
    let dir = fakes::TempDir::new("fiber-shell-terminal-receipt-deadline");
    let (text, job) = hand_off_with_timeout(
        dir.path(),
        3,
        GroupMembers::Members(vec!["sleep (12)".to_owned()]),
        Some(Duration::from_secs(30)),
        Some(Limit {
            ms: 60_000,
            monitor: false,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
        true,
    );
    let path = dir.path().join("j_0000000000000001.log");
    assert_eq!(
        text,
        format!(
            "The shell exited with code 3, leaving sleep (12) running, so it moved to the background.\nJob {job}. Output: {path}. Type into it with `jobs write`; `jobs wait` waits for it.\nFiber stops it at 2023-11-14T22:13:50Z (timeout_ms 60000). To keep a long-running process such as a server, start it with `run_in_background` and no timeout, or with `detach`.\n",
            path = path.display(),
        )
    );
}

#[test]
fn moved_receipt_with_no_timeout_names_no_deadline() {
    let dir = fakes::TempDir::new("fiber-shell-moved-receipt-no-timeout");
    // No limit: nothing to name.
    let (text, _) =
        hand_off_with_timeout(dir.path(), 3, GroupMembers::Unavailable, None, None, false);
    assert!(!text.contains("Fiber stops it at"), "{text}");
    assert!(!text.contains("timeout_ms"), "{text}");
    // A limit with no deadline in hand: still nothing to name.
    let (text, _) = hand_off_with_timeout(
        dir.path(),
        3,
        GroupMembers::Unavailable,
        None,
        Some(Limit {
            ms: 5_000,
            monitor: false,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
        false,
    );
    assert!(!text.contains("Fiber stops it at"), "{text}");
    assert!(!text.contains("timeout_ms"), "{text}");
}

const DEADLINE: Duration = Duration::from_secs(10);

/// A `ps` that runs `script` with `sh`.
fn fake_ps(dir: &Path, script: &str) -> std::path::PathBuf {
    fakes::script(dir, "ps", script)
}

/// Lists group `pgid` with `ps` on a thread of its own, so a listing that
/// never returns fails the test at its deadline.
fn list(ps: &Path, pgid: u32, clock: &Arc<FakeClock>) -> mpsc::Receiver<GroupMembers> {
    let ps = ps.to_path_buf();
    let clock = Arc::clone(clock);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        tx.send(group_members(&ps, pgid, clock.as_ref())).unwrap();
    });
    rx
}

/// Takes one `ps` listing with the deadline, failing at the caller's
/// line: `#[track_caller]` cannot cross the closure this replaces, and one
/// definition line serves three calls.
#[track_caller]
fn listed(ps: &Path, pgid: u32, clock: &Arc<FakeClock>) -> GroupMembers {
    Deadline::after(DEADLINE)
        .recv(&list(ps, pgid, clock))
        .unwrap()
}

#[test]
fn the_group_is_listed_from_ps() {
    let dir = fakes::TempDir::new("fiber-shell-ps");
    let clock = FakeClock::new();
    let rows = "printf '  12  99 sleep\\n  50   7 other\\n  34  99 my cmd\\n'";
    let ps = fake_ps(dir.path(), rows);
    // `docs/tools.md` promises the names, not their order: the fixture
    // prints in row order, so the test compares the listing as a set.
    let GroupMembers::Members(mut got) = listed(&ps, 99, &clock) else {
        panic!("the fixture group lists its members");
    };
    got.sort_unstable();
    assert_eq!(got, ["my cmd (34)", "sleep (12)"]);
    assert_eq!(listed(&ps, 8, &clock), GroupMembers::Empty);
    let missing = list(&dir.path().join("missing"), 99, &clock);
    assert_eq!(
        Deadline::after(DEADLINE).recv(&missing).unwrap(),
        GroupMembers::Unavailable
    );
    let failing = fake_ps(dir.path(), &format!("{rows}; exit 1"));
    let failed = list(&failing, 99, &clock);
    assert_eq!(
        Deadline::after(DEADLINE).recv(&failed).unwrap(),
        GroupMembers::Unavailable
    );
}

#[test]
fn a_ps_that_closes_its_output_and_hangs_is_killed_at_the_bound() {
    let dir = fakes::TempDir::new("fiber-shell-ps-closes");
    let clock = FakeClock::new();
    let ready = Ready::new(dir.path());
    let ps = fake_ps(
        dir.path(),
        &format!(
            "printf '  12  99 sleep\\n'; exec >&-; echo $$ > '{ready}'; exec sleep 1000",
            ready = ready.path().display()
        ),
    );
    let bound = clock.now() + PS_BOUND;
    let rx = list(&ps, 99, &clock);
    let pid = ready.wait(fakes::Deadline::after(DEADLINE))[0];
    assert!(
        clock.await_parked(bound, fakes::Deadline::after(DEADLINE)),
        "the listing did not park at its bound"
    );
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the listing returned before its bound"
    );
    clock.advance_when_parked(Some(bound), None, PS_BOUND, &Deadline::after(DEADLINE));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&rx)
            .expect("the bound to end it"),
        GroupMembers::Unavailable
    );
    assert!(!kill_pid(pid, "0").unwrap(), "the hung ps was not killed");
}

#[test]
fn a_ps_that_does_not_finish_is_killed_at_the_bound() {
    let dir = fakes::TempDir::new("fiber-shell-ps-hangs");
    let clock = FakeClock::new();
    let ps = fake_ps(dir.path(), "exec sleep 1000");
    let bound = clock.now() + PS_BOUND;
    let rx = list(&ps, 99, &clock);
    assert!(
        clock.await_parked(bound, fakes::Deadline::after(DEADLINE)),
        "the listing did not park at its bound"
    );
    assert!(
        matches!(rx.try_recv(), Err(mpsc::TryRecvError::Empty)),
        "the listing returned before its bound"
    );
    clock.advance_when_parked(Some(bound), None, PS_BOUND, &Deadline::after(DEADLINE));
    assert_eq!(
        Deadline::after(DEADLINE)
            .recv(&rx)
            .expect("the bound to end it"),
        GroupMembers::Unavailable
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
fn the_end_maps_like_a_foreground_result_and_tails_every_outcome() {
    let dir = fakes::TempDir::new("fiber-shell-end");
    let path = dir.path().join("out");
    std::fs::write(&path, "partial\n").unwrap();
    let id = JobId("j_1".into());

    let end = |finished: Finished, ms| {
        to_completed(
            id.clone(),
            &path,
            finished,
            Some(Limit {
                ms,
                monitor: false,
                // No test command at this site holds a `|`.
                has_pipe: false,
            }),
        )
    };

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
    assert_eq!(ok.output_tail.as_deref(), Some("partial\n"));

    let nonzero = end(
        finished(Some(status_of("exit 3")), None, false, false, true),
        1,
    );
    assert_eq!(nonzero.status, Outcome::Failed);
    assert_eq!(nonzero.error.unwrap().code, ErrorCode::NonzeroExit);
    assert_eq!(nonzero.process.unwrap().exit_code, Some(3));

    let signaled = end(
        finished(Some(status_of("kill -USR1 $$")), None, false, false, false),
        1,
    );
    assert_eq!(signaled.status, Outcome::Failed);
    assert_eq!(signaled.error.unwrap().code, ErrorCode::Signal);
    assert_eq!(signaled.process.unwrap().signal.as_deref(), Some("SIGUSR1"));

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
    assert_eq!(stopped.output_tail.as_deref(), Some("partial\n"));

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
fn completed_and_failed_ends_cut_a_large_tail_identically() {
    let dir = fakes::TempDir::new("fiber-shell-tail-identity");
    let id = JobId("j_tail".into());
    // Over 2 KiB with line structure, so the window cuts mid-body.
    let mut body = String::new();
    for line in 0..300 {
        body.push_str(&format!("line {line:04}\n"));
    }
    assert!(body.len() > 2_048, "{}", body.len());
    let big = dir.path().join("big");
    std::fs::write(&big, &body).unwrap();

    let end = |path: &Path, command: &str| {
        to_completed(
            id.clone(),
            path,
            finished(Some(status_of(command)), None, false, false, true),
            Some(Limit {
                ms: 1,
                monitor: false,
                // No test command at this site holds a `|`.
                has_pipe: false,
            }),
        )
    };
    let completed_tail = end(&big, "exit 0").output_tail.unwrap();
    let failed_tail = end(&big, "exit 3").output_tail.unwrap();
    assert_eq!(completed_tail, failed_tail);
    assert_eq!(completed_tail.len(), 2_048);
    assert!(body.ends_with(&completed_tail), "{completed_tail:?}");
}

#[test]
fn the_jobs_stop_cancels_once_and_wakes_subscribers() {
    let cancel = Token::new();
    assert!(!cancel.is_cancelled());
    let hits = Arc::new(Hits(AtomicUsize::new(0)));
    let wake: Arc<dyn Wake> = hits.clone();
    cancel.subscribe(Arc::downgrade(&wake));
    let stop: Stop = stop_of(&cancel);
    (stop.0)();
    assert!(cancel.is_cancelled());
    assert_eq!(hits.0.load(Ordering::SeqCst), 1);
    (stop.0)();
    assert_eq!(hits.0.load(Ordering::SeqCst), 1);
    let late = Arc::new(Hits(AtomicUsize::new(0)));
    let late_wake: Arc<dyn Wake> = late.clone();
    cancel.subscribe(Arc::downgrade(&late_wake));
    assert_eq!(late.0.load(Ordering::SeqCst), 0);
    assert!(Cancel::is_cancelled(&cancel));
}

struct Hits(AtomicUsize);

impl Wake for Hits {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct CountWake(AtomicUsize);

impl Wake for CountWake {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn a_move_request_sets_asked_and_wakes_the_drive_loop() {
    let dir = fakes::TempDir::new("fiber-ask");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let waker = Arc::new(CountWake(AtomicUsize::new(0)));
    let wake: Arc<dyn Wake> = waker.clone();
    let (ask, request) = MoveAsk::register(jobs.as_ref(), Arc::downgrade(&wake));
    assert!(!ask.asked(), "asked before any request");
    assert_eq!(waker.0.load(Ordering::SeqCst), 0);
    assert_eq!(jobs.background(), 1);
    assert!(ask.asked());
    assert_eq!(waker.0.load(Ordering::SeqCst), 1);
    drop(request);
    assert_eq!(jobs.background(), 0, "the jobs kept a finished call alive");
}

#[test]
fn a_request_after_the_call_left_the_foreground_moves_nothing() {
    let dir = fakes::TempDir::new("fiber-ask-over");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let waker = Arc::new(CountWake(AtomicUsize::new(0)));
    let wake: Arc<dyn Wake> = waker.clone();
    let (ask, _request) = MoveAsk::register(jobs.as_ref(), Arc::downgrade(&wake));
    ask.end();
    assert_eq!(jobs.background(), 0);
    assert!(!ask.asked(), "a request after the end counted");
    assert_eq!(
        waker.0.load(Ordering::SeqCst),
        0,
        "a request after the end woke the loop"
    );
}

#[test]
fn a_request_after_it_was_asked_still_counts_until_the_end() {
    let dir = fakes::TempDir::new("fiber-ask-twice");
    let jobs = fakes::jobs::FakeJobs::new(dir.path());
    let wake: Arc<dyn Wake> = Arc::new(CountWake(AtomicUsize::new(0)));
    let (ask, _request) = MoveAsk::register(jobs.as_ref(), Arc::downgrade(&wake));
    assert_eq!(jobs.background(), 1);
    assert_eq!(jobs.background(), 1);
    ask.end();
    assert!(!ask.asked(), "an ended ask still reads as asked");
}

#[test]
fn a_capped_job_ends_failed_with_output_cap_whatever_else_ended_it() {
    let dir = fakes::TempDir::new("fiber-shell-capped");
    let path = dir.path().join("out");
    std::fs::write(&path, "partial\n").unwrap();
    let id = JobId("j_1".into());
    let capped = |mut finished: Finished| {
        finished.capped = true;
        to_completed(
            id.clone(),
            &path,
            finished,
            Some(Limit {
                ms: 5_000,
                monitor: false,
                // No test command at this site holds a `|`.
                has_pipe: false,
            }),
        )
    };
    let cases = [
        finished(None, Some(StopKind::Cancel), false, true, false),
        finished(None, Some(StopKind::Timeout), false, true, false),
        finished(None, Some(StopKind::Cancel), true, true, false),
        finished(Some(status_of("exit 0")), None, false, false, true),
        finished(Some(status_of("exit 3")), None, false, false, true),
    ];
    for case in cases {
        let end = capped(case);
        assert_eq!(end.status, Outcome::Failed);
        let error = end.error.unwrap();
        assert_eq!(error.code, ErrorCode::OutputCap);
        assert_eq!(
            error.message,
            "The output passed 5 GB and the job was stopped."
        );
        assert_eq!(end.output_tail.as_deref(), Some("partial\n"));
        assert!(end.process.is_some());
    }
    let exited = capped(finished(
        Some(status_of("exit 3")),
        None,
        false,
        false,
        true,
    ));
    assert_eq!(exited.process.unwrap().exit_code, Some(3));
}

#[test]
fn a_flooded_monitor_ends_flooded_unless_the_cap_fired() {
    let dir = fakes::TempDir::new("fiber-shell-flooded");
    let path = dir.path().join("out");
    std::fs::write(&path, "partial\n").unwrap();
    let id = JobId("j_1".into());
    let deadline = Limit {
        ms: 5_000,
        monitor: true,
        // A monitor's deadline names itself either way.
        has_pipe: false,
    };
    let flooded = |mut finished: Finished| {
        finished.flooded = true;
        to_completed(id.clone(), &path, finished, Some(deadline))
    };
    for case in [
        finished(None, Some(StopKind::Cancel), false, true, false),
        finished(None, Some(StopKind::Timeout), false, true, false),
        finished(None, Some(StopKind::Cancel), true, true, false),
    ] {
        let end = flooded(case);
        assert_eq!(end.status, Outcome::Failed);
        let error = end.error.unwrap();
        assert_eq!(error.code, ErrorCode::Flooded);
        assert_eq!(
            error.message,
            "The monitor's output was suppressed for 30 seconds, so it was stopped. Restart it with a more selective source."
        );
    }
    let mut both = finished(None, Some(StopKind::Cancel), false, true, false);
    both.capped = true;
    let end = flooded(both);
    assert_eq!(end.error.unwrap().code, ErrorCode::OutputCap);
}

#[test]
fn a_monitor_at_its_deadline_says_it_expired() {
    let dir = fakes::TempDir::new("fiber-shell-deadline-end");
    let path = dir.path().join("out");
    std::fs::write(&path, "partial\n").unwrap();
    let end = to_completed(
        JobId("j_1".into()),
        &path,
        finished(None, Some(StopKind::Timeout), false, true, false),
        Some(Limit {
            ms: 5_000,
            monitor: true,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
    );
    assert_eq!(end.status, Outcome::Failed);
    assert!(end.process.unwrap().timed_out);
    let error = end.error.unwrap();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert_eq!(
        error.message,
        "The monitor's deadline of 5000 ms passed; start it again if you still need it."
    );
    // An escapee holding the pipe past the drain is still indeterminate.
    let held = to_completed(
        JobId("j_1".into()),
        &path,
        finished(None, Some(StopKind::Timeout), true, true, false),
        Some(Limit {
            ms: 5_000,
            monitor: true,
            // No test command at this site holds a `|`.
            has_pipe: false,
        }),
    );
    assert_eq!(held.error.unwrap().code, ErrorCode::Indeterminate);
}

#[test]
fn a_terminal_receipt_says_how_to_type_and_carries_the_output_so_far() {
    let started = JobStarted {
        job_id: JobId("j_1".into()),
        tool: Some("shell".into()),
        extension: None,
        description: "python3".into(),
        output_path: "j_1.log".into(),

        pgid: None,
        start_time: None,
        kind: None,
    };
    let path = Path::new("/tmp/j_1.log");
    let reason = MoveReason::StartedInBackground;
    let text_of = |first: &str| {
        let output = terminal_receipt(&started, path, &reason, None, first);
        assert_eq!(
            output.jobs,
            vec![contract::jobs::JobRecord::Started(started.clone())]
        );
        let ContentPart::Text { text } = &output.content[0] else {
            panic!("receipt text");
        };
        text.clone()
    };
    let head = "Started in a terminal and moved to the background.\nJob j_1. Output: /tmp/j_1.log. Type into it with `jobs write`; `jobs wait` waits for it.\n";
    assert_eq!(text_of(""), head);
    assert_eq!(text_of(">>> "), format!("{head}Output so far:\n>>> \n"));
    assert_eq!(text_of("a\n"), format!("{head}Output so far:\na\n"));
    let other = terminal_receipt(&started, path, &MoveReason::AfterThirtySeconds, None, "");
    let ContentPart::Text { text } = &other.content[0] else {
        panic!("receipt text");
    };
    assert!(text.starts_with("Still running after 30 seconds"), "{text}");
}

#[test]
fn the_receipt_gains_the_spills_extra_line() {
    let started = JobStarted {
        job_id: JobId("j_1".into()),
        tool: Some("shell".into()),
        extension: None,
        description: "echo hi".into(),
        output_path: "j_1.log".into(),

        pgid: None,
        start_time: None,
        kind: None,
    };
    let path = Path::new("/tmp/j_1.log");
    let plain = receipt(&started, path, &MoveReason::StartedInBackground, None);
    assert_eq!(with_spill_line(plain, None).content.len(), 1);
    let plain = receipt(&started, path, &MoveReason::StartedInBackground, None);
    let lined = with_spill_line(
        plain,
        Some("Output before the move is also in /s/s_1/artifacts/s_x.txt.".to_owned()),
    );
    let ContentPart::Text { text } = &lined.content[0] else {
        panic!("receipt text");
    };
    assert!(
        text.ends_with("Output before the move is also in /s/s_1/artifacts/s_x.txt.\n"),
        "{text}"
    );
}

#[test]
fn the_deadline_closes_the_receipt_after_the_spill_line() {
    // `docs/tools.md`, "Moving to the background": the receipt ends with the
    // stop sentence, so the deadline comes after any spill line.
    let started = JobStarted {
        job_id: JobId("j_1".into()),
        tool: Some("shell".into()),
        extension: None,
        description: "echo hi".into(),
        output_path: "j_1.log".into(),

        pgid: None,
        start_time: None,
        kind: None,
    };
    let path = Path::new("/tmp/j_1.log");
    let plain = receipt(&started, path, &MoveReason::StartedInBackground, None);
    // A wall a minute past the fake clock's 2023-11-14T22:13:20Z origin.
    let wall = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_060);
    let composed = with_deadline(
        with_spill_line(
            plain,
            Some("Output before the move is also in /s/s_1/artifacts/s_x.txt.".to_owned()),
        ),
        Some((60_000, wall)),
    );
    let ContentPart::Text { text } = &composed.content[0] else {
        panic!("receipt text");
    };
    assert!(text.contains("Output before the move is also in"), "{text}");
    assert!(
        text.contains("Fiber stops it at 2023-11-14T22:14:20Z (timeout_ms 60000)"),
        "{text}"
    );
    assert!(text.ends_with("or with `detach`.\n"), "{text}");
    let spill_at = text.find("Output before the move").expect("spill line");
    let deadline_at = text.find("Fiber stops it at").expect("deadline line");
    assert!(spill_at < deadline_at, "{text}");
}
