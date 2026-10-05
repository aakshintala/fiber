//! Opening a job, reporting its end, and delivering that end once.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::Write as _;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::Duration;

use contract::events::{JobCompleted, Outcome};
use contract::inbox::Delivery;
use contract::jobs::{JobRecord, OpenError, Opening, Stop};
use contract::shapes::{Failure, Process};
use contract::{ErrorCode, JobId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, TempDir};

use super::Registry;

fn world() -> (TempDir, Arc<Registry>) {
    let dir = TempDir::new("fiber-jobs");
    let artifacts = dir.path().join("artifacts");
    std::fs::create_dir(&artifacts).unwrap();
    let clock: Arc<dyn contract::clock::Clock> = FakeClock::new();
    (dir, Registry::new(artifacts, clock))
}

fn opening(description: &str) -> Opening {
    Opening {
        tool: "shell".into(),
        description: description.into(),
        stop: Stop(Box::new(|| {})),
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
            code: ErrorCode::ToolError,
            message: "The job ended without a result.".into(),
            retry_after: None,
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
        inner: Mutex::new(super::Inner {
            jobs: Vec::new(),
            seq: 0,
            inbox: None,
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
    let registry = Registry::new(artifacts.clone(), clock);
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
    opened.end.end(JobCompleted {
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
fn a_dropped_end_records_a_tool_error() {
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
fn the_completion_is_claimed_once() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    opened.end.end(JobCompleted {
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
        super::BEFORE_PARK.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                end.end(JobCompleted {
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
    opened.end.end(ended_ok(&id));
    let sent = notice(&rx);
    assert!(rx.try_recv().is_err(), "one end sends one notice");
    assert_eq!(sent.completed, ended_ok(&id));
    assert!((sent.claim.0)(), "the notice claims the unclaimed end");
    let cancel = CancelToken::new();
    let answer = registry.wait(&id, 0, &cancel).unwrap();
    assert!(answer.record.is_none(), "the notice already claimed it");
    assert!(answer.text.contains("completed"), "{}", answer.text);
}

#[test]
fn a_notice_after_a_wait_claimed_the_end_does_not_hold() {
    let (_dir, registry) = world();
    let (tx, rx) = mpsc::channel();
    registry.deliver_to(tx);
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    opened.end.end(ended_ok(&id));
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
    opened.end.end(ended_ok(&id));
    drop(registry);
    let sent = notice(&rx);
    assert!(!(sent.claim.0)());
}

#[test]
fn without_an_inbox_an_end_sends_nothing() {
    let (_dir, registry) = world();
    let opened = registry.open(opening("npm test")).unwrap();
    let id = opened.started.job_id.0.clone();
    opened.end.end(ended_ok(&id));
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
    opened.end.end(ended_ok(&id));
    let answer = registry.wait(&id, 0, &CancelToken::new()).unwrap();
    assert!(matches!(answer.record, Some(JobRecord::Completed(_))));
}
