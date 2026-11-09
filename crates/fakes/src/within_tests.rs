use std::panic::AssertUnwindSafe;
use std::sync::mpsc;
use std::time::Duration;

use super::*;
use crate::deadline::Deadline;

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
    let value = within("a value", Deadline::after(Duration::from_secs(10)), || {
        41 + 1
    });
    assert_eq!(value, 42);
}

#[test]
fn a_blocked_worker_fails_naming_the_wait() {
    let (tx, rx) = mpsc::channel::<()>();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within(
            "a stuck line",
            Deadline::after(Duration::from_millis(100)),
            move || {
                rx.recv().unwrap();
            },
        )
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
        within(
            "a turn line",
            Deadline::after(Duration::from_secs(10)),
            || {
                panic!("boom");
            },
        )
    }));
    let text = message(result.expect_err("a panic fails"));
    assert!(text.contains("the wait for a turn line panicked"), "{text}");
    assert!(text.contains("boom"), "{text}");
}

#[test]
fn a_string_panic_reports_the_payload() {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within(
            "a turn line",
            Deadline::after(Duration::from_secs(10)),
            || {
                std::panic::panic_any(String::from("boom 1"));
            },
        )
    }));
    let text = message(result.expect_err("a panic fails"));
    assert!(text.contains("the wait for a turn line panicked"), "{text}");
    assert!(text.contains("boom 1"), "{text}");
}

#[test]
fn a_non_string_panic_reports_a_fixed_line() {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within(
            "a turn line",
            Deadline::after(Duration::from_secs(10)),
            || {
                std::panic::panic_any(7);
            },
        )
    }));
    let text = message(result.expect_err("a panic fails"));
    assert!(text.contains("the wait for a turn line panicked"), "{text}");
    assert!(text.contains("a non-string panic"), "{text}");
}

#[test]
fn the_outer_bound_is_twice_the_floor() {
    assert_eq!(
        WHOLE_SUBSYSTEM_WITHIN,
        Duration::from_secs(2 * MUST_SUCCEED_WITHIN.as_secs())
    );
    assert!(WHOLE_SUBSYSTEM_WITHIN > MUST_SUCCEED_WITHIN);
}

#[test]
fn equal_floor_budgets_report_the_outer_step() {
    let clock: &'static std::sync::Arc<crate::clock::FakeClock> =
        Box::leak(Box::new(crate::clock::FakeClock::new()));
    let root = Deadline::on(&**clock);
    let outer_deadline = root.within(MUST_SUCCEED_WITHIN);
    let inner_deadline = root.within(MUST_SUCCEED_WITHIN);
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(MUST_SUCCEED_WITHIN);
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let (hold_tx, hold_rx) = mpsc::channel::<()>();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within("outer", outer_deadline, move || {
            go_rx.recv().unwrap();
            within("inner", inner_deadline, move || {
                hold_rx.recv().unwrap();
            })
        })
    }));
    go_tx.send(()).unwrap();
    let err = result.expect_err("equal budgets time out on the outer wait");
    let text = message(err);
    assert!(text.contains("waited"), "names the wait: {text}");
    assert!(text.contains("outer"), "names the outer step: {text}");
    assert!(
        !text.contains("inner"),
        "does not name the inner step: {text}"
    );
    drop(hold_tx);
}

#[test]
fn a_doubled_outer_reports_the_inner_step() {
    let clock: &'static std::sync::Arc<crate::clock::FakeClock> =
        Box::leak(Box::new(crate::clock::FakeClock::new()));
    let root = Deadline::on(&**clock);
    let outer_deadline = root.within(WHOLE_SUBSYSTEM_WITHIN);
    let inner_deadline = root.within(MUST_SUCCEED_WITHIN);
    #[allow(
        clippy::disallowed_methods,
        reason = "no code under test waits on the clock (ruling 6179406086)"
    )]
    clock.advance(MUST_SUCCEED_WITHIN);
    let (hold_tx, hold_rx) = mpsc::channel::<()>();
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        within("outer", outer_deadline, move || {
            within("inner", inner_deadline, move || {
                hold_rx.recv().unwrap();
            })
        })
    }));
    let err = result.expect_err("doubled outer reports the inner step");
    let text = message(err);
    assert!(
        text.contains("the wait for outer panicked"),
        "carries the outer wait: {text}"
    );
    assert!(text.contains("for inner"), "names the inner step: {text}");
    assert!(
        !text.trim_end().ends_with("for outer"),
        "does not end at the outer wait: {text}"
    );
    drop(hold_tx);
}
