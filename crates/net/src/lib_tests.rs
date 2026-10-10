use ureq::tls::RootCerts;

use super::{Error, config, tls_config};

#[test]
fn tls_config_uses_the_platform_verifier() {
    assert!(
        matches!(tls_config().root_certs(), RootCerts::PlatformVerifier),
        "the shared TLS config verifies against the platform store"
    );
}

#[test]
fn agent_config_carries_the_platform_verifier() {
    assert!(
        matches!(
            config().build().tls_config().root_certs(),
            RootCerts::PlatformVerifier
        ),
        "the shared agent config verifies against the platform store"
    );
}

#[test]
fn a_stopped_call_reports_connection_failed() {
    assert_eq!(Error::Stopped.code(), contract::ErrorCode::ConnectionFailed);
}
