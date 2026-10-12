use super::TokenFileError;
use crate::ErrorCode;

#[test]
fn refused_is_io_failed() {
    assert_eq!(
        TokenFileError::Refused("path".into()).code(),
        ErrorCode::IoFailed
    );
}
#[test]
fn unreadable_is_io_failed() {
    assert_eq!(
        TokenFileError::Unreadable("path".into()).code(),
        ErrorCode::IoFailed
    );
}
#[test]
fn unwritable_is_io_failed() {
    assert_eq!(
        TokenFileError::Unwritable("path".into()).code(),
        ErrorCode::IoFailed
    );
}
#[test]
fn unsynced_is_io_failed() {
    assert_eq!(
        TokenFileError::Unsynced("path".into()).code(),
        ErrorCode::IoFailed
    );
}
