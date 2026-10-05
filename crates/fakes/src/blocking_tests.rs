//! Tests for [`BlockingProvider`](super::BlockingProvider): the first call
//! blocks until cancelled, later ones answer at once.

use std::sync::PoisonError;
use std::time::Duration;

use contract::events::CacheLifetime;
use contract::provider::{CallError, Delta, ModelCall, ModelRequest, Provider};

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
        session_dir: std::path::PathBuf::new(),
    }
}

/// Cancels the call when dropped, so a failed `wait_started` still ends
/// the blocked call instead of waiting out its bound.
struct CancelOnDrop<'a> {
    call: &'a dyn ModelCall,
}

impl Drop for CancelOnDrop<'_> {
    fn drop(&mut self) {
        self.call.cancel();
    }
}

#[test]
fn the_first_call_blocks_until_its_cancel() {
    let provider = BlockingProvider::default();
    let call = provider.call(&request());
    std::thread::scope(|scope| {
        let run = scope.spawn(|| {
            let mut drops = 0;
            let ended = call.run(&mut |_: Delta| {
                drops += 1;
            });
            (ended, drops)
        });
        // If `wait_started` fails, the guard still cancels, so the test
        // fails with its message instead of hanging on the call's bound.
        let _guard = CancelOnDrop { call: &*call };
        provider.wait_started(WAIT);
        call.cancel();
        let (ended, drops) = run.join().unwrap();
        assert!(matches!(ended, Err(CallError::Cancelled)));
        assert_eq!(drops, 0);
    });
}

#[test]
fn a_cancel_before_the_call_starts_ends_it_at_once() {
    let provider = BlockingProvider::default();
    let call = provider.call(&request());
    call.cancel();
    // The cancel arrived before `run` waited: it stays buffered, so this
    // fails at once when the cancel never arrives instead of waiting out
    // the call's bound.
    assert!(
        provider
            .inner
            .cancel_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_recv()
            .is_ok()
    );
    call.cancel();
    let mut drops = 0;
    let ended = call.run(&mut |_: Delta| {
        drops += 1;
    });
    assert!(matches!(ended, Err(CallError::Cancelled)));
    assert_eq!(drops, 0);
    // `run` signalled it started to block.
    assert!(
        provider
            .inner
            .started_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_recv()
            .is_ok()
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
