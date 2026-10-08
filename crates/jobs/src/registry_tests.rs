//! Opening a job, reporting its end, and delivering that end once.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::Write as _;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::Duration;

use contract::clock::Clock as _;
use contract::events::{JobCompleted, Outcome};
use contract::inbox::Delivery;
use contract::jobs::{Foreground, JobRecord, Jobs, OpenError, Opening, Stop};
use contract::shapes::{Failure, Process};
use contract::{ErrorCode, JobId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, Recorder, TempDir};

use super::Registry;

const DEADLINE: Duration = Duration::from_secs(5);

fn world() -> (TempDir, Arc<Registry>) {
    let dir = TempDir::new("fiber-jobs");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    (
        dir,
        Registry::new(artifacts, clock, Arc::new(Recorder::default())),
    )
}

fn opening(description: &str) -> Opening {
    Opening {
        tool: "shell".into(),
        description: description.into(),
        stop: Stop(Box::new(|| {})),
        lines: false,
        input: None,
    }
}

fn is_job_id(id: &str) -> bool {
    let Some(hex) = id.strip_prefix("j_") else {
        return false;
    };
    hex.len() == 16
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn failure() -> JobCompleted {
    JobCompleted {
        job_id: JobId("j_other".into()),
        status: Outcome::Failed,
        error: Some(Failure {
            code: ErrorCode::Indeterminate,
            message: "The job ended without a result.".into(),
            retry_after_ms: None,
            provider: None,
        }),
        process: None,
        output_tail: None,
    }
}

#[test]
fn open_through_the_jobs_trait_mints_an_id_and_a_file() {
    let (_dir, registry) = world();
    let jobs: &dyn contract::jobs::Jobs = registry.as_ref();
    let opened = contract::jobs::Jobs::open(jobs, opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    assert!(is_job_id(&id), "{id}");
    assert_eq!(opened.started.tool.as_deref(), Some("shell"));
    assert_eq!(opened.started.description, "npm test");
    assert_eq!(opened.started.output_path, format!("artifacts/{id}.log"));
    assert!(opened.path.is_file());
    assert!(registry.list_text().contains(&id));
    assert!(registry.list_text().contains("npm test"));
    drop(opened.end);
}

#[test]
fn open_without_the_registry_arc_records_nothing() {
    let dir = TempDir::new("fiber-jobs-detached");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let registry = Registry {
        artifacts: artifacts.clone(),
        clock,
        emit: Arc::new(Recorder::default()),
        inner: Mutex::new(super::Inner {
            jobs: Vec::new(),
            seq: 0,
            inbox: None,
            foreground: Vec::new(),
        }),
        cv: Condvar::new(),
        me: Weak::new(),
    };
    let Err(error) = registry.open(opening("npm test")) else {
        panic!("open recorded a job without the registry");
    };
    assert!(matches!(error, OpenError::Io { .. }));
    assert!(error.to_string().contains("registry is gone"), "{error}");
    assert!(
        std::fs::read_dir(&artifacts).unwrap().next().is_none(),
        "open created a file without the registry"
    );
    assert_eq!(registry.list_text(), "No jobs.\n");
}

#[test]
fn open_mints_an_id_and_an_empty_output_file() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    assert!(is_job_id(&id), "{id}");
    assert_eq!(opened.started.tool.as_deref(), Some("shell"));
    assert_eq!(opened.started.extension, None);
    assert_eq!(opened.started.description, "npm test");
    assert_eq!(opened.started.output_path, format!("artifacts/{id}.log"));
    assert!(opened.path.is_absolute());
    assert_eq!(
        opened.path.file_name().and_then(|name| name.to_str()),
        Some(format!("{id}.log").as_str())
    );
    assert!(opened.path.exists());
    assert_eq!(std::fs::metadata(&opened.path).unwrap().len(), 0);
    writeln!(&opened.file, "hello").unwrap();
    assert_eq!(std::fs::read_to_string(&opened.path).unwrap(), "hello\n");
    drop(opened.end);
}

#[test]
fn two_opens_mint_different_ids() {
    let (_dir, registry) = world();
    let first = registry.open(opening("one")).unwrap();
    let second = registry.open(opening("two")).unwrap();
    assert_ne!(first.started.job_id, second.started.job_id);
    drop(first.end);
    drop(second.end);
}

#[test]
fn open_into_a_missing_artifacts_directory_records_nothing() {
    let dir = TempDir::new("fiber-jobs-missing");
    let artifacts = dir.path().join("artifacts");
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let registry = Registry::new(artifacts.clone(), clock, Arc::new(Recorder::default()));
    let Err(error) = registry.open(opening("npm test")) else {
        panic!("open recorded a job when the output file could not be created");
    };
    assert!(matches!(error, OpenError::Io { .. }));
    assert_eq!(error.code(), ErrorCode::ToolError);
    let text = error.to_string();
    assert!(text.contains("could not be created"), "{text}");
    assert!(text.contains(&artifacts.display().to_string()), "{text}");
    assert_eq!(registry.list_text(), "No jobs.\n");
}

#[test]
fn calling_end_records_that_result_and_drop_does_not_record_again() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(JobCompleted {
        job_id: JobId(id.clone()),
        status: Outcome::Completed,
        error: None,
        process: Some(Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        }),
        output_tail: None,
    });
    let cancel = CancelToken::new();
    let answer = registry.wait(&id, 0, &cancel).unwrap();
    let Some(JobRecord::Completed(completed)) = answer.record else {
        panic!("{:?}", answer.record);
    };
    assert_eq!(completed.job_id, JobId(id.clone()));
    assert_eq!(completed.status, Outcome::Completed);
    assert_eq!(completed.error, None);
    let again = registry.wait(&id, 0, &cancel).unwrap();
    assert!(again.record.is_none());
    assert!(again.text.contains("completed"), "{}", again.text);
    assert!(!again.text.contains("without a result"), "{}", again.text);
}

#[test]
fn a_dropped_end_records_its_job_failed_indeterminate() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    drop(opened.end);
    let cancel = CancelToken::new();
    let answer = registry.wait(&id, 0, &cancel).unwrap();
    assert_eq!(
        answer.record,
        Some(JobRecord::Completed(JobCompleted {
            job_id: JobId(id.clone()),
            ..failure()
        }))
    );
    let again = registry.wait(&id, 0, &cancel).unwrap();
    assert!(again.record.is_none());
}

#[test]
fn a_dropped_end_sends_one_indeterminate_notice_for_its_own_job() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    drop(opened);
    let sent = notice(&rx);
    assert!(rx.try_recv().is_err(), "a dropped end reports once");
    assert_eq!(
        sent.completed,
        JobCompleted {
            job_id: JobId(id),
            ..failure()
        }
    );
}

#[test]
fn a_reported_end_sends_one_notice_with_its_own_id_and_no_fallback() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok("j_other"));
    let sent = notice(&rx);
    assert!(rx.try_recv().is_err(), "a reported end reports once");
    assert_eq!(sent.completed, ended_ok(&id));
}

#[test]
fn the_completion_is_claimed_once() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(JobCompleted {
        job_id: JobId(id.clone()),
        status: Outcome::Cancelled,
        error: None,
        process: None,
        output_tail: None,
    });
    let cancel = CancelToken::new();
    let first = registry.wait(&id, 0, &cancel).unwrap();
    let second = registry.wait(&id, 0, &cancel).unwrap();
    assert!(matches!(first.record, Some(JobRecord::Completed(_))));
    assert!(second.record.is_none());
    assert!(second.text.contains("cancelled"), "{}", second.text);
}

#[test]
fn a_wake_after_the_sequence_snapshot_returns_the_wait() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    let waited = id.clone();
    let (tx, rx) = mpsc::channel();
    let waiting = Arc::clone(&registry);
    let end = opened.end;
    let reported = id.clone();
    thread::spawn(move || {
        super::park::BEFORE_PARK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                (end.0)(JobCompleted {
                    job_id: JobId(reported),
                    status: Outcome::Completed,
                    error: None,
                    process: None,
                    output_tail: None,
                });
            }));
        });
        let answer = waiting.wait(&id, 60_000, &CancelToken::new()).unwrap();
        let _sent = tx.send(answer);
    });
    let answer = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the wait returned after a wake that landed before it parked");
    let Some(JobRecord::Completed(completed)) = answer.record else {
        panic!("the wait did not deliver the completion: {}", answer.text);
    };
    assert_eq!(completed.status, Outcome::Completed);
    assert_eq!(completed.job_id, JobId(waited));
}

fn ended_ok(id: &str) -> JobCompleted {
    JobCompleted {
        job_id: JobId(id.into()),
        status: Outcome::Completed,
        error: None,
        process: Some(Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        }),
        output_tail: None,
    }
}

fn notice(rx: &mpsc::Receiver<Delivery>) -> contract::inbox::JobNotice {
    let delivery = rx.try_recv().expect("the end sent a notice");
    let Delivery::Job(notice) = delivery else {
        panic!("the end sent {delivery:?}");
    };
    notice
}

#[test]
fn an_unclaimed_end_sends_one_notice_whose_claim_holds_once() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let sent = notice(&rx);
    assert!(rx.try_recv().is_err(), "one end sends one notice");
    assert_eq!(sent.completed, ended_ok(&id));
    assert!((sent.claim.0)(), "the notice claims the unclaimed end");
    let cancel = CancelToken::new();
    let answer = registry.wait(&id, 0, &cancel).unwrap();
    assert!(answer.record.is_none(), "the notice already claimed it");
    assert!(answer.text.contains("completed"), "{}", answer.text);
}

fn line(id: &str, lines: &str, suppressed: Option<u64>) -> contract::events::JobLine {
    contract::events::JobLine {
        job_id: JobId(id.into()),
        lines: lines.into(),
        suppressed,
    }
}

#[test]
fn lines_are_offered_only_to_a_job_that_asks() {
    let (_dir, registry) = world();
    let plain = registry.open(opening("npm test")).unwrap();
    assert!(plain.lines.is_none());
    let monitor = registry
        .open(Opening {
            lines: true,
            ..opening("tail -f log")
        })
        .unwrap();
    assert!(monitor.lines.is_some());
}

#[test]
fn a_monitors_lines_reach_the_inbox_in_order_before_its_notice() {
    let (_dir, registry) = world();
    let opened = registry
        .open(Opening {
            lines: true,
            ..opening("tail -f log")
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let lines = opened.lines.unwrap();
    // Before `deliver_to` a batch is dropped.
    (lines.0)(line(&id, "early", None));
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    (lines.0)(line(&id, "one", None));
    (lines.0)(line(&id, "two", Some(3)));
    (opened.end.0)(ended_ok(&id));
    for expected in [line(&id, "one", None), line(&id, "two", Some(3))] {
        let delivery = rx.try_recv().expect("a batch");
        let Delivery::JobLine(sent) = delivery else {
            panic!("expected a line, got {delivery:?}");
        };
        assert_eq!(sent, expected);
    }
    let _end = notice(&rx);
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_notice_after_a_wait_claimed_the_end_does_not_hold() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let answer = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(matches!(answer.record, Some(JobRecord::Completed(_))));
    let sent = notice(&rx);
    assert!(!(sent.claim.0)(), "the wait claimed the end first");
}

#[test]
fn a_second_end_report_sends_no_second_notice() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    registry.finish(ended_ok(&id));
    registry.finish(ended_ok(&id));
    let _first = notice(&rx);
    assert!(rx.try_recv().is_err(), "a job ends once");
    drop(opened.end);
    assert!(rx.try_recv().is_err(), "a job ends once");
}

#[test]
fn a_claim_after_the_registry_is_gone_does_not_hold() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    drop(registry);
    let sent = notice(&rx);
    assert!(!(sent.claim.0)());
}

#[test]
fn without_an_inbox_an_end_sends_nothing() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let answer = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(matches!(answer.record, Some(JobRecord::Completed(_))));
}

#[test]
fn an_end_after_the_loop_is_gone_is_still_recorded() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    drop(rx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    (opened.end.0)(ended_ok(&id));
    let answer = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(matches!(answer.record, Some(JobRecord::Completed(_))));
}

fn counted_stop(
    registry: &Arc<Registry>,
) -> (
    JobId,
    Arc<std::sync::atomic::AtomicUsize>,
    contract::jobs::Opened,
) {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "sleep".into(),
            stop: Stop(Box::new(move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
            lines: false,
            input: None,
        })
        .unwrap();
    (opened.started.job_id.clone(), calls, opened)
}

#[test]
fn stop_through_the_trait_sends_the_stop_once_and_reports_running() {
    let (_dir, registry) = world();
    let (id, calls, opened) = counted_stop(&registry);
    assert!(Jobs::stop(registry.as_ref(), &id));
    assert!(
        Jobs::stop(registry.as_ref(), &id),
        "a second stop is still running"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    drop(opened);
}

#[test]
fn stop_through_the_trait_on_an_ended_or_unknown_job_is_false() {
    let (_dir, registry) = world();
    let (id, calls, opened) = counted_stop(&registry);
    (opened.end.0)(ended_ok(&id.0));
    assert!(!Jobs::stop(registry.as_ref(), &id));
    assert!(!Jobs::stop(registry.as_ref(), &JobId("j_missing".into())));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn background_asks_each_live_call_once_and_counts_the_ones_that_move() {
    let (_dir, registry) = world();
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let moves = |moves: bool| {
        let asked = Arc::clone(&asked);
        let call: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            moves
        });
        registry.foreground(Foreground(Arc::downgrade(&call)));
        call
    };
    let first = moves(true);
    let second = moves(false);
    let dropped = moves(true);
    drop(dropped);
    assert_eq!(registry.background(), 1);
    assert_eq!(
        asked.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "each live call is asked once; a dropped one is not"
    );
    drop(first);
    assert_eq!(registry.background(), 0);
    drop(second);
    assert_eq!(registry.background(), 0);
}

#[test]
fn background_with_no_calls_is_zero() {
    let (_dir, registry) = world();
    assert_eq!(registry.background(), 0);
}

#[test]
fn registering_forgets_the_calls_that_are_gone() {
    let (_dir, registry) = world();
    for _ in 0..3 {
        let call: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| true);
        registry.foreground(Foreground(Arc::downgrade(&call)));
    }
    let live: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| true);
    registry.foreground(Foreground(Arc::downgrade(&live)));
    assert_eq!(super::lock(&registry.inner).foreground.len(), 1);
}

#[test]
fn an_opened_job_emits_through_the_registrys_emitter() {
    let dir = TempDir::new("fiber-jobs-emit");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    let recorder = Arc::new(Recorder::default());
    let registry = Registry::new(artifacts, clock, Arc::clone(&recorder) as _);
    let opened = registry.open(opening("npm test")).unwrap();
    let event = contract::events::Event::JobDelta(contract::events::JobDelta {
        job_id: opened.started.job_id.clone(),
        progress: contract::events::Progress {
            text: Some("x".to_owned()),
            details: None,
        },
    });
    opened.emit.emit(&event);
    assert_eq!(recorder.events(), vec![event]);
}

// `write`: typing into a job started with `tty` and collecting what arrives.

type Typed = Arc<Mutex<Vec<u8>>>;

fn clocked_world() -> (TempDir, Arc<FakeClock>, Arc<Registry>) {
    let dir = TempDir::new("fiber-jobs-write");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let clock = FakeClock::new();
    let as_clock: Arc<dyn contract::clock::Clock> = clock.clone();
    let registry = Registry::new(artifacts, as_clock, Arc::new(Recorder::default()));
    (dir, clock, registry)
}

/// A `tty` job whose terminal answers every write by appending `reply` to
/// the output file, as the reader does for a program's echo.
fn open_tty(
    registry: &Arc<Registry>,
    reply: &'static [u8],
) -> (String, contract::jobs::Opened, Typed) {
    let typed: Typed = Arc::default();
    let sink: Arc<Mutex<Option<std::fs::File>>> = Arc::default();
    let (record, file) = (Arc::clone(&typed), Arc::clone(&sink));
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "python3".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(move |bytes, _, _| {
                record.lock().unwrap().extend_from_slice(bytes);
                file.lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .write_all(reply)
                    .map(|()| bytes.len())
            }))),
        })
        .unwrap();
    *sink.lock().unwrap() = Some(opened.file.try_clone().unwrap());
    (opened.started.job_id.0.clone(), opened, typed)
}

/// Runs `write` on its own thread and returns the answer's channel.
fn writing(
    registry: &Arc<Registry>,
    id: &str,
    input: &'static str,
    wait_ms: u64,
    cancel: CancelToken,
) -> mpsc::Receiver<Result<super::Answer, super::WriteError>> {
    let registry = Arc::clone(registry);
    let id = id.to_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _sent = tx.send(registry.write(&id, input, wait_ms, &cancel));
    });
    rx
}

#[test]
fn write_returns_the_output_after_the_write_when_the_wait_passes() {
    let (_dir, clock, registry) = clocked_world();
    let (id, opened, typed) = open_tty(&registry, b"got:hi\r\n");
    // Output from before the write is not part of the answer.
    writeln!(&opened.file, "earlier").unwrap();
    let deadline = clock.now().checked_add(Duration::from_millis(250)).unwrap();
    let rx = writing(&registry, &id, "hi\n", 250, CancelToken::new());
    assert!(
        clock.await_parked(deadline, DEADLINE),
        "the write did not park at its wait"
    );
    assert!(rx.try_recv().is_err(), "the write returned before its wait");
    clock.advance(Duration::from_millis(250));
    let answer = rx
        .recv_timeout(DEADLINE)
        .expect("the write returned")
        .ok()
        .unwrap();
    assert_eq!(*typed.lock().unwrap(), b"hi\n");
    assert!(answer.text.starts_with("got:hi\r\n"), "{}", answer.text);
    assert!(!answer.text.contains("earlier"), "{}", answer.text);
    assert!(answer.text.contains("still running"), "{}", answer.text);
    assert!(answer.record.is_none());
    drop(opened.end);
}

#[test]
fn write_returns_at_once_with_the_final_state_when_the_job_ends_in_the_wait() {
    let (_dir, clock, registry) = clocked_world();
    let (id, opened, _typed) = open_tty(&registry, b"got:hi\n");
    let deadline = clock.now().checked_add(Duration::from_millis(250)).unwrap();
    let rx = writing(&registry, &id, "hi\n", 250, CancelToken::new());
    assert!(clock.await_parked(deadline, DEADLINE));
    (opened.end.0)(ended_ok(&id));
    let answer = rx
        .recv_timeout(DEADLINE)
        .expect("the write returned")
        .ok()
        .unwrap();
    assert!(answer.text.starts_with("got:hi\n"), "{}", answer.text);
    assert!(answer.text.contains("completed"), "{}", answer.text);
    assert!(!answer.text.contains("still running"), "{}", answer.text);
    assert!(matches!(answer.record, Some(JobRecord::Completed(_))));
    // The completion was claimed by the write.
    let again = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(again.record.is_none());
}

#[test]
fn a_cancel_returns_the_write_at_once_with_what_arrived() {
    let (_dir, clock, registry) = clocked_world();
    let (id, opened, _typed) = open_tty(&registry, b"partial\n");
    let deadline = clock.now().checked_add(Duration::from_millis(250)).unwrap();
    let cancel = CancelToken::new();
    let rx = writing(&registry, &id, "x", 250, cancel.clone());
    assert!(clock.await_parked(deadline, DEADLINE));
    cancel.cancel();
    let answer = rx
        .recv_timeout(DEADLINE)
        .expect("the cancelled write returned")
        .ok()
        .unwrap();
    assert!(answer.text.starts_with("partial\n"), "{}", answer.text);
    assert!(answer.text.contains("still running"), "{}", answer.text);
    drop(opened.end);
}

#[test]
fn write_does_not_reach_a_job_without_a_terminal_an_unknown_job_or_an_ended_one() {
    let (_dir, _clock, registry) = clocked_world();
    let plain = registry.open(opening("ls")).unwrap();
    let plain_id = plain.started.job_id.0.clone();
    let cancel = CancelToken::new();
    assert!(matches!(
        registry.write(&plain_id, "x", 0, &cancel),
        Err(super::WriteError::NotTty)
    ));
    assert!(matches!(
        registry.write("j_missing", "x", 0, &cancel),
        Err(super::WriteError::Unknown)
    ));
    let (id, tty, typed) = open_tty(&registry, b"");
    (tty.end.0)(ended_ok(&id));
    assert!(matches!(
        registry.write(&id, "x", 0, &cancel),
        Err(super::WriteError::Ended(Outcome::Completed))
    ));
    assert!(
        typed.lock().unwrap().is_empty(),
        "a write reached an ended job"
    );
    drop(plain.end);
}

#[test]
fn a_failed_write_to_the_terminal_is_an_io_error() {
    let (_dir, _clock, registry) = clocked_world();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|_, _, _| {
                Err(std::io::Error::other("the terminal is closed"))
            }))),
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let result = registry.write(&id, "x", 0, &CancelToken::new());
    assert!(
        matches!(&result, Err(super::WriteError::Io(err)) if err.to_string().contains("closed"))
    );
    drop(opened.end);
}

#[test]
fn write_returns_all_the_output_with_no_cut() {
    const BIG: &[u8] = &[b'a'; 40 * 1024];
    let (_dir, _clock, registry) = clocked_world();
    let (id, opened, _typed) = open_tty(&registry, BIG);
    let answer = registry
        .write(&id, "x", 0, &CancelToken::new())
        .ok()
        .unwrap();
    assert_eq!(answer.text.lines().next().unwrap().len(), 40 * 1024);
    assert!(!answer.text.contains("omitted"));
    drop(opened.end);
}

#[test]
fn a_write_starting_inside_a_character_drops_its_continuation_bytes() {
    let (_dir, _clock, registry) = clocked_world();
    let (id, opened, _typed) = open_tty(&registry, b"\xA9ok");
    // The file ends inside U+00E9 before the write, so the new bytes begin
    // with its second byte.
    (&opened.file).write_all(b"x\xC3").unwrap();
    let answer = registry
        .write(&id, "x", 0, &CancelToken::new())
        .ok()
        .unwrap();
    assert!(answer.text.starts_with("ok\n"), "{:?}", answer.text);
    drop(opened.end);
}

#[test]
fn a_write_that_typed_less_than_it_was_given_says_so() {
    let (_dir, _clock, registry) = clocked_world();
    let opened = registry
        .open(Opening {
            tool: "shell".into(),
            description: "cat".into(),
            stop: Stop(Box::new(|| {})),
            lines: false,
            input: Some(contract::jobs::Input(Box::new(|_, _, _| Ok(1)))),
        })
        .unwrap();
    let id = opened.started.job_id.0.clone();
    let answer = registry
        .write(&id, "abc", 0, &CancelToken::new())
        .ok()
        .unwrap();
    assert!(
        answer.text.starts_with("Wrote 1 of 3 bytes.\n"),
        "{}",
        answer.text
    );
    let all = registry
        .write(&id, "", 0, &CancelToken::new())
        .ok()
        .unwrap();
    assert!(!all.text.contains("Wrote"), "{}", all.text);
    drop(opened.end);
}

#[test]
fn an_ended_job_drops_its_input() {
    let (_dir, _clock, registry) = clocked_world();
    let (id, opened, typed) = open_tty(&registry, b"");
    assert_eq!(Arc::strong_count(&typed), 2);
    (opened.end.0)(ended_ok(&id));
    assert_eq!(Arc::strong_count(&typed), 1, "the ended job kept its input");
    assert!(matches!(
        registry.write(&id, "x", 0, &CancelToken::new()),
        Err(super::WriteError::Ended(Outcome::Completed))
    ));
}

#[test]
fn since_text_ends_in_one_newline_and_is_empty_for_no_bytes() {
    let dir = TempDir::new("fiber-jobs-since");
    let path = dir.path().join("out.log");
    std::fs::write(&path, b"").unwrap();
    assert_eq!(super::since_text(&path, 0), "");
    std::fs::write(&path, b"a").unwrap();
    assert_eq!(super::since_text(&path, 0), "a\n");
    std::fs::write(&path, b"a\n").unwrap();
    assert_eq!(super::since_text(&path, 0), "a\n");
    assert_eq!(super::since_text(&dir.path().join("missing.log"), 0), "");
}

#[test]
fn since_text_from_the_start_keeps_every_byte_and_from_inside_drops_continuations() {
    let dir = TempDir::new("fiber-jobs-since-bytes");
    let path = dir.path().join("out.log");
    // A continuation byte at the very start is invalid, not half of an
    // earlier character, so nothing is dropped.
    std::fs::write(&path, b"\xA9ok").unwrap();
    assert_eq!(super::since_text(&path, 0), "\u{fffd}ok\n");
    // From one byte in, the same byte is the end of a split character.
    assert_eq!(super::since_text(&path, 1), "ok\n");
    std::fs::write(&path, b"ab").unwrap();
    assert_eq!(super::since_text(&path, 1), "b\n");
}

#[test]
fn running_lists_running_jobs_in_start_order_and_drops_ended_ones() {
    let (_dir, registry) = world();
    let seam: &dyn Jobs = registry.as_ref();
    assert!(seam.running().is_empty());
    let first = seam.open(opening("one")).unwrap();
    let second = seam.open(opening("two")).unwrap();
    let third = seam.open(opening("three")).unwrap();
    let ids = [
        first.started.job_id.clone(),
        second.started.job_id.clone(),
        third.started.job_id.clone(),
    ];
    assert_eq!(seam.running(), ids.to_vec());
    (second.end.0)(ended_ok(&ids[1].0));
    assert_eq!(seam.running(), vec![ids[0].clone(), ids[2].clone()]);
    drop(first.end);
    (third.end.0)(ended_ok(&ids[2].0));
    assert!(seam.running().is_empty());
}

#[test]
fn deliver_to_through_the_seam_sends_the_end_before_running_drops_it() {
    let (_dir, registry) = world();
    let seam: &dyn Jobs = registry.as_ref();
    let (tx, rx) = mpsc::channel();
    seam.deliver_to(tx);
    let opened = seam.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.clone();
    assert_eq!(seam.running(), vec![id.clone()]);
    (opened.end.0)(ended_ok(&id.0));
    assert!(seam.running().is_empty());
    let sent = notice(&rx);
    assert_eq!(sent.completed, ended_ok(&id.0));
}
