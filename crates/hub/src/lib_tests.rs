//! Tests for the hub's signal arm: a raised signal reaches the flag the
//! idle wait reads.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use super::*;

const WITHIN: Duration = Duration::from_secs(10);

#[test]
fn arm_records_a_signal_raised_at_the_process() {
    let got = Arc::new(AtomicI32::new(0));
    arm(&got);
    signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
    // The handler thread records the signal: a wait, so the test receives
    // it with a wall-clock deadline.
    let (done_tx, done_rx) = mpsc::channel();
    thread::Builder::new()
        .name("hub-test-arm".to_owned())
        .spawn({
            let got = Arc::clone(&got);
            move || {
                while got.load(Ordering::SeqCst) != signal_hook::consts::SIGTERM {
                    thread::yield_now();
                }
                done_tx.send(()).unwrap_or(());
            }
        })
        .unwrap();
    done_rx
        .recv_timeout(WITHIN)
        .expect("the arm records SIGTERM before its deadline");
}
