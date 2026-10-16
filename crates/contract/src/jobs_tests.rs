use super::*;
use crate::events::Outcome;

struct StubJobs;

impl Jobs for StubJobs {
    fn open(&self, _opening: Opening) -> Result<Opened, OpenError> {
        Err(OpenError {
            path: std::path::PathBuf::from("/none"),
            source: std::io::Error::other("refused"),
        })
    }

    fn stop(&self, _job_id: &crate::JobId) -> bool {
        false
    }

    fn background(&self) -> usize {
        0
    }

    fn foreground(&self, _call: Foreground) {}

    fn running(&self) -> Vec<crate::JobId> {
        Vec::new()
    }

    fn stop_delegates(&self) -> usize {
        0
    }

    fn deliver_to(&self, _inbox: std::sync::mpsc::Sender<crate::inbox::Delivery>) {}
}

#[test]
fn default_write_reports_not_running() {
    let jobs = StubJobs;
    assert!(
        matches!(
            jobs.write(&crate::JobId("j_1".into()), "x"),
            Err(WriteError::NotRunning)
        ),
        "default write did not report the job as not running"
    );
}

#[test]
fn write_error_codes_match_the_job_input_command() {
    let cases: Vec<(WriteError, ErrorCode)> = vec![
        (WriteError::NotRunning, ErrorCode::StaleRequest),
        (
            WriteError::Ended(Outcome::Completed),
            ErrorCode::StaleRequest,
        ),
        (WriteError::Ended(Outcome::Failed), ErrorCode::StaleRequest),
        (
            WriteError::Ended(Outcome::Cancelled),
            ErrorCode::StaleRequest,
        ),
        (WriteError::NotTty, ErrorCode::InvalidArguments),
        (
            WriteError::Io(std::io::Error::other("refused")),
            ErrorCode::IoFailed,
        ),
    ];
    for (error, code) in &cases {
        assert_eq!(error.code(), *code);
    }
}

#[test]
fn stop_error_codes_match_the_job_stop_command() {
    let cases: Vec<(StopError, ErrorCode)> = vec![
        (StopError::NotRunning, ErrorCode::StaleRequest),
        (
            StopError::Ended(Outcome::Completed),
            ErrorCode::StaleRequest,
        ),
        (StopError::Ended(Outcome::Failed), ErrorCode::StaleRequest),
        (
            StopError::Ended(Outcome::Cancelled),
            ErrorCode::StaleRequest,
        ),
    ];
    for (error, code) in &cases {
        assert_eq!(error.code(), *code);
    }
}

#[test]
fn open_error_display_writes_the_path_and_the_source() {
    // The struct writes the same text the old Io variant wrote.
    let error = OpenError {
        path: std::path::PathBuf::from("jobs/j_abc.log"),
        source: std::io::Error::other("denied"),
    };
    assert_eq!(
        error.to_string(),
        "the job's output file jobs/j_abc.log could not be created: denied"
    );
}
