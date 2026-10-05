//! `FakeJobs` opens a file, records the start, and delivers the end.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::Write as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::events::{JobCompleted, Outcome};
use contract::jobs::{Jobs, OpenError, Opening, Stop};
use contract::shapes::Process;
use contract::{ErrorCode, JobId};

use super::FakeJobs;

const DEADLINE: Duration = Duration::from_secs(5);

fn opening(description: &str, stop: Stop) -> Opening {
    Opening {
        tool: "shell".into(),
        description: description.into(),
        stop,
    }
}

fn completed(id: &str, status: Outcome) -> JobCompleted {
    JobCompleted {
        job_id: JobId(id.into()),
        status,
        error: None,
        process: Some(Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        }),
        output_tail: None,
    }
}

#[test]
fn open_creates_a_file_under_the_directory_and_records_the_start() {
    let dir = fakes_temp();
    let jobs = FakeJobs::new(dir.path());
    let opened = jobs
        .open(opening("echo hi", Stop(Box::new(|| {}))))
        .unwrap();
    assert!(opened.path.is_absolute());
    assert!(opened.path.starts_with(dir.path()));
    assert_eq!(
        opened.path.file_name().and_then(|name| name.to_str()),
        Some(format!("{}.log", opened.started.job_id.0).as_str())
    );
    assert_eq!(std::fs::metadata(&opened.path).unwrap().len(), 0);
    writeln!(&opened.file, "hello").unwrap();
    assert_eq!(std::fs::read_to_string(&opened.path).unwrap(), "hello\n");
    assert_eq!(opened.started.tool.as_deref(), Some("shell"));
    assert_eq!(opened.started.extension, None);
    assert_eq!(opened.started.description, "echo hi");
    assert_eq!(
        opened.started.output_path,
        format!("{}.log", opened.started.job_id.0)
    );
    assert_eq!(jobs.started(), vec![opened.started.clone()]);
    drop(opened.end);
    let dropped = jobs.ended(DEADLINE).unwrap();
    assert_eq!(dropped.job_id, opened.started.job_id);
    assert_eq!(dropped.status, Outcome::Failed);
    assert_eq!(
        dropped.error.as_ref().map(|error| error.code.clone()),
        Some(ErrorCode::ToolError)
    );
}

#[test]
fn two_opens_record_two_starts_with_different_ids() {
    let dir = fakes_temp();
    let jobs = FakeJobs::new(dir.path());
    let first = jobs.open(opening("one", Stop(Box::new(|| {})))).unwrap();
    let second = jobs.open(opening("two", Stop(Box::new(|| {})))).unwrap();
    assert_ne!(first.started.job_id, second.started.job_id);
    let started = jobs.started();
    assert_eq!(started.len(), 2);
    assert_eq!(started[0].description, "one");
    assert_eq!(started[1].description, "two");
    drop(first.end);
    drop(second.end);
}

#[test]
fn stop_calls_that_jobs_stop_and_end_is_delivered() {
    let dir = fakes_temp();
    let jobs = FakeJobs::new(dir.path());
    let (fired_tx, fired_rx) = mpsc::channel();
    let opened = jobs
        .open(opening(
            "echo hi",
            Stop(Box::new(move || fired_tx.send(()).unwrap())),
        ))
        .unwrap();
    let id = opened.started.job_id.clone();
    assert!(jobs.ended(Duration::from_millis(1)).is_none());
    assert!(!jobs.stop(&JobId("j_missing".into())));
    assert!(
        fired_rx.try_recv().is_err(),
        "an unknown id called this job's stop"
    );
    assert!(jobs.stop(&id));
    assert!(
        fired_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the job's stop"
    );
    assert!(jobs.stop(&id), "a second stop of a running job is true");
    let racers: Vec<_> = (0..2)
        .map(|_| {
            let jobs = Arc::clone(&jobs);
            let id = id.clone();
            std::thread::spawn(move || jobs.stop(&id))
        })
        .collect();
    for racer in racers {
        assert!(racer.join().unwrap());
    }
    assert!(
        fired_rx.try_recv().is_err(),
        "a later stop called the closure again"
    );
    let done = completed(&id.0, Outcome::Completed);
    opened.end.end(JobCompleted {
        job_id: JobId("j_other".into()),
        ..done.clone()
    });
    let delivered = jobs.ended(DEADLINE).unwrap();
    assert_eq!(delivered.job_id, id);
    assert_eq!(delivered.status, Outcome::Completed);
    assert_eq!(delivered.process, done.process);
    assert!(jobs.ended(Duration::from_millis(1)).is_none());
}

#[test]
fn stop_calls_only_the_named_job() {
    let dir = fakes_temp();
    let jobs = FakeJobs::new(dir.path());
    let first_flag = Arc::new(AtomicBool::new(false));
    let second_flag = Arc::new(AtomicBool::new(false));
    let first_hit = Arc::clone(&first_flag);
    let second_hit = Arc::clone(&second_flag);
    let first = jobs
        .open(opening(
            "one",
            Stop(Box::new(move || first_hit.store(true, Ordering::SeqCst))),
        ))
        .unwrap();
    let second = jobs
        .open(opening(
            "two",
            Stop(Box::new(move || second_hit.store(true, Ordering::SeqCst))),
        ))
        .unwrap();
    jobs.stop(&second.started.job_id);
    assert!(!first_flag.load(Ordering::SeqCst));
    assert!(second_flag.load(Ordering::SeqCst));
    drop(first.end);
    drop(second.end);
}

#[test]
fn failing_open_is_io_and_records_nothing() {
    let jobs = FakeJobs::failing();
    let Err(error) = jobs.open(opening("echo hi", Stop(Box::new(|| {})))) else {
        panic!("failing open recorded a job");
    };
    assert!(matches!(error, OpenError::Io { .. }));
    assert_eq!(error.code(), ErrorCode::ToolError);
    let text = error.to_string();
    assert!(text.contains("could not be created"), "{text}");
    assert!(text.contains("unavailable.log"), "{text}");
    assert!(jobs.started().is_empty());
    assert!(jobs.ended(Duration::from_millis(1)).is_none());
}

#[test]
fn open_into_a_missing_directory_is_io() {
    let dir = fakes_temp();
    let missing = dir.path().join("missing");
    let jobs = FakeJobs::new(&missing);
    let Err(error) = jobs.open(opening("echo hi", Stop(Box::new(|| {})))) else {
        panic!("open recorded a job when the file could not be created");
    };
    assert!(matches!(error, OpenError::Io { .. }));
    assert!(jobs.started().is_empty());
}

fn fakes_temp() -> crate::TempDir {
    crate::TempDir::new("fiber-fake-jobs")
}

#[test]
fn stop_after_the_end_is_false_and_calls_nothing() {
    let dir = fakes_temp();
    let jobs = FakeJobs::new(dir.path());
    let (fired_tx, fired_rx) = mpsc::channel();
    let opened = jobs
        .open(opening(
            "echo hi",
            Stop(Box::new(move || fired_tx.send(()).unwrap())),
        ))
        .unwrap();
    let id = opened.started.job_id.clone();
    opened.end.end(completed(&id.0, Outcome::Completed));
    assert!(!jobs.stop(&id));
    assert!(
        fired_rx.try_recv().is_err(),
        "an ended job's stop was called"
    );
}

#[test]
fn background_asks_each_live_call_and_counts_the_ones_that_move() {
    let dir = fakes_temp();
    let jobs = FakeJobs::new(dir.path());
    assert_eq!(jobs.background(), 0);
    let moving: std::sync::Arc<dyn Fn() -> bool + Send + Sync> = std::sync::Arc::new(|| true);
    let staying: std::sync::Arc<dyn Fn() -> bool + Send + Sync> = std::sync::Arc::new(|| false);
    let gone: std::sync::Arc<dyn Fn() -> bool + Send + Sync> = std::sync::Arc::new(|| true);
    for call in [&moving, &staying, &gone] {
        jobs.foreground(contract::jobs::Foreground(std::sync::Arc::downgrade(call)));
    }
    drop(gone);
    assert_eq!(jobs.background(), 1);
    drop(moving);
    assert_eq!(jobs.background(), 0);
    drop(staying);
    assert_eq!(jobs.background(), 0);
}

#[test]
fn registering_forgets_the_calls_that_are_gone() {
    let dir = fakes_temp();
    let jobs = FakeJobs::new(dir.path());
    for _ in 0..3 {
        let call: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| true);
        jobs.foreground(contract::jobs::Foreground(Arc::downgrade(&call)));
    }
    let live: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(|| true);
    jobs.foreground(contract::jobs::Foreground(Arc::downgrade(&live)));
    assert_eq!(super::lock(&jobs.inner).foreground.len(), 1);
}

#[test]
fn a_jobs_deltas_are_recorded_in_order_and_other_events_ignored() {
    use contract::events::{Event, JobDelta, Progress};

    let dir = crate::TempDir::new("fiber-fake-jobs-deltas");
    let jobs = FakeJobs::new(dir.path());
    let opened = jobs.open(opening("x", Stop(Box::new(|| {})))).unwrap();
    let id = opened.started.job_id.clone();
    let delta = |text: &str| {
        Event::JobDelta(JobDelta {
            job_id: id.clone(),
            progress: Progress {
                text: Some(text.to_owned()),
                details: None,
            },
        })
    };
    let deltas = jobs.deltas();
    assert!(!deltas.wait_for_text("ab", Duration::from_millis(1)));
    opened.emit.emit(&delta("a"));
    opened.emit.emit(&Event::ToolCallDelta(Progress {
        text: Some("ignored".to_owned()),
        details: None,
    }));
    opened.emit.emit(&delta("b"));
    assert_eq!(
        deltas.deltas(),
        vec![(id.clone(), "a".to_owned()), (id, "b".to_owned())]
    );
    assert_eq!(deltas.text(), "ab");
    assert!(deltas.wait_for_text("ab", DEADLINE));
    assert!(!deltas.wait_for_text("abc", Duration::from_millis(1)));
}

#[test]
fn lacks_is_true_exactly_while_the_text_misses_the_needle() {
    let texts = vec![
        (JobId("j".into()), "ab".to_owned()),
        (JobId("j".into()), "c".to_owned()),
    ];
    assert!(!super::lacks(&texts, "abc"));
    assert!(!super::lacks(&texts, "bc"));
    assert!(super::lacks(&texts, "ac"));
    assert!(super::lacks(&[], "a"));
}

#[test]
fn wait_for_text_sees_a_delta_emitted_after_the_wait_started() {
    use contract::emit::Emit as _;
    use contract::events::{Event, JobDelta, Progress};

    let deltas = super::JobDeltas::default();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            deltas.emit(&Event::JobDelta(JobDelta {
                job_id: JobId("j".into()),
                progress: Progress {
                    text: Some("hello".into()),
                    details: None,
                },
            }));
        });
        assert!(deltas.wait_for_text("hell", DEADLINE));
    });
}
