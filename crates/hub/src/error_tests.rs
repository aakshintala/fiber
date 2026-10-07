//! Tests for the hub's start errors: each case's code and sentence.

use std::io;
use std::path::PathBuf;

use super::*;

#[test]
fn a_too_long_home_is_usage_naming_the_variable() {
    let error = StartError::HomeTooLong { max: 103 };
    assert_eq!(error.code(), ErrorCode::Usage);
    assert_eq!(
        error.to_string(),
        "FIBER_HOME is too long for the hub's socket path, which must fit in 103 bytes."
    );
}

#[test]
fn an_io_failure_is_io_failed_naming_the_path() {
    let error = StartError::Io {
        path: PathBuf::from("/h/run"),
        source: io::Error::other("refused"),
    };
    assert_eq!(error.code(), ErrorCode::IoFailed);
    assert_eq!(error.to_string(), "/h/run: refused");
}

#[test]
fn a_configure_failure_keeps_its_code_and_message() {
    let error = StartError::Configure(Failure {
        code: ErrorCode::ConfigInvalid,
        message: "config.json is not valid JSON.".to_owned(),
        retry_after: None,
        provider: None,
    });
    assert_eq!(error.code(), ErrorCode::ConfigInvalid);
    assert_eq!(error.to_string(), "config.json is not valid JSON.");
}
