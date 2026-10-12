use super::*;
use crate::events::Outcome;

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
