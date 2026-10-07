//! One deadline for a blocking call (`docs/testing.md`, "Waits and timeouts").

use std::time::Duration;

/// Runs `work` on its own thread and returns its value. Fails the test
/// naming `what` when `work` panics, and when it has not returned within
/// `deadline`. Never joins the thread.
#[allow(clippy::panic, reason = "a test helper; a failure is the test's")]
pub fn within<T: Send + 'static>(
    what: &str,
    deadline: Duration,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("fakes-within".to_owned())
        .spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work));
            done.send(outcome).unwrap_or(());
        })
        .unwrap_or_else(|err| panic!("could not start a thread for {what}: {err}"));
    match finished.recv_timeout(deadline) {
        Ok(Ok(value)) => value,
        Ok(Err(payload)) => panic!("the wait for {what} panicked: {}", describe(&payload)),
        Err(_) => panic!("waited {deadline:?} for {what}"),
    }
}

/// The payload's text, or a fixed line when it is not a string.
fn describe(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "a non-string panic".to_owned()
    }
}

#[cfg(test)]
#[path = "within_tests.rs"]
mod tests;
