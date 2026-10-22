#![allow(clippy::unwrap_used, reason = "test code; a failure is the test's")]

use std::cell::RefCell;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::ErrorCode;
use contract::shapes::Failure;
use fakes::Deadline;

/// The deadline of each wait in these tests.
const DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn shutdown_publishes_before_releasing_callbacks() {
    let cancel = r#loop::TurnCancel::default();
    let order = RefCell::new(Vec::new());
    let observed = RefCell::new(None);
    super::shutdown_steps(
        143,
        |code| {
            cancel.shutdown(code);
            order.borrow_mut().push("publish");
        },
        || order.borrow_mut().push("reads"),
        || {
            *observed.borrow_mut() = cancel.shutdown_code();
            order.borrow_mut().push("callbacks");
        },
        || order.borrow_mut().push("rest"),
    );
    assert_eq!(*observed.borrow(), Some(143));
    assert_eq!(*order.borrow(), ["publish", "reads", "callbacks", "rest"]);
}

/// Stopping the extension callbacks breaks a reload's extension
/// initialisation; the read in flight must already be cancelled by then, so
/// it answers `closing` and not a degraded value.
#[test]
fn shutdown_cancels_reads_before_stopping_callbacks() {
    let reads = Arc::new(crate::switch::Reads::default());
    let (entered_tx, entered) = mpsc::channel::<()>();
    let (open_tx, open) = mpsc::channel::<()>();
    let (done_tx, done) = mpsc::channel::<Result<i32, Failure>>();
    let runner = Arc::clone(&reads);
    let thread = std::thread::spawn(move || {
        let outcome = runner.run("credential read", move || {
            entered_tx.send(()).unwrap_or(());
            open.recv().unwrap_or(());
            7
        });
        done_tx.send(outcome).unwrap_or(());
    });
    Deadline::after(DEADLINE).recv_or_fail(&entered, "the read to start");
    super::shutdown_steps(
        1,
        |_| {},
        || reads.cancel(),
        || {
            open_tx.send(()).unwrap_or(());
            let outcome = Deadline::after(DEADLINE).recv_or_fail(&done, "the run to return");
            assert_eq!(
                outcome.map_err(|failure| failure.code),
                Err(ErrorCode::Closing)
            );
        },
        || {},
    );
    thread.join().unwrap();
}
