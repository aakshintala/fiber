use super::Error;
use contract::ErrorCode;
use std::io;

#[test]
fn every_hub_failure_has_its_stable_code() {
    let io_error = || io::Error::other("test failure");
    for (error, code) in [
        (Error::UnknownHub { name: "box".into() }, ErrorCode::Usage),
        (
            Error::BadName {
                name: ".box".into(),
            },
            ErrorCode::Usage,
        ),
        (
            Error::BadAddress {
                why: "grammar".into(),
            },
            ErrorCode::Usage,
        ),
        (
            Error::StoredAddress {
                key: "hubs.box.address".into(),
                why: "grammar".into(),
            },
            ErrorCode::ConfigInvalid,
        ),
        (
            Error::MissingDefault { name: "box".into() },
            ErrorCode::ConfigInvalid,
        ),
        (
            Error::MissingToken { name: "box".into() },
            ErrorCode::CredentialMissing,
        ),
        (
            Error::Config(config::ConfigError::Override { arg: "bad".into() }),
            ErrorCode::Usage,
        ),
        (
            Error::Unreachable {
                hub: "box".into(),
                source: net::Error::Accept,
            },
            ErrorCode::ConnectionFailed,
        ),
        (
            Error::Unix {
                hub: "box".into(),
                path: "/tmp/socket".into(),
                source: io_error(),
            },
            ErrorCode::ConnectionFailed,
        ),
        (
            Error::Hello {
                hub: "box".into(),
                source: io_error(),
            },
            ErrorCode::ConnectionFailed,
        ),
        (
            Error::Refused {
                hub: "box".into(),
                message: "refused".into(),
            },
            ErrorCode::AuthenticationFailed,
        ),
        (Error::Local(io_error()), ErrorCode::IoFailed),
    ] {
        assert_eq!(error.code(), code, "{error}");
    }
}
