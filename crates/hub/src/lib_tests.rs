//! Tests for the hub's signal arm: a raised signal reaches the flag the
//! idle wait reads, then wakes it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test helpers; a failure is the test's"
)]

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use super::*;

const WITHIN: Duration = Duration::from_secs(10);

/// A waker that reports each wake on a channel.
struct Sent(Mutex<mpsc::Sender<()>>);

impl Wake for Sent {
    fn wake(&self) {
        let sender = self.0.lock().unwrap();
        sender.send(()).unwrap_or(());
    }
}

#[test]
fn arm_records_a_signal_raised_at_the_process_and_wakes() {
    let got = Arc::new(AtomicI32::new(0));
    let (woke_tx, woke_rx) = mpsc::channel();
    arm(&got, Arc::new(Sent(Mutex::new(woke_tx))));
    signal_hook::low_level::raise(signal_hook::consts::SIGTERM).unwrap();
    // The handler thread records the signal, then wakes the idle wait.
    woke_rx
        .recv_timeout(WITHIN)
        .expect("the arm wakes the idle wait before its deadline");
    assert_eq!(got.load(Ordering::SeqCst), signal_hook::consts::SIGTERM);
}
