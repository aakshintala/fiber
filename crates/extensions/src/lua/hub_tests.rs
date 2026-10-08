//! `send` against `set_inbox`: buffered deliveries flush in order,
//! and a `deliver_to` racing a delivery's end strands nothing
//! (`docs/extensions.md`, "Host calls"). `cancel_timer` twice. `emit`
//! and the `set_emit` flush hold the hub lock past `seal`, so no extension
//! line follows `fiber_exited`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
use std::sync::TryLockError;
use std::sync::mpsc;

use contract::emit::Emit;
use contract::events::{Event, ExtensionExec, ExtensionUi, Ui};
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
        Delivery::ExtensionLog(log) => Some(log.message),
        Delivery::Prompt(..)
        | Delivery::Steer(..)
        | Delivery::SteerDrop(..)
        | Delivery::Handoff(..)
        | Delivery::Model(..)
        | Delivery::Reply(..)
        | Delivery::Rewind(..)
        | Delivery::Close(_)
        | Delivery::Job(_)
        | Delivery::JobLine(_)
        | Delivery::Interaction(_)
        | Delivery::Resolved(..)
        | Delivery::Cancelled => None,
    }
}

/// A run that ends before any sender is buffered and flushed in order
/// on the first sender.
#[test]
fn a_run_before_any_sender_flushes_on_the_first_one() {
    let clock = FakeClock::new();
    let hub = Hub::new(clock);
    hub.send(Delivery::ExtensionExec(exec("first")));
    hub.send(Delivery::ExtensionExec(exec("second")));
    let (tx, rx) = std::sync::mpsc::channel();
    hub.set_inbox(tx);
    assert_eq!(received(&rx).as_deref(), Some("first"));
    assert_eq!(received(&rx).as_deref(), Some("second"));
    assert!(hub.lock().buffer.is_empty());
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
    hub.set_inbox(tx);
    let (proceed_tx, proceed_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let other = Arc::clone(&hub);
    std::thread::spawn(move || {
        proceed_rx
            .recv_timeout(WAIT)
            .expect("waited for the drop's signal before routing the late run");
        other.send(Delivery::ExtensionExec(exec("late")));
        let _done = done_tx.send(());
    });
    hub.dispose("ext");
    let _proceed = proceed_tx.send(());
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the late run to be routed");
    assert!(received(&rx).is_none(), "nothing arrives after the drop");
    assert!(
        hub.lock().buffer.is_empty(),
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
    hub.send(Delivery::ExtensionExec(exec("early")));
    assert_eq!(hub.lock().buffer.len(), 1, "the run is buffered");
    let (tx, rx) = mpsc::channel();
    let (proceed_tx, proceed_rx) = mpsc::channel::<()>();
    let (done_tx, done_rx) = mpsc::channel();
    let other = Arc::clone(&hub);
    std::thread::spawn(move || {
        proceed_rx
            .recv_timeout(WAIT)
            .expect("waited for the drop's signal before setting the late sender");
        other.set_inbox(tx);
        let _done = done_tx.send(());
    });
    hub.dispose("ext");
    let _proceed = proceed_tx.send(());
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the late sender to be routed");
    assert!(received(&rx).is_none(), "nothing flushes after the drop");
    assert_eq!(
        hub.lock().buffer.len(),
        1,
        "the buffered run is never flushed nor delivered"
    );
}

/// Whether the hub lock is held: at a window it must be, so the other half
/// of the race cannot run inside it.
fn locked(hub: &Hub) -> bool {
    matches!(hub.shared.try_lock(), Err(TryLockError::WouldBlock))
}

/// A run ending against `deliver_to`, paused where `send` has chosen
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
            other.set_inbox(tx);
            let _done = done_tx.send(());
        });
    }));
    hub.send(Delivery::ExtensionExec(exec("run")));
    assert!(
        held_rx
            .recv_timeout(WAIT)
            .expect("waited for the pause at the buffer choice"),
        "the buffer choice holds the hub lock"
    );
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the racing deliver_to to return");
    assert_eq!(received(&rx).as_deref(), Some("run"), "the run was flushed");
    assert!(hub.lock().buffer.is_empty(), "nothing is stranded");
}

/// A run ending against `deliver_to`, paused where `set_inbox` has
/// taken the buffer but not flushed it: the hub lock is still held, so the
/// run started there is sent only after the buffered one, in end order.
#[test]
fn a_run_ending_at_a_flush_follows_the_buffered_runs() {
    let hub = Hub::new(FakeClock::new());
    hub.send(Delivery::ExtensionExec(exec("first")));
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
            other.send(Delivery::ExtensionExec(exec("second")));
            let _done = done_tx.send(());
        });
    }));
    hub.set_inbox(tx);
    assert!(
        held_rx
            .recv_timeout(WAIT)
            .expect("waited for the pause at the flush"),
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

/// After `seal`, a delivery reaches neither the inbox nor the buffer: one
/// in flight finishes first and every later emission or delivery is dropped.
#[test]
fn a_delivery_after_seal_is_dropped() {
    let hub = Hub::new(FakeClock::new());
    let (tx, rx) = mpsc::channel();
    hub.set_inbox(tx);
    hub.seal();
    hub.send(Delivery::ExtensionExec(exec("late")));
    assert!(received(&rx).is_none(), "nothing arrives after the seal");
    assert!(hub.lock().buffer.is_empty());
}

fn status(text: &str) -> Event {
    Event::ExtensionUi(ExtensionUi {
        extension: "ext".to_owned(),
        ui: Ui::Status {
            status: text.to_owned(),
        },
    })
}

/// An emitter that blocks inside `emit` until the test releases it, so a
/// racing `seal` must wait for the in-flight emission when the hub lock is
/// held through the write.
struct BlockingEmit {
    entered: mpsc::Sender<()>,
    release: std::sync::Mutex<mpsc::Receiver<()>>,
    recorded: std::sync::Mutex<Vec<Event>>,
}

impl Emit for BlockingEmit {
    fn emit(&self, event: &Event) {
        let _entered = self.entered.send(());
        let _released = self.release.lock().unwrap().recv_timeout(WAIT).ok();
        self.recorded.lock().unwrap().push(event.clone());
    }
}

/// An emission holds the hub lock through the write, so `seal` cannot slip
/// between the choice and the write: `fiber_exited` follows the emitted
/// line. The worker blocks inside the emitter holding the lock; the seal
/// spawned at the pause point must still be waiting when the worker gets
/// there. Dropped early, the seal returns first and the later emission is
/// still written after it.
#[test]
fn an_emission_holds_the_lock_past_seal() {
    let hub = Hub::new(FakeClock::new());
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let emit = Arc::new(BlockingEmit {
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
        recorded: std::sync::Mutex::new(Vec::new()),
    });
    hub.set_emit(Arc::clone(&emit) as Arc<dyn Emit>);
    let other = Arc::clone(&hub);
    let (sealed_tx, sealed_rx) = mpsc::channel();
    hub.pause_at_windows(Arc::new(move |hub, at| {
        if at != Window::Emitting {
            return;
        }
        assert!(locked(hub), "the emission holds the hub lock past seal");
        let (other, sealed_tx) = (Arc::clone(&other), sealed_tx.clone());
        std::thread::spawn(move || {
            other.seal();
            let _sealed = sealed_tx.send(());
        });
    }));
    let event = status("syncing");
    let (done_tx, done_rx) = mpsc::channel();
    let worker = Arc::clone(&hub);
    std::thread::spawn(move || {
        worker.emit(event);
        let _done = done_tx.send(());
    });
    entered_rx
        .recv_timeout(WAIT)
        .expect("waited for the emission to reach the emitter");
    assert!(
        sealed_rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "seal waited for the in-flight emission"
    );
    let _released = release_tx.send(());
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the emission to finish");
    sealed_rx
        .recv_timeout(WAIT)
        .expect("waited for the seal to return after the emission");
    assert_eq!(emit.recorded.lock().unwrap().len(), 1);
    hub.emit(status("late"));
    assert_eq!(
        emit.recorded.lock().unwrap().len(),
        1,
        "nothing is emitted after the seal"
    );
}

/// The `set_emit` flush holds the hub lock through the last write, so a
/// `seal` racing it cannot return before the buffered lines are written:
/// `fiber_exited` follows every flushed line.
#[test]
fn a_buffered_flush_holds_the_lock_past_seal() {
    let hub = Hub::new(FakeClock::new());
    hub.emit(status("early"));
    assert_eq!(hub.lock().emit_buffer.len(), 1);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let emit = Arc::new(BlockingEmit {
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
        recorded: std::sync::Mutex::new(Vec::new()),
    });
    let other = Arc::clone(&hub);
    let (sealed_tx, sealed_rx) = mpsc::channel();
    hub.pause_at_windows(Arc::new(move |hub, at| {
        if at != Window::EmitFlushing {
            return;
        }
        assert!(locked(hub), "the flush holds the hub lock past seal");
        let (other, sealed_tx) = (Arc::clone(&other), sealed_tx.clone());
        std::thread::spawn(move || {
            other.seal();
            let _sealed = sealed_tx.send(());
        });
    }));
    let (done_tx, done_rx) = mpsc::channel();
    let worker = Arc::clone(&hub);
    let emit_for_worker = Arc::clone(&emit);
    std::thread::spawn(move || {
        worker.set_emit(emit_for_worker as Arc<dyn Emit>);
        let _done = done_tx.send(());
    });
    entered_rx
        .recv_timeout(WAIT)
        .expect("waited for the flush to reach the emitter");
    assert!(
        sealed_rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "seal waited for the in-flight flush"
    );
    let _released = release_tx.send(());
    done_rx
        .recv_timeout(WAIT)
        .expect("waited for the flush to finish");
    sealed_rx
        .recv_timeout(WAIT)
        .expect("waited for the seal to return after the flush");
    assert_eq!(
        emit.recorded.lock().unwrap().len(),
        1,
        "the buffered line was flushed before the seal"
    );
    hub.emit(status("late"));
    assert_eq!(
        emit.recorded.lock().unwrap().len(),
        1,
        "nothing is emitted after the seal"
    );
}

#[derive(Default)]
struct Recorder {
    events: std::sync::Mutex<Vec<Event>>,
}

impl Emit for Recorder {
    fn emit(&self, event: &Event) {
        self.events.lock().unwrap().push(event.clone());
    }
}

#[test]
fn set_emit_after_dispose_installs_nothing_and_flushes_nothing() {
    // Disposed but not sealed: `set_emit` installs no emitter and flushes
    // nothing, so the `||` becoming `&&` would wrongly install and flush.
    let hub = Hub::new(FakeClock::new());
    hub.emit(status("early"));
    assert_eq!(hub.lock().emit_buffer.len(), 1);
    hub.dispose("ext");
    assert!(!hub.lock().sealed, "dispose alone does not seal");
    let recorder = Arc::new(Recorder::default());
    hub.set_emit(Arc::clone(&recorder) as Arc<dyn Emit>);
    assert!(
        recorder.events.lock().unwrap().is_empty(),
        "nothing flushes after dispose"
    );
    assert!(
        hub.lock().emitter.is_none(),
        "no emitter is installed after dispose"
    );
    assert_eq!(
        hub.lock().emit_buffer.len(),
        1,
        "the buffered line stays buffered"
    );
    hub.emit(status("late"));
    assert!(
        recorder.events.lock().unwrap().is_empty(),
        "nothing is emitted after dispose"
    );
}

#[test]
fn set_emit_after_seal_installs_nothing_and_flushes_nothing() {
    // Sealed but not disposed: `set_emit` installs no emitter and flushes
    // nothing, so the `||` becoming `&&` would wrongly install and flush.
    let hub = Hub::new(FakeClock::new());
    hub.emit(status("early"));
    assert_eq!(hub.lock().emit_buffer.len(), 1);
    hub.seal();
    assert!(!hub.lock().disposed, "seal alone does not dispose");
    let recorder = Arc::new(Recorder::default());
    hub.set_emit(Arc::clone(&recorder) as Arc<dyn Emit>);
    assert!(
        recorder.events.lock().unwrap().is_empty(),
        "nothing flushes after seal"
    );
    assert!(
        hub.lock().emitter.is_none(),
        "no emitter is installed after seal"
    );
    assert_eq!(
        hub.lock().emit_buffer.len(),
        1,
        "the buffered line stays buffered"
    );
    hub.emit(status("late"));
    assert!(
        recorder.events.lock().unwrap().is_empty(),
        "nothing is emitted after seal"
    );
}

/// A driver whose presence is all `driver` reports: it answers `Ok(None)`.
struct TestDrive;

impl contract::extension::Drive for TestDrive {
    fn drive(
        &self,
        _extension: &str,
        _command: &str,
        _args: serde_json::Map<String, serde_json::Value>,
        answer: contract::inbox::Ack,
    ) {
        answer.0(Ok(None));
    }
}

/// Disposed but not sealed: `driver` returns none, so the `||` becoming
/// `&&` would wrongly return the driver.
#[test]
fn driver_after_dispose_alone_is_none() {
    let hub = Hub::new(FakeClock::new());
    hub.set_driver(Arc::new(TestDrive) as Arc<dyn contract::extension::Drive>);
    assert!(hub.driver().is_some(), "a driver is bound before the drop");
    hub.dispose("ext");
    assert!(!hub.lock().sealed, "dispose alone does not seal");
    assert!(hub.driver().is_none(), "no drive follows the drop");
}

/// Sealed but not disposed: `driver` returns none, so the `||` becoming
/// `&&` would wrongly return the driver.
#[test]
fn driver_after_seal_alone_is_none() {
    let hub = Hub::new(FakeClock::new());
    hub.set_driver(Arc::new(TestDrive) as Arc<dyn contract::extension::Drive>);
    assert!(hub.driver().is_some(), "a driver is bound before the seal");
    hub.seal();
    assert!(!hub.lock().disposed, "seal alone does not dispose");
    assert!(hub.driver().is_none(), "no drive follows fiber_exited");
}
