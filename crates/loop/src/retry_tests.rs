//! Unit tests of `Retry::decide`: retryable failures, backoff and attempt limit
//! (`docs/model-routing.md`, "When a model call fails").

use std::time::Duration;

use contract::ErrorCode;
use contract::shapes::Failure;

use super::{Decision, Retry};

fn failure(code: ErrorCode) -> Failure {
    Failure {
        code,
        message: "The call failed.".to_owned(),
        retry_after: None,
        provider: None,
    }
}

fn failed_after(code: ErrorCode, retry_after: Option<f64>) -> Failure {
    Failure {
        code,
        message: "The call failed.".to_owned(),
        retry_after,
        provider: None,
    }
}

fn retry_ms(decision: Decision) -> u64 {
    match decision {
        Decision::Retry(delay) => u64::try_from(delay.as_millis()).unwrap(),
        Decision::Fail(failure) => panic!("expected a retry, failed with {:?}", failure.code),
    }
}

fn fail_code(decision: Decision) -> Failure {
    match decision {
        Decision::Fail(failure) => failure,
        Decision::Retry(delay) => panic!("expected a failure, retrying in {delay:?}"),
    }
}

#[test]
fn the_defaults_are_3_retries_backing_off_2s_4s_8s_capped_at_60s() {
    let retry = Retry::default();
    assert_eq!(retry.attempts, 3);
    assert_eq!(retry.initial, Duration::from_millis(2000));
    assert_eq!(retry.max, Duration::from_millis(60000));
}

#[test]
fn each_retried_code_retries() {
    let retry = Retry::default();
    for code in [
        ErrorCode::RateLimited,
        ErrorCode::ProviderUnavailable,
        ErrorCode::ConnectionFailed,
        ErrorCode::StreamIncomplete,
    ] {
        assert!(
            matches!(
                retry.decide(&failure(code.clone()), None, 0),
                Decision::Retry(_)
            ),
            "{code:?} retries"
        );
    }
}

#[test]
fn each_never_retried_code_fails_unchanged() {
    let retry = Retry::default();
    for code in [
        ErrorCode::QuotaExceeded,
        ErrorCode::AuthenticationFailed,
        ErrorCode::ContextOverflow,
        ErrorCode::Refused,
        ErrorCode::ModelNotFound,
        ErrorCode::InvalidRequest,
        ErrorCode::UnknownStopReason,
    ] {
        let failed = fail_code(retry.decide(&failure(code.clone()), None, 0));
        assert_eq!(failed.code, code, "{code:?} fails");
        assert_eq!(failed.retry_after, None);
    }
}

#[test]
fn should_retry_true_retries_a_never_code_but_not_the_two_exceptions() {
    let retry = Retry::default();
    assert!(
        matches!(
            retry.decide(&failure(ErrorCode::InvalidRequest), Some(true), 0),
            Decision::Retry(_)
        ),
        "true retries invalid_request"
    );
    // `docs/errors.md` names only these two exceptions: `context_overflow`
    // with `true` retries, under the overflow rule's header.
    assert!(
        matches!(
            retry.decide(&failure(ErrorCode::ContextOverflow), Some(true), 0),
            Decision::Retry(_)
        ),
        "true retries context_overflow"
    );
    for code in [ErrorCode::QuotaExceeded, ErrorCode::UnknownStopReason] {
        let failed = fail_code(retry.decide(&failure(code.clone()), Some(true), 0));
        assert_eq!(
            failed.code, code,
            "{code:?} never retries, whatever the header"
        );
    }
}

#[test]
fn should_retry_false_stops_a_retried_code() {
    let retry = Retry::default();
    for code in [
        ErrorCode::RateLimited,
        ErrorCode::ProviderUnavailable,
        ErrorCode::ConnectionFailed,
        ErrorCode::StreamIncomplete,
    ] {
        let failed = fail_code(retry.decide(&failure(code.clone()), Some(false), 0));
        assert_eq!(failed.code, code, "{code:?} stops on false");
    }
}

#[test]
fn the_backoff_doubles_then_the_attempts_run_out() {
    let retry = Retry::default();
    let rate_limited = || failure(ErrorCode::RateLimited);
    assert_eq!(retry_ms(retry.decide(&rate_limited(), None, 0)), 2000);
    assert_eq!(retry_ms(retry.decide(&rate_limited(), None, 1)), 4000);
    assert_eq!(retry_ms(retry.decide(&rate_limited(), None, 2)), 8000);
    let failed = fail_code(retry.decide(&rate_limited(), None, 3));
    assert_eq!(failed, rate_limited(), "the last failure is unchanged");
}

#[test]
fn the_cap_clamps_the_doubling() {
    let retry = Retry {
        attempts: 10,
        initial: Duration::from_secs(50),
        max: Duration::from_secs(60),
    };
    assert_eq!(
        retry_ms(retry.decide(&failure(ErrorCode::RateLimited), None, 0)),
        50_000
    );
    assert_eq!(
        retry_ms(retry.decide(&failure(ErrorCode::RateLimited), None, 1)),
        60_000
    );
    assert_eq!(
        retry_ms(retry.decide(&failure(ErrorCode::RateLimited), None, 5)),
        60_000
    );
}

#[test]
fn an_asked_wait_within_the_cap_waits_the_larger() {
    let retry = Retry::default();
    assert_eq!(
        retry_ms(retry.decide(&failed_after(ErrorCode::RateLimited, Some(5.0)), None, 0)),
        5000,
        "5s asked beats the 2s backoff"
    );
    assert_eq!(
        retry_ms(retry.decide(&failed_after(ErrorCode::RateLimited, Some(1.0)), None, 0)),
        2000,
        "the 2s backoff beats 1s asked"
    );
}

#[test]
fn an_asked_wait_over_the_cap_fails_at_once_as_rate_limited() {
    let retry = Retry::default();
    for code in [
        ErrorCode::RateLimited,
        ErrorCode::ProviderUnavailable,
        ErrorCode::ConnectionFailed,
    ] {
        let failed = fail_code(retry.decide(&failed_after(code, Some(61.0)), None, 0));
        assert_eq!(failed.code, ErrorCode::RateLimited);
        assert_eq!(failed.retry_after, Some(61.0));
    }
}

#[test]
fn an_over_cap_wait_keeps_a_never_retried_code() {
    let retry = Retry::default();
    let failed =
        fail_code(retry.decide(&failed_after(ErrorCode::QuotaExceeded, Some(61.0)), None, 0));
    assert_eq!(failed.code, ErrorCode::QuotaExceeded);
    assert_eq!(failed.retry_after, Some(61.0));
}

#[test]
fn an_asked_wait_exactly_at_the_cap_retries() {
    let retry = Retry::default();
    assert_eq!(
        retry_ms(retry.decide(&failed_after(ErrorCode::RateLimited, Some(60.0)), None, 0)),
        60_000
    );
}

#[test]
fn no_attempts_means_no_retries() {
    let retry = Retry {
        attempts: 0,
        ..Retry::default()
    };
    let failed = fail_code(retry.decide(&failure(ErrorCode::RateLimited), None, 0));
    assert_eq!(failed.code, ErrorCode::RateLimited);
}

#[test]
fn huge_values_do_not_overflow() {
    let retry = Retry {
        attempts: u32::MAX,
        initial: Duration::MAX,
        max: Duration::MAX,
    };
    assert!(matches!(
        retry.decide(&failure(ErrorCode::RateLimited), None, 0),
        Decision::Retry(_)
    ));
    assert!(matches!(
        retry.decide(&failure(ErrorCode::RateLimited), None, u32::MAX - 1),
        Decision::Retry(_)
    ));
    assert!(matches!(
        retry.decide(&failure(ErrorCode::RateLimited), None, u32::MAX),
        Decision::Fail(_)
    ));
}

#[test]
fn an_unusable_asked_wait_is_absent() {
    let retry = Retry::default();
    for asked in [f64::NAN, f64::INFINITY, -1.0] {
        assert_eq!(
            retry_ms(retry.decide(&failed_after(ErrorCode::RateLimited, Some(asked)), None, 0)),
            2000,
            "asked {asked} is absent, so the backoff applies"
        );
    }
}
