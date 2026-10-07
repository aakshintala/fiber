use std::panic::AssertUnwindSafe;
use std::sync::mpsc;
use std::time::Duration;

use super::*;

fn message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_owned()
    } else {
        "<not a string>".to_owned()
    }
}

#[test]
fn returns_the_value() {
    let value = within("a value", Duration::from_secs(10), || 41 + 1);
    assert_eq!(value, 42);
}

#[test]
fn a_blocked_worker_fails_naming_the_wait() {
    let (tx, rx) = mpsc::channel::<()>();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within("a stuck line", Duration::from_millis(100), move || {
            rx.recv().unwrap();
        })
    }));
    drop(tx);
    let err = result.expect_err("a blocked worker fails");
    let text = message(err);
    assert!(text.contains("waited"), "names the wait: {text}");
    assert!(text.contains("a stuck line"), "names what: {text}");
}

#[test]
fn a_str_panic_reports_the_payload() {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within("a turn line", Duration::from_secs(10), || {
            panic!("boom");
        })
    }));
    let text = message(result.expect_err("a panic fails"));
    assert!(text.contains("the wait for a turn line panicked"), "{text}");
    assert!(text.contains("boom"), "{text}");
}

#[test]
fn a_string_panic_reports_the_payload() {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within("a turn line", Duration::from_secs(10), || {
            std::panic::panic_any(String::from("boom 1"));
        })
    }));
    let text = message(result.expect_err("a panic fails"));
    assert!(text.contains("the wait for a turn line panicked"), "{text}");
    assert!(text.contains("boom 1"), "{text}");
}

#[test]
fn a_non_string_panic_reports_a_fixed_line() {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within("a turn line", Duration::from_secs(10), || {
            std::panic::panic_any(7);
        })
    }));
    let text = message(result.expect_err("a panic fails"));
    assert!(text.contains("the wait for a turn line panicked"), "{text}");
    assert!(text.contains("a non-string panic"), "{text}");
}
