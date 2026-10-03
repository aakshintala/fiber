use std::sync::{Arc, Mutex};

use contract::clock::Wake;
use contract::tool::Cancel;

use super::CancelToken;

struct Wakes {
    count: Mutex<usize>,
}

impl Wake for Wakes {
    fn wake(&self) {
        *self.count.lock().unwrap() += 1;
    }
}

#[test]
fn a_fresh_token_is_not_cancelled() {
    assert!(!CancelToken::new().is_cancelled());
    assert!(!CancelToken::default().is_cancelled());
}

#[test]
fn cancel_sets_the_flag_on_every_clone() {
    let token = CancelToken::new();
    let clone = token.clone();
    token.cancel();
    assert!(token.is_cancelled());
    assert!(clone.is_cancelled());
}

#[test]
fn cancel_wakes_every_subscriber_and_a_second_cancel_changes_nothing() {
    let token = CancelToken::new();
    let first = Arc::new(Wakes {
        count: Mutex::new(0),
    });
    let second = Arc::new(Wakes {
        count: Mutex::new(0),
    });
    let first_wake: Arc<dyn Wake> = first.clone();
    let second_wake: Arc<dyn Wake> = second.clone();
    token.subscribe(Arc::downgrade(&first_wake));
    token.subscribe(Arc::downgrade(&second_wake));
    token.cancel();
    token.cancel();
    assert!(token.is_cancelled());
    assert_eq!(*first.count.lock().unwrap(), 1);
    assert_eq!(*second.count.lock().unwrap(), 1);
}

#[test]
fn subscribing_after_cancel_still_observes_the_flag_and_is_not_woken() {
    let token = CancelToken::new();
    token.cancel();
    let wakes = Arc::new(Wakes {
        count: Mutex::new(0),
    });
    let wake: Arc<dyn Wake> = wakes.clone();
    token.subscribe(Arc::downgrade(&wake));
    assert!(token.is_cancelled());
    assert_eq!(*wakes.count.lock().unwrap(), 0);
}
