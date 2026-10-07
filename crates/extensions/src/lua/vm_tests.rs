#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use super::pending_error;
use crate::host::failure::{Boundary, PendingFailure};
use crate::{Error, ErrorCode};

fn pending(message: &str, boundary: Boundary) -> PendingFailure {
    PendingFailure {
        text: message.to_owned(),
        message: message.to_owned(),
        boundary,
    }
}

#[test]
fn unattended_oauth_keeps_authentication_failed_when_uncaught() {
    let error = pending_error(
        "acme",
        pending(
            "host.oauth.poll needs a person to log in, and nobody is attached",
            Boundary::Unattended {
                call: "poll".to_owned(),
            },
        ),
    );
    assert!(matches!(&error, Error::Unattended { call, .. } if call == "poll"));
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
}

#[test]
fn a_reached_refresh_keeps_authentication_failed_when_uncaught() {
    let error = pending_error(
        "acme",
        pending("offline", Boundary::Refresh { reached: true }),
    );
    assert!(matches!(&error, Error::RefreshRejected { message, .. } if message == "offline"));
    assert_eq!(error.code(), ErrorCode::AuthenticationFailed);
}

#[test]
fn an_unreached_refresh_keeps_connection_failed_when_uncaught() {
    let error = pending_error(
        "acme",
        pending("host.http: offline", Boundary::Refresh { reached: false }),
    );
    assert!(matches!(&error, Error::RefreshUnreachable { .. }));
    assert_eq!(error.code(), ErrorCode::ConnectionFailed);
}
