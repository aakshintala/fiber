//! Tests for [`BlockingProvider`](super::BlockingProvider): the first call
//! blocks until cancelled, later ones answer at once.

use std::sync::PoisonError;
use std::time::Duration;

use contract::events::CacheLifetime;
use contract::provider::{CallError, Delta, ModelRequest, Provider};

use super::BlockingProvider;

/// How long a wait that must succeed may take. Every wait below ends as
/// soon as its signal arrives; the deadline only reports a missed one.
const WAIT: Duration = Duration::from_secs(10);

fn request() -> ModelRequest {
    ModelRequest {
        system_prompt: String::new(),
        tools: Vec::new(),
        effort: None,
        tool_choice: "auto".to_owned(),
        cache_lifetime: CacheLifetime::OneHour,
        cache_key: "s_test".to_owned(),
        conversation: Vec::new(),
        previous_end: None,
        max_output_tokens: None,
    }
}

#[test]
fn the_first_call_blocks_until_its_cancel() {
    let provider = BlockingProvider::default();
    let call = provider.call(&request());
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut drops = 0;
            let ended = call.run(&mut |_: Delta| {
                drops += 1;
            });
            assert!(matches!(ended, Err(CallError::Cancelled)));
            assert_eq!(drops, 0);
        });
        provider.wait_started(WAIT);
        call.cancel();
    });
}

#[test]
fn cancel_sets_the_flag_the_blocked_call_waits_on() {
    let provider = BlockingProvider::default();
    let call = provider.call(&request());
    call.cancel();
    assert!(
        provider
            .inner
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled
    );
}

#[test]
fn later_calls_answer_at_once() {
    let provider = BlockingProvider::default();
    let _first = provider.call(&request());
    let later = provider.call(&request());
    let ended = later.run(&mut |_| {});
    assert_eq!(ended.unwrap().text(), "After.");
}

#[test]
#[should_panic(expected = "timed out waiting for the model call to start")]
fn wait_started_fails_when_no_call_starts() {
    // Nothing ever calls this provider, so the deadline always expires:
    // the wait is the assertion.
    BlockingProvider::default().wait_started(Duration::from_millis(100));
}
