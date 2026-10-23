use std::sync::{Arc, Mutex};

use fakes::{Deadline, MUST_SUCCEED_WITHIN};

use super::{Cancel, Token};
use crate::clock::Wake;

struct Wakes {
    count: Mutex<usize>,
}

impl Wake for Wakes {
    fn wake(&self) {
        *self.count.lock().unwrap() += 1;
    }
}

fn wakes() -> (Arc<Wakes>, Arc<dyn Wake>) {
    let wakes = Arc::new(Wakes {
        count: Mutex::new(0),
    });
    let wake: Arc<dyn Wake> = wakes.clone();
    (wakes, wake)
}

fn count_of(wakes: &Wakes) -> usize {
    *wakes.count.lock().unwrap()
}

#[test]
fn a_fresh_token_is_not_cancelled() {
    assert!(!Token::new().is_cancelled());
    assert!(!Token::default().is_cancelled());
}

#[test]
fn cancel_sets_the_flag_on_every_clone() {
    let token = Token::new();
    let clone = token.clone();
    token.cancel();
    assert!(token.is_cancelled());
    assert!(clone.is_cancelled());
}

#[test]
fn cancel_wakes_every_subscriber_and_a_second_cancel_changes_nothing() {
    let token = Token::new();
    let (first, first_wake) = wakes();
    let (second, second_wake) = wakes();
    token.subscribe(Arc::downgrade(&first_wake));
    token.subscribe(Arc::downgrade(&second_wake));
    token.cancel();
    token.cancel();
    assert!(token.is_cancelled());
    assert_eq!(count_of(&first), 1);
    assert_eq!(count_of(&second), 1);
}

#[test]
fn subscribing_after_cancel_still_observes_the_flag_and_is_not_woken() {
    let token = Token::new();
    token.cancel();
    let (late, late_wake) = wakes();
    token.subscribe(Arc::downgrade(&late_wake));
    assert!(token.is_cancelled());
    assert_eq!(count_of(&late), 0);
    assert_eq!(crate::lock(&token.inner).wakers.len(), 0);
}

/// A waker that locks the token's own state inside `wake`: cancel wakes
/// after releasing the lock, so this cannot deadlock it.
struct Reentrant {
    token: Token,
    count: Mutex<usize>,
}

impl Wake for Reentrant {
    fn wake(&self) {
        *self.count.lock().unwrap() += 1;
        assert!(self.token.is_cancelled());
    }
}

#[test]
fn a_waker_that_reads_the_token_inside_wake_does_not_deadlock_cancel() {
    let token = Token::new();
    let reentrant = Arc::new(Reentrant {
        token: token.clone(),
        count: Mutex::new(0),
    });
    let wake: Arc<dyn Wake> = reentrant.clone();
    token.subscribe(Arc::downgrade(&wake));
    let (done_tx, done) = std::sync::mpsc::channel();
    let cancelling = token.clone();
    std::thread::spawn(move || {
        cancelling.cancel();
        let _ignored = done_tx.send(());
    });
    Deadline::after(MUST_SUCCEED_WITHIN)
        .recv(&done)
        .expect("cancel wakes a re-entrant subscriber without deadlocking");
    assert!(token.is_cancelled());
    assert_eq!(*reentrant.count.lock().unwrap(), 1);
}

#[test]
fn a_later_subscribe_prunes_a_dead_waker_before_it_pushes() {
    let token = Token::new();
    let (dead, dead_wake) = wakes();
    token.subscribe(Arc::downgrade(&dead_wake));
    drop(dead);
    drop(dead_wake);
    let (live, live_wake) = wakes();
    token.subscribe(Arc::downgrade(&live_wake));
    assert_eq!(crate::lock(&token.inner).wakers.len(), 1);
    token.cancel();
    assert_eq!(count_of(&live), 1);
}
