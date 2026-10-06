//! `send_exec` against `set_exec_inbox`: buffered runs flush in order,
//! and a `deliver_to` racing a run's end strands nothing
//! (`docs/extensions.md`, "Host calls"). `cancel_timer` twice.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
use std::sync::TryLockError;
use std::sync::mpsc;

use fakes::clock::FakeClock;

use super::*;

fn exec(tag: &str) -> ExtensionExec {
    ExtensionExec {
        extension: "ext".to_owned(),
        program: tag.to_owned(),
        args: Vec::new(),
        cwd: "/tmp".to_owned(),
        process: contract::shapes::Process {
            exit_code: Some(0),
            signal: None,
            timed_out: false,
        },
    }
}

fn received(rx: &std::sync::mpsc::Receiver<Delivery>) -> Option<String> {
    match rx.try_recv().ok()? {
        Delivery::ExtensionExec(exec) => Some(exec.program),
        Delivery::Prompt(..)
        | Delivery::Steer(..)
        | Delivery::SteerDrop(..)
        | Delivery::Handoff(..)
        | Delivery::Reply(..)
        | Delivery::Close(_)
        | Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::Cancelled => None,
    }
}

/// A run that ends before any sender is buffered and flushed in order
/// on the first sender.
#[test]
fn a_run_before_any_sender_flushes_on_the_first_one() {
    let clock = FakeClock::new();
    let hub = Hub::new(clock);
    hub.send_exec(exec("first"));
    hub.send_exec(exec("second"));
    let (tx, rx) = std::sync::mpsc::channel();
    hub.set_exec_inbox(tx);
    assert_eq!(received(&rx).as_deref(), Some("first"));
    assert_eq!(received(&rx).as_deref(), Some("second"));
    assert!(hub.lock().exec_buffer.is_empty());
}

/// How long a test waits on a worker before it fails.
const WAIT: Duration = Duration::from_secs(10);

/// A run that ends after the extension's drop is dropped, even with a
/// live receiver: `dispose` (what `LuaExtension::drop` records) is held
/// under the same lock that routes deliveries, so anything routed after
/// it is suppressed. The worker sends only after the drop's signal, which
/// forces the order without timing; each wait has the one named deadline.
#[test]
fn a_run_ending_after_dispose_is_dropped() {
    let hub = Hub::new(FakeClock::new());
    let (tx, rx) = mpsc::channel();
    hub.set_exec_inbox(tx);
    let (proceed_tx, proceed_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let other = Arc::clone(&hub);
    std::thread::spawn(move || {
        let _waited = proceed_rx.recv_timeout(WAIT);
        other.send_exec(exec("late"));
        let _done = done_tx.send(());
    });
    hub.dispose("ext");
    let _proceed = proceed_tx.send(());
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the late run to be routed");
    assert!(received(&rx).is_none(), "nothing arrives after the drop");
    assert!(
        hub.lock().exec_buffer.is_empty(),
        "nothing is buffered after the drop"
    );
}

/// A sender that arrives after the extension's drop flushes nothing, so a
/// run buffered before the drop never leaves: `dispose` is held under the
/// same lock that routes deliveries. The worker sets the sender only after
/// the drop's signal, which forces the order without timing; each wait has
/// the one named deadline.
#[test]
fn a_sender_after_dispose_flushes_nothing() {
    let hub = Hub::new(FakeClock::new());
    hub.send_exec(exec("early"));
    assert_eq!(hub.lock().exec_buffer.len(), 1, "the run is buffered");
    let (tx, rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let other = Arc::clone(&hub);
    std::thread::spawn(move || {
        let _waited = proceed_rx.recv_timeout(WAIT);
        other.set_exec_inbox(tx);
        let _done = done_tx.send(());
    });
    hub.dispose("ext");
    let _proceed = proceed_tx.send(());
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the late sender to be routed");
    assert!(received(&rx).is_none(), "nothing flushes after the drop");
    assert_eq!(
        hub.lock().exec_buffer.len(),
        1,
        "the buffered run is never flushed nor delivered"
    );
}

/// Whether the hub lock is held: at a window it must be, so the other half
/// of the race cannot run inside it.
fn locked(hub: &Hub) -> bool {
    matches!(hub.shared.try_lock(), Err(TryLockError::WouldBlock))
}

/// A run ending against `deliver_to`, paused where `send_exec` has chosen
/// the buffer: the hub lock is still held, so the `deliver_to` started
/// there runs only after the run is buffered, and flushes it.
#[test]
fn deliver_to_at_a_runs_buffer_choice_flushes_the_run() {
    let hub = Hub::new(FakeClock::new());
    let (tx, rx) = mpsc::channel();
    let (held_tx, held_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let inbox = std::sync::Mutex::new(Some(tx));
    let other = Arc::clone(&hub);
    hub.pause_at_windows(Arc::new(move |hub, at| {
        if at != Window::Selected {
            return;
        }
        let _held = held_tx.send(locked(hub));
        let Some(tx) = inbox.lock().unwrap().take() else {
            return;
        };
        let (other, done_tx) = (Arc::clone(&other), done_tx.clone());
        std::thread::spawn(move || {
            other.set_exec_inbox(tx);
            let _done = done_tx.send(());
        });
    }));
    hub.send_exec(exec("run"));
    assert!(
        held_rx.recv_timeout(WAIT).unwrap(),
        "the buffer choice holds the hub lock"
    );
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the racing deliver_to to return");
    assert_eq!(received(&rx).as_deref(), Some("run"), "the run was flushed");
    assert!(hub.lock().exec_buffer.is_empty(), "nothing is stranded");
}

/// A run ending against `deliver_to`, paused where `set_exec_inbox` has
/// taken the buffer but not flushed it: the hub lock is still held, so the
/// run started there is sent only after the buffered one, in end order.
#[test]
fn a_run_ending_at_a_flush_follows_the_buffered_runs() {
    let hub = Hub::new(FakeClock::new());
    hub.send_exec(exec("first"));
    let (tx, rx) = mpsc::channel();
    let (held_tx, held_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let started = std::sync::Mutex::new(false);
    let other = Arc::clone(&hub);
    hub.pause_at_windows(Arc::new(move |hub, at| {
        if at != Window::Flushing || std::mem::replace(&mut *started.lock().unwrap(), true) {
            return;
        }
        let _held = held_tx.send(locked(hub));
        let (other, done_tx) = (Arc::clone(&other), done_tx.clone());
        std::thread::spawn(move || {
            other.send_exec(exec("second"));
            let _done = done_tx.send(());
        });
    }));
    hub.set_exec_inbox(tx);
    assert!(
        held_rx.recv_timeout(WAIT).unwrap(),
        "the flush holds the hub lock"
    );
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the racing run to be sent");
    assert_eq!(received(&rx).as_deref(), Some("first"));
    assert_eq!(received(&rx).as_deref(), Some("second"));
}

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
    hub.lock().timers.insert(0, timer(true));
    hub.cancel_timer(0);
    hub.cancel_timer(0);
    {
        let shared = hub.lock();
        assert!(shared.timers.get(&0).unwrap().cancelled);
        assert!(
            shared.timer_cleanup.is_empty(),
            "a firing frees nothing yet"
        );
    }
    hub.lock().timers.insert(0, timer(false));
    hub.cancel_timer(0);
    hub.cancel_timer(0);
    let shared = hub.lock();
    assert!(shared.timers.is_empty(), "an idle cancel removes it");
    assert_eq!(shared.timer_cleanup, vec![0], "listed once to be freed");
}

/// `take_timer_cleanup` hands over what is listed and empties the list.
#[test]
fn take_timer_cleanup_takes_the_list() {
    let hub = Hub::new(FakeClock::new());
    hub.lock().timer_cleanup.extend([3, 5]);
    assert_eq!(hub.take_timer_cleanup(), vec![3, 5]);
    assert!(hub.take_timer_cleanup().is_empty());
}
