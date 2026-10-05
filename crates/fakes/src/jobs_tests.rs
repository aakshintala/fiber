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
    jobs.stop(&JobId("j_missing".into()));
    assert!(
        fired_rx.try_recv().is_err(),
        "an unknown id called this job's stop"
    );
    jobs.stop(&id);
    assert!(
        fired_rx.recv_timeout(DEADLINE).is_ok(),
        "waited {DEADLINE:?} for the job's stop"
    );
    jobs.stop(&id);
    assert!(
        fired_rx.recv_timeout(DEADLINE).is_ok(),
        "a second stop did not call the closure"
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
