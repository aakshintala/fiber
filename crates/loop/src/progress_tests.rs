//! The pacer (`docs/tools.md`, "Progress"): what one running call holds,
//! when it is due, and what a write costs. `Instant`s come from the caller;
//! the pacer never reads a clock.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use contract::ActionId;
use contract::clock::Wake;
use contract::emit::Emit;
use contract::events::{Event, Progress};
use contract::tool::Output;
use fakes::clock::FakeClock;
use serde_json::json;

use super::{Pacer, SharedWake, Stream};

/// One origin; every instant below is built from it, so no clock is read.
fn origin() -> std::time::Instant {
    FakeClock::new().origin()
}

fn text(value: &str) -> Progress {
    Progress {
        text: Some(value.into()),
        details: None,
    }
}

#[test]
fn the_first_change_after_idle_is_due_at_once() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&text("half"));
    assert_eq!(pacer.take_due(at), Some(text("half")));
}

#[test]
fn a_second_change_inside_100_ms_is_held_until_written_at_plus_100_ms() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&text("first"));
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(1, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.take_due(at + Duration::from_millis(99)), None);
    assert_eq!(
        pacer.take_due(at + Duration::from_millis(100)),
        Some(text("second"))
    );
}

#[test]
fn three_held_changes_collapse_to_concatenated_text_and_the_latest_details() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&text("a"));
    assert_eq!(pacer.take_due(at), Some(text("a")));
    pacer.wrote(1, at);
    pacer.hold(&Progress {
        text: Some("b".into()),
        details: Some(json!({"done": 1})),
    });
    pacer.hold(&Progress {
        text: Some("c".into()),
        details: None,
    });
    pacer.hold(&Progress {
        text: Some("d".into()),
        details: Some(json!({"done": 2})),
    });
    // Nothing is due yet: the collapsed delta waits for the interval.
    assert_eq!(pacer.take_due(at + Duration::from_millis(50)), None);
    assert_eq!(
        pacer.take_due(at + Duration::from_millis(100)),
        Some(Progress {
            text: Some("bcd".into()),
            details: Some(json!({"done": 2})),
        })
    );
}

#[test]
fn a_later_delta_without_details_keeps_the_earlier_held_details() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&Progress {
        text: Some("a".into()),
        details: Some(json!({"done": 1})),
    });
    assert_eq!(
        pacer.take_due(at),
        Some(Progress {
            text: Some("a".into()),
            details: Some(json!({"done": 1})),
        })
    );
    pacer.wrote(1, at);
    pacer.hold(&Progress {
        text: Some("b".into()),
        details: Some(json!({"done": 1})),
    });
    pacer.hold(&text("c"));
    assert_eq!(
        pacer.take_due(at + Duration::from_millis(100)),
        Some(Progress {
            text: Some("bc".into()),
            details: Some(json!({"done": 1})),
        })
    );
}

#[test]
fn a_20_kib_write_pushes_the_next_due_to_200_ms() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&text("first"));
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(20 * 1024, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.take_due(at + Duration::from_millis(199)), None);
    assert_eq!(
        pacer.take_due(at + Duration::from_millis(200)),
        Some(text("second"))
    );
}

#[test]
fn a_1_byte_write_keeps_the_next_due_at_100_ms() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&text("first"));
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(1, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.deadline(), Some(at + Duration::from_millis(100)));
}

#[test]
fn the_deadline_is_none_when_idle_or_when_nothing_is_held() {
    let at = origin();
    let mut pacer = Pacer::default();
    assert_eq!(pacer.deadline(), None);
    pacer.hold(&text("first"));
    // Idle: the held change is due at once, so there is nothing to wait for.
    assert_eq!(pacer.deadline(), None);
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(20 * 1024, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.deadline(), Some(at + Duration::from_millis(200)));
}

#[test]
fn the_final_take_returns_the_held_change_before_the_interval() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&text("first"));
    assert_eq!(pacer.take_due(at), Some(text("first")));
    pacer.wrote(20 * 1024, at);
    pacer.hold(&text("second"));
    assert_eq!(pacer.take_final(), Some(text("second")));
    assert_eq!(pacer.take_final(), None);
}

#[test]
fn an_empty_delta_is_ignored() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&Progress {
        text: None,
        details: None,
    });
    assert_eq!(pacer.take_due(at), None);
    assert_eq!(pacer.take_final(), None);
    assert_eq!(pacer.deadline(), None);
}

#[test]
fn a_details_only_delta_is_held_and_taken_without_text() {
    let at = origin();
    let mut pacer = Pacer::default();
    pacer.hold(&Progress {
        text: None,
        details: Some(json!({"done": 3})),
    });
    assert_eq!(
        pacer.take_due(at),
        Some(Progress {
            text: None,
            details: Some(json!({"done": 3})),
        })
    );
}

#[test]
fn take_finished_returns_the_output_with_whatever_is_held() {
    let stream = Stream::new(Arc::new(SharedWake::default()), ActionId("a_1".into()));
    assert_eq!(stream.take_finished(), None);
    stream.emit(&Event::ToolCallDelta(Progress {
        text: Some("late".into()),
        details: None,
    }));
    assert_eq!(stream.take_finished(), None);
    stream.finish(Output::default());
    assert_eq!(
        stream.take_finished(),
        Some((
            Output::default(),
            Some(Progress {
                text: Some("late".into()),
                details: None,
            })
        ))
    );
    assert_eq!(stream.take_finished(), None);
}

#[test]
fn take_flush_returns_the_held_change_only_once_the_call_returns() {
    let stream = Stream::new(Arc::new(SharedWake::default()), ActionId("a_2".into()));
    stream.emit(&Event::ToolCallDelta(Progress {
        text: Some("held".into()),
        details: None,
    }));
    assert_eq!(stream.take_flush(), None);
    stream.finish(Output::default());
    assert_eq!(
        stream.take_flush(),
        Some(Progress {
            text: Some("held".into()),
            details: None,
        })
    );
    assert_eq!(stream.take_flush(), None);
}

/// Wall-clock bound on every wait for a helper thread.
const DEADLINE: Duration = Duration::from_secs(5);

/// A forward target that counts its wakes.
#[derive(Default)]
struct Counting(AtomicUsize);

impl Counting {
    fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}

impl Wake for Counting {
    fn wake(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// Parks `wake` with no deadline on `clock` from a helper thread, which
/// sends once the park returns.
fn park_on(wake: &Arc<SharedWake>, clock: &Arc<FakeClock>) -> mpsc::Receiver<()> {
    let (done, parked) = mpsc::channel();
    let wake = Arc::clone(wake);
    let clock = Arc::clone(clock);
    std::thread::spawn(move || {
        wake.park(clock.as_ref(), None);
        let _sent = done.send(());
    });
    parked
}

#[test]
fn forwarding_passes_on_a_bump_that_landed_before_it_once() {
    let wake = Arc::new(SharedWake::default());
    let target = Arc::new(Counting::default());
    wake.wake();
    wake.forward(Some(Arc::clone(&target) as Arc<dyn Wake>));
    assert_eq!(target.count(), 1, "the earlier bump reaches the target");
    // The bump was passed on, so the flag is clear and a park blocks.
    assert!(
        !super::lock(&wake.inner).set,
        "nothing asks for another pass"
    );
    wake.forward(None);
    let clock = FakeClock::new();
    let parked = park_on(&wake, &clock);
    assert!(clock.await_parked_unbounded(DEADLINE), "the park waits");
    wake.wake();
    parked
        .recv_timeout(DEADLINE)
        .expect("a later bump ends the park");
}

#[test]
fn every_bump_while_forwarding_wakes_the_target() {
    let wake = SharedWake::default();
    let target = Arc::new(Counting::default());
    wake.forward(Some(Arc::clone(&target) as Arc<dyn Wake>));
    assert_eq!(target.count(), 0, "no bump landed before");
    wake.wake();
    wake.wake();
    assert_eq!(target.count(), 2);
}

#[test]
fn clearing_the_forward_keeps_the_flag_and_drops_the_target() {
    let wake = Arc::new(SharedWake::default());
    let target = Arc::new(Counting::default());
    wake.forward(Some(Arc::clone(&target) as Arc<dyn Wake>));
    wake.wake();
    wake.forward(None);
    let clock = FakeClock::new();
    park_on(&wake, &clock)
        .recv_timeout(DEADLINE)
        .expect("the bump while forwarding ends the next park at once");
    wake.wake();
    assert_eq!(target.count(), 1, "a bump after clearing reaches no target");
}
