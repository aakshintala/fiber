//! One row per [`StartError`](super::StartError) case: its code and its
//! sentence. The forwarding case holds two different codes, so a
//! hard-coded code fails here.

use std::path::PathBuf;

use contract::ErrorCode;
use contract::shapes::Failure;

use super::StartError;

fn failure(code: ErrorCode, message: &str) -> Failure {
    Failure {
        code,
        message: message.to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

fn io(path: &str, why: &str) -> StartError {
    StartError::Io {
        path: PathBuf::from(path),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, why.to_owned()),
    }
}

#[test]
fn each_start_error_maps_to_its_code_sentence_and_record() {
    let rows: Vec<(&str, StartError, ErrorCode, String)> = vec![
        (
            "home too long",
            StartError::HomeTooLong { max: 104 },
            ErrorCode::Usage,
            "FIBER_HOME is too long for the hub's socket path, which must fit in 104 bytes."
                .to_owned(),
        ),
        (
            "io",
            io("/x/run", "missing"),
            ErrorCode::IoFailed,
            "/x/run: missing".to_owned(),
        ),
        (
            "port",
            StartError::Port {
                port: 8080,
                source: std::io::Error::new(std::io::ErrorKind::AddrInUse, "taken".to_owned()),
            },
            ErrorCode::IoFailed,
            "127.0.0.1:8080: taken".to_owned(),
        ),
        (
            "configure forwards authentication failed",
            StartError::Configure(failure(ErrorCode::AuthenticationFailed, "denied")),
            ErrorCode::AuthenticationFailed,
            "denied".to_owned(),
        ),
        (
            "configure forwards config invalid",
            StartError::Configure(failure(ErrorCode::ConfigInvalid, "bad key")),
            ErrorCode::ConfigInvalid,
            "bad key".to_owned(),
        ),
    ];
    for (name, error, code, message) in &rows {
        assert_eq!(&error.code(), code, "code, row: {name}");
        assert_eq!(&error.to_string(), message, "message, row: {name}");
        let record = Failure::from(error);
        assert_eq!(&record.code, code, "record code, row: {name}");
        assert_eq!(&record.message, message, "record message, row: {name}");
        assert_eq!(record.retry_after_ms, None, "record retry, row: {name}");
        assert_eq!(record.provider, None, "record provider, row: {name}");
    }
    let forwarded = failure(ErrorCode::AuthenticationFailed, "denied");
    let configure = StartError::Configure(forwarded);
    assert_ne!(configure.code(), ErrorCode::Usage, "a forwarded code is not usage");
    assert_ne!(
        configure.code(),
        ErrorCode::IoFailed,
        "a forwarded code is not io failed"
    );
}
