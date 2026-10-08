//! Unit tests of `Retry::decide`: retryable failures, backoff and attempt limit
//! (`docs/model-routing.md`, "When a model call fails").

use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use crate::{Loop, Model};
use contract::ErrorCode;
use contract::inbox::{Ack, Delivery, Message};
use contract::rules::{Rules, RulesError, StandingRules};
use contract::shapes::{ContentPart, Failure, Origin, Sender};
use contract::{CommandId, SessionId};
use fakes::clock::FakeClock;
use fakes::{Scripted, ScriptedProvider};
use log::Log;

use super::{Decision, Retry};

fn failure(code: ErrorCode) -> Failure {
    Failure {
        code,
        message: "The call failed.".to_owned(),
        retry_after_ms: None,
        provider: None,
    }
}

fn failed_after(code: ErrorCode, retry_after_ms: Option<u64>) -> Failure {
    Failure {
        code,
        message: "The call failed.".to_owned(),
        retry_after_ms,
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
        assert_eq!(failed.retry_after_ms, None);
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
        retry_ms(retry.decide(&failed_after(ErrorCode::RateLimited, Some(5000)), None, 0)),
        5000,
        "5s asked beats the 2s backoff"
    );
    assert_eq!(
        retry_ms(retry.decide(&failed_after(ErrorCode::RateLimited, Some(1000)), None, 0)),
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
        let failed = fail_code(retry.decide(&failed_after(code, Some(61_000)), None, 0));
        assert_eq!(failed.code, ErrorCode::RateLimited);
        assert_eq!(failed.retry_after_ms, Some(61_000));
    }
}

#[test]
fn an_over_cap_wait_keeps_a_never_retried_code() {
    let retry = Retry::default();
    let failed = fail_code(retry.decide(
        &failed_after(ErrorCode::QuotaExceeded, Some(61_000)),
        None,
        0,
    ));
    assert_eq!(failed.code, ErrorCode::QuotaExceeded);
    assert_eq!(failed.retry_after_ms, Some(61_000));
}

#[test]
fn an_asked_wait_exactly_at_the_cap_retries() {
    let retry = Retry::default();
    assert_eq!(
        retry_ms(retry.decide(&failed_after(ErrorCode::RateLimited, Some(60_000)), None, 0)),
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
fn a_saturated_asked_wait_fails_at_once_whatever_the_cap() {
    for max in [Duration::from_millis(u64::MAX), Retry::default().max] {
        let retry = Retry {
            max,
            ..Retry::default()
        };
        let failed = fail_code(retry.decide(
            &failed_after(ErrorCode::RateLimited, Some(u64::MAX)),
            None,
            0,
        ));
        assert_eq!(failed.code, ErrorCode::RateLimited);
        assert_eq!(failed.retry_after_ms, Some(u64::MAX));
    }
    let retry = Retry {
        max: Duration::from_millis(u64::MAX),
        ..Retry::default()
    };
    assert_eq!(
        retry_ms(retry.decide(
            &failed_after(ErrorCode::RateLimited, Some(u64::MAX - 1)),
            None,
            0
        )),
        u64::MAX - 1,
        "only the saturated value counts as over every cap"
    );
}

#[test]
fn advancing_on_retry_scheduled_does_not_stretch_the_retry_deadline() {
    const DEADLINE: Duration = Duration::from_secs(5);
    const DELAY: Duration = Duration::from_secs(2);
    // A full advance after the line releases the retry with no further
    // advance: the deadline was fixed before the line was appended.
    let mut full = RetryRun::start();
    full.wait_for_retry_scheduled(DEADLINE);
    full.clock.advance(DELAY);
    full.wait_for_retry_started(DEADLINE);
    full.finish();

    // A smaller advance leaves the retry parked at its fixed deadline, and
    // the rest releases it.
    let mut partial = RetryRun::start();
    partial.wait_for_retry_scheduled(DEADLINE);
    partial.clock.advance(Duration::from_secs(1));
    assert!(
        partial.clock.await_parked(partial.origin + DELAY, DEADLINE),
        "the retry stays parked at its fixed deadline"
    );
    assert_eq!(
        partial.provider.requests().len(),
        1,
        "a smaller advance releases no retry"
    );
    partial.clock.advance(Duration::from_secs(1));
    partial.wait_for_retry_started(DEADLINE);
    partial.finish();
}

/// One failing-then-recovering turn on a fake clock: the loop runs on its
/// own thread and the test watches its log. The `retry_scheduled` line the
/// loop already emits is the signal each advance waits on (`docs/testing.md`,
/// "Waits and timeouts").
struct RetryRun {
    _home: fakes::TempDir,
    origin: std::time::Instant,
    clock: Arc<FakeClock>,
    provider: Arc<ScriptedProvider>,
    watcher: log::Watcher,
    starts: usize,
    turn: thread::JoinHandle<Result<Option<contract::events::TurnOutcome>, crate::Error>>,
}

impl RetryRun {
    /// Starts a turn whose first call fails retryably and whose retry
    /// succeeds, and returns before the loop waits: the test advances the
    /// clock only after `retry_scheduled` is on the log.
    fn start() -> Self {
        let home = fakes::TempDir::new("fiber-retry-deadline");
        let workspace = home.path().join("workspace");
        let credentials = home.path().join("credentials");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&credentials).unwrap();
        let clock = FakeClock::new();
        let origin = clock.origin();
        let clock_for_loop: Arc<dyn contract::clock::Clock> = clock.clone();
        let log = Arc::new(
            Log::create(
                home.path(),
                SessionId("s_retry_deadline".into()),
                clock_for_loop.clone(),
            )
            .unwrap(),
        );
        let (inbox, receiver) = mpsc::channel();
        let provider = Arc::new(ScriptedProvider::new(vec![
            Scripted::failed(Failure {
                code: ErrorCode::RateLimited,
                message: "try again".into(),
                retry_after_ms: None,
                provider: None,
            }),
            Scripted::text("Recovered."),
        ]));
        let rules: Arc<dyn Rules> = Arc::new(NoRules);
        let mut looped = Loop::start(
            Arc::clone(&log),
            Arc::clone(&provider) as Arc<dyn contract::provider::Provider>,
            Model {
                reference: "fake/model".into(),
                cost: None,
                subscription: false,
            },
            crate::prompt::PromptInputs::new(
                home.path().to_path_buf(),
                "/bin/sh".into(),
                home.path()
                    .join("s_retry_deadline/events.jsonl")
                    .display()
                    .to_string(),
                clock_for_loop,
                fakes::CONTEXT_WINDOW,
            ),
            receiver,
            Vec::new(),
            crate::Permissions {
                workspace: workspace.display().to_string(),
                credentials,
                credential_files: Vec::new(),
                rules,
            },
        )
        .unwrap();
        inbox
            .send(Delivery::Prompt(
                Message {
                    content: vec![ContentPart::Text { text: "hi".into() }],
                    sender: Sender {
                        origin: Origin::Driver,
                        command_id: Some(CommandId("c_retry_deadline".into())),
                    },
                },
                Ack(Box::new(|_| {})),
            ))
            .unwrap();
        let watcher = log.watch_all().unwrap();
        let turn = thread::spawn(move || looped.turn());
        Self {
            _home: home,
            origin,
            clock,
            provider,
            watcher,
            starts: 0,
            turn,
        }
    }

    /// Reads log lines until `retry_scheduled` appears: the signal the
    /// clock advance waits on. Each read carries the wall deadline.
    fn wait_for_retry_scheduled(&mut self, within: Duration) {
        loop {
            match self.watcher.recv_timeout(within) {
                Some(Ok(Some(line))) => {
                    let scheduled = line.kind == "retry_scheduled";
                    self.note(&line);
                    if scheduled {
                        return;
                    }
                }
                Some(Ok(None)) => panic!("the session log ended before retry_scheduled"),
                Some(Err(error)) => panic!("reading the session log: {error}"),
                None => panic!("retry_scheduled was not appended within {within:?}"),
            }
        }
    }

    /// Reads log lines until the retry's `assistant_message_started`
    /// appears: the advance released the wait.
    fn wait_for_retry_started(&mut self, within: Duration) {
        while self.starts < 2 {
            match self.watcher.recv_timeout(within) {
                Some(Ok(Some(line))) => self.note(&line),
                Some(Ok(None)) => panic!("the session log ended before the retry started"),
                Some(Err(error)) => panic!("reading the session log: {error}"),
                None => panic!("the retry did not start within {within:?}"),
            }
        }
    }

    /// Joins the turn: it completed on the retry, which sent one request more.
    fn finish(self) {
        assert_eq!(
            self.turn.join().unwrap().unwrap(),
            Some(contract::events::TurnOutcome::Completed)
        );
        assert_eq!(self.provider.requests().len(), 2);
    }

    fn note(&mut self, line: &contract::Envelope) {
        if line.kind == "assistant_message_started" {
            self.starts += 1;
        }
    }
}

struct NoRules;

impl Rules for NoRules {
    fn read(&self) -> Result<StandingRules, RulesError> {
        Ok(StandingRules::default())
    }

    fn remember(&self, _: &str, _: &str, _: &SessionId) -> Result<(), RulesError> {
        Ok(())
    }
}

#[test]
fn a_43_ms_cap_retries_43_ms_and_fails_44_ms() {
    let retry = Retry {
        max: Duration::from_millis(43),
        ..Retry::default()
    };
    assert_eq!(
        retry_ms(retry.decide(&failed_after(ErrorCode::RateLimited, Some(43)), None, 0)),
        43
    );
    let failed = fail_code(retry.decide(&failed_after(ErrorCode::RateLimited, Some(44)), None, 0));
    assert_eq!(failed.code, ErrorCode::RateLimited);
    assert_eq!(failed.retry_after_ms, Some(44));
}
