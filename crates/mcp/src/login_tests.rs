//! `login`: the error codes, the redaction, and the flow against a routed
//! fake server and a fake clock.

#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test code; a failure is the test's"
)]

use super::*;

#[test]
fn string_bearing_variants_never_print_their_string() {
    for error in [
        LoginError::Unreachable("secret".to_owned()),
        LoginError::Discovery("secret".to_owned()),
        LoginError::Registration("secret".to_owned()),
        LoginError::Exchange("secret".to_owned()),
        LoginError::Callback("secret".to_owned()),
        LoginError::Denied("secret".to_owned()),
    ] {
        let debug = format!("{error:?}");
        assert!(!debug.contains("secret"), "debug leaks: {debug}");
    }
}

#[test]
fn each_login_error_maps_to_its_stable_code() {
    use crate::token::TokenError;
    use contract::mcp_tokens::TokenFileError;
    for (error, code) in [
        (LoginError::Url, ErrorCode::ConfigInvalid),
        (LoginError::Userinfo, ErrorCode::ConfigInvalid),
        (
            LoginError::Unreachable("step".to_owned()),
            ErrorCode::ConnectionFailed,
        ),
        (
            LoginError::Discovery("step".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (
            LoginError::Registration("step".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (
            LoginError::Exchange("step".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (
            LoginError::Callback("step".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (
            LoginError::Denied("access_denied".to_owned()),
            ErrorCode::AuthenticationFailed,
        ),
        (LoginError::PkceUnsupported, ErrorCode::ConfigInvalid),
        (LoginError::Timeout, ErrorCode::AuthenticationFailed),
        (
            LoginError::Store(TokenError::Url),
            ErrorCode::InvalidArguments,
        ),
        (
            LoginError::Store(TokenError::Busy),
            ErrorCode::IoFailed,
        ),
        (
            LoginError::Store(TokenError::Files(TokenFileError::Unsynced("path".into()))),
            ErrorCode::IoFailed,
        ),
    ] {
        assert_eq!(error.code(), code, "{error:?}");
    }
}

#[test]
fn pkce_unsupported_names_no_code() {
    let error = LoginError::PkceUnsupported;
    assert_eq!(
        error.to_string(),
        "its authorization server does not offer PKCE with S256"
    );
}
