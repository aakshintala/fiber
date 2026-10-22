//! The timer table: starting the earliest due timer that waits, ending a
//! firing, and listing each finished timer once for its Lua function to be
//! freed (`docs/extensions.md`, "Host calls").

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
use std::time::Duration;

use contract::JobId;
use contract::clock::Clock;
use fakes::clock::FakeClock;

use super::super::Target;
use super::*;

fn timer(firing: bool) -> Timer {
    Timer {
        id: 0,
        every: Some(Duration::from_millis(50)),
        due: FakeClock::new().now(),
        timeout: Duration::from_millis(100),
        cancelled: false,
        firing,
    }
}

/// Cancelling a firing timer marks it and leaves it to end its firing;
/// twice changes nothing. Cancelling an idle one removes it and lists it
/// once for its Lua function to be freed; twice is a no-op.
#[test]
fn cancel_twice_is_a_no_op() {
    let hub = Hub::new(FakeClock::new());
    hub.lock().timers.by_id.insert(0, timer(true));
    hub.cancel_timer(0);
    hub.cancel_timer(0);
    {
        let shared = hub.lock();
        assert!(shared.timers.by_id.get(&0).unwrap().cancelled);
        assert!(
            shared.timers.cleanup.is_empty(),
            "a firing frees nothing yet"
        );
    }
    hub.lock().timers.by_id.insert(0, timer(false));
    hub.cancel_timer(0);
    hub.cancel_timer(0);
    let shared = hub.lock();
    assert!(shared.timers.by_id.is_empty(), "an idle cancel removes it");
    assert_eq!(shared.timers.cleanup, vec![0], "listed once to be freed");
}

/// `take_timer_cleanup` hands over what is listed and empties the list.
#[test]
fn take_timer_cleanup_takes_the_list() {
    let hub = Hub::new(FakeClock::new());
    hub.lock().timers.cleanup.extend([3, 5]);
    assert_eq!(hub.take_timer_cleanup(), vec![3, 5]);
    assert!(hub.take_timer_cleanup().is_empty());
}

/// `take_delegate_cleanup` hands over the `run` handle `finish_delegate`
/// queued and empties the list: the thread clears exactly this entry.
#[test]
fn take_delegate_cleanup_clears_the_run_handle_list() {
    let hub = Hub::new(FakeClock::new());
    let job_id = JobId("j_1".to_owned());
    hub.bind_delegate(job_id.clone(), 7, Duration::from_secs(5));
    let target = Target::Delegate {
        extension: "ext".to_owned(),
        job_id: job_id.clone(),
        handle: 7,
        timeout: Duration::from_secs(5),
    };
    hub.finish_delegate("ext", 0, &target, Ok(serde_json::Value::Null));
    assert_eq!(hub.take_delegate_cleanup(), vec![7]);
    assert!(hub.take_delegate_cleanup().is_empty());
}

/// The scheduler starts the earliest due timer that waits: due, neither
/// cancelled nor firing.
#[test]
fn timer_fire_starts_the_earliest_due_timer_that_waits() {
    let clock = FakeClock::new();
    let hub = Hub::new(clock.clone());
    let mut shared = hub.lock();
    let now = clock.now();
    shared.timers.next = 3;
    for (id, due, cancelled, firing) in [
        (0, now, false, true),
        (1, now, true, false),
        (
            2,
            now.checked_add(Duration::from_secs(60)).unwrap(),
            false,
            false,
        ),
    ] {
        shared.timers.next = shared.timers.next.max(id + 1);
        shared.timers.by_id.insert(
            id,
            Timer {
                id,
                every: None,
                due,
                timeout: Duration::from_millis(100),
                cancelled,
                firing,
            },
        );
    }
    assert!(shared.timers.fire(now).is_none(), "nothing due waits");
    let late = now.checked_add(Duration::from_secs(60)).unwrap();
    let (id, _) = shared.timers.fire(late).unwrap();
    assert_eq!(id, 2, "the future one is due at its time");
    shared.timers.end_firing(2, late);
    shared.timers.by_id.get_mut(&1).unwrap().cancelled = false;
    let (id, timeout) = shared.timers.fire(late).unwrap();
    assert_eq!(id, 1, "the earliest due id fires");
    assert_eq!(timeout, Duration::from_millis(100));
    assert!(shared.timers.by_id.get(&1).unwrap().firing);
}

/// An `every` ended uncancelled fires again `ms` after its end; anything
/// else leaves, freeing its Lua function on the extension's thread.
#[test]
fn timer_end_reschedules_an_uncancelled_every_and_removes_the_rest() {
    let clock = FakeClock::new();
    let hub = Hub::new(clock.clone());
    let mut shared = hub.lock();
    let now = clock.now();
    for (id, every) in [
        (0, None),
        (1, Some(Duration::from_millis(50))),
        (2, Some(Duration::from_millis(50))),
    ] {
        shared.timers.by_id.insert(
            id,
            Timer {
                id,
                every,
                due: now,
                timeout: Duration::from_millis(100),
                cancelled: id == 2,
                firing: true,
            },
        );
    }
    shared.timers.end_firing(0, now);
    shared.timers.end_firing(1, now);
    shared.timers.end_firing(2, now);
    assert!(!shared.timers.by_id.contains_key(&0), "an `after` leaves");
    assert!(
        !shared.timers.by_id.contains_key(&2),
        "a cancelled `every` leaves"
    );
    let timer = shared.timers.by_id.get(&1).unwrap();
    assert_eq!(
        timer.due,
        now.checked_add(Duration::from_millis(50)).unwrap()
    );
    assert!(!timer.firing);
    assert_eq!(shared.timers.cleanup, vec![0, 2]);
}
