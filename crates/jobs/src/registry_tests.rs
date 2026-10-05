//! Opening a job, reporting its end, and delivering that end once.

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test code")]

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use contract::events::{JobCompleted, Outcome};
use contract::jobs::{JobRecord, OpenError, Opening, Stop};
use contract::shapes::{Failure, Process};
use contract::{ErrorCode, JobId};
use fakes::clock::FakeClock;
use fakes::{CancelToken, TempDir};

use super::{Registry, mint_id};

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
fn mint_id_skips_an_id_that_is_taken() {
    let calls = AtomicUsize::new(0);
    let id = mint_id(|_| {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        n == 0
    });
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(is_job_id(&id), "{id}");
}
