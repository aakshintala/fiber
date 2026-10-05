//! `Stop` and `End`: their debug text, and that an end reports once.

use std::sync::mpsc;

use super::{End, Stop};
use crate::events::{JobCompleted, Outcome};
use crate::shapes::Failure;
use crate::{ErrorCode, JobId};

fn completed(id: &str, status: Outcome) -> JobCompleted {
    JobCompleted {
        job_id: JobId(id.into()),
        status,
        error: None,
        process: None,
        output_tail: None,
    }
}

#[test]
fn stop_and_end_debug_without_their_closures() {
    let stop = Stop(Box::new(|| {}));
    assert_eq!(format!("{stop:?}"), "Stop(..)");
    let end = End::new(JobId("j_abc".into()), Box::new(|_| {}));
    assert_eq!(format!("{end:?}"), "End(..)");
}

#[test]
fn end_reports_that_completion_once() {
    let (tx, rx) = mpsc::channel();
    let end = End::new(
        JobId("j_abc".into()),
        Box::new(move |completed| tx.send(completed).unwrap()),
    );
    let done = completed("j_reported", Outcome::Completed);
    end.end(done.clone());
    assert_eq!(rx.try_recv().unwrap(), done);
    assert!(rx.try_recv().is_err(), "end reported again on drop");
}

#[test]
fn a_dropped_end_reports_a_tool_error_for_its_job() {
    let (tx, rx) = mpsc::channel();
    let end = End::new(
        JobId("j_abc".into()),
        Box::new(move |completed| tx.send(completed).unwrap()),
    );
    drop(end);
    assert_eq!(
        rx.try_recv().unwrap(),
        JobCompleted {
            job_id: JobId("j_abc".into()),
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
    );
    assert!(rx.try_recv().is_err(), "drop reported twice");
}
