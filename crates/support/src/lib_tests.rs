//! [`lock`](super::lock) returns the guard of a poisoned mutex.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use std::sync::Mutex;
use std::thread;

use super::lock;

#[test]
fn lock_returns_the_guard_of_a_poisoned_mutex() {
    let mutex = Mutex::new(7);
    thread::scope(|scope| {
        let held = scope.spawn(|| {
            let mut guard = lock(&mutex);
            *guard = 42;
            panic!("holding the lock while panicking");
        });
        assert!(held.join().is_err(), "the holder panicked");
    });
    assert_eq!(*lock(&mutex), 42);
}
