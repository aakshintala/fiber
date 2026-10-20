//! The shared steering queue's contract: push, drop-by-id, snapshot
//! order, duplicate rejection, and the forced interleaving that proves the
//! final event matches the final queue.

use std::sync::{Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use super::{Duplicate, EntryGate, Queue};
use crate::CommandId;
use crate::emit::Emit;
use crate::events::{Event, QueuedMessage};
use crate::shapes::{ContentPart, Origin, Sender};
use fakes::Deadline;

/// One hang bound for every wait: the queue takes no clock, so real time
/// only bounds the test's own waits.
const BOUND: Duration = Duration::from_secs(10);

/// An [`Emit`] that records every event in arrival order. Local, not
/// `fakes::emit::EventLog`: the dev-cycle builds a second `contract`
/// instance for `fakes`, so its event log's trait impl never matches
/// this crate's `Emit`.
#[derive(Default)]
struct Recorded {
    events: Mutex<Vec<Event>>,
}

impl Recorded {
    fn events(&self) -> Vec<Event> {
        BlockingEmit::lock(&self.events).clone()
    }
}

impl Emit for Recorded {
    fn emit(&self, event: &Event) {
        BlockingEmit::lock(&self.events).push(event.clone());
    }
}

fn driver(id: &str) -> (CommandId, Sender) {
    let id = CommandId(id.to_owned());
    (
        id.clone(),
        Sender {
            origin: Origin::Driver,
            command_id: Some(id),
        },
    )
}

fn text(text: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text {
        text: text.to_owned(),
    }]
}

fn queued(queue: &Queue, id: &str, text: &str, log: &Recorded) {
    let (id, sender) = driver(id);
    queue
        .push(id, self::text(text), sender, log)
        .expect("a fresh id queues");
}

fn messages_of(events: &[Event]) -> Vec<Vec<QueuedMessage>> {
    events
        .iter()
        .map(|event| {
            let Event::SteeringQueue(queue) = event else {
                panic!("only steering_queue is emitted: {event:?}");
            };
            queue.messages.clone()
        })
        .collect()
}

fn messages(log: &Recorded) -> Vec<Vec<QueuedMessage>> {
    messages_of(&log.events())
}

#[test]
fn drop_removes_exactly_its_id() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    queued(&queue, "c_b", "b", &log);
    assert!(queue.remove(&CommandId("c_a".into()), &log));
    assert_eq!(
        queue
            .snapshot()
            .iter()
            .map(|message| message.sender.command_id.clone())
            .collect::<Vec<_>>(),
        [Some(CommandId("c_b".into()))],
        "only the named entry leaves"
    );
    assert!(
        !queue.remove(&CommandId("c_a".into()), &log),
        "a second drop finds nothing"
    );
    assert_eq!(
        messages(&log).last().expect("every section emits"),
        &queue.snapshot(),
        "the last event matches the final queue"
    );
}

#[test]
fn snapshot_lists_entries_oldest_first() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    queued(&queue, "c_b", "b", &log);
    queued(&queue, "c_c", "c", &log);
    let texts: Vec<String> = queue
        .snapshot()
        .iter()
        .map(|message| match &message.content[..] {
            [ContentPart::Text { text }] => text.clone(),
            content => panic!("one text part: {content:?}"),
        })
        .collect();
    assert_eq!(texts, ["a", "b", "c"]);
}

#[test]
fn a_duplicate_id_is_rejected() {
    let log = Recorded::default();
    let queue = Queue::new();
    let (id, sender) = driver("c_a");
    queue
        .push(id.clone(), text("a"), sender.clone(), &log)
        .expect("a fresh id queues");
    assert_eq!(
        queue.push(id, text("again"), sender, &log),
        Err(Duplicate),
        "the id stays with its first entry"
    );
    assert_eq!(queue.snapshot().len(), 1, "the duplicate queues nothing");
}

#[test]
fn an_empty_snapshot_emits_empty() {
    let log = Recorded::default();
    let queue = Queue::new();
    assert!(queue.snapshot().is_empty());
    queued(&queue, "c_a", "a", &log);
    assert!(queue.remove(&CommandId("c_a".into()), &log));
    assert_eq!(
        messages(&log).last().expect("the drop emits"),
        &[],
        "removing the last entry emits the empty queue"
    );
}

/// An [`Emit`] that holds the first `steering_queue` append inside the
/// caller's section until the test releases it, recording every event in
/// arrival order once released. Later appends pass through: only the push
/// is held, so a drop that finds no lock held completes at once.
struct BlockingEmit {
    entered: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
    held: Mutex<bool>,
    events: Mutex<Vec<Event>>,
}

impl BlockingEmit {
    fn hold_first(entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) -> Self {
        Self {
            entered,
            release: Mutex::new(release),
            held: Mutex::new(true),
            events: Mutex::new(Vec::new()),
        }
    }

    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Emit for BlockingEmit {
    fn emit(&self, event: &Event) {
        // Acknowledged FIFO-style: the test takes one entered per append.
        let _sent = self.entered.send(());
        if *Self::lock(&self.held) {
            *Self::lock(&self.held) = false;
            // The push waits here, inside its queue-lock section.
            let _released = Self::lock(&self.release).recv();
        }
        Self::lock(&self.events).push(event.clone());
    }
}

/// The scope's only push-release sender: an explicit [`release`](Self::release)
/// frees the held push on the success path, and [`Drop`] frees it on a
/// failing path, so a panic never strands the push thread inside
/// `thread::scope`'s join. A second release is a buffered no-op.
struct ReleaseOnDrop {
    tx: mpsc::Sender<()>,
}

impl ReleaseOnDrop {
    fn release(&self) {
        let _sent = self.tx.send(());
    }
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        let _sent = self.tx.send(());
    }
}

#[test]
fn a_drop_released_while_the_push_is_held_still_lands_empty_last() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let blocked = BlockingEmit::hold_first(entered_tx, release_rx);
    // Shared by reference: both threads append through the same emitter.
    let queue = Queue::new();
    let queued = &queue;
    let emitting = &blocked;
    thread::scope(|scope| {
        // The closure owns the only release sender: unwinding drops it
        // (releasing the push) before the scope joins its threads.
        let release = ReleaseOnDrop { tx: release_tx };
        let pushed = scope.spawn(move || {
            let (id, sender) = driver("c_a");
            queued.push(id, text("a"), sender, emitting)
        });
        // The push is provably inside its section: its append fired.
        Deadline::after(BOUND)
            .recv(&entered_rx)
            .expect("the push appends while holding the lock");
        // Structural, no scheduling: the append fired inside the push's
        // section, so the lock must still be held — the test thread
        // observes the mutex directly instead of asking the drop thread
        // to prove blockedness. Under the old emit-after-unlock shape
        // the append fires after the release and this fails.
        assert!(
            queued.is_locked_for_test(),
            "the push holds the queue lock across its emit"
        );
        // The drop reaches the acquisition path only after the push is
        // inside, so the gate holds the drop thread alone.
        let (gate_tx, gate_rx) = mpsc::channel();
        let (free_tx, free_rx) = mpsc::channel();
        queue.hold_entry_for_test(EntryGate {
            entered: gate_tx,
            release: free_rx,
        });
        let (done_tx, done_rx) = mpsc::channel();
        let dropped = scope.spawn(move || {
            let removed = queued.remove(&CommandId("c_a".into()), emitting);
            let _sent = done_tx.send(removed);
        });
        Deadline::after(BOUND)
            .recv(&gate_rx)
            .expect("the drop waits at the gate");
        // Open the drop gate while the push is still held. With the fix
        // the two sections serialize on the shared mutex, so the drop's
        // append cannot precede the push's release and this bounded
        // receive must expire; without the fix the push released the
        // mutex before blocking, so the drop's lock-free path appends
        // inside the bound and this assertion fails.
        let _sent = free_tx.send(());
        assert!(
            Deadline::after(BOUND).recv(&entered_rx).is_err(),
            "the drop's append stays blocked while the push holds the lock"
        );
        // Only then is the push released: it emits `[A]`, the drop takes
        // the lock, removes A and emits `[]` last.
        release.release();
        assert!(
            Deadline::after(BOUND)
                .recv(&done_rx)
                .expect("the drop completes after the release"),
            "the drop removed the pushed entry"
        );
        pushed
            .join()
            .expect("the push thread ends")
            .expect("push queued");
        dropped.join().expect("the drop thread ends");
        // The drop's pass-through append fired its own entered.
        Deadline::after(BOUND)
            .recv(&entered_rx)
            .expect("the drop appended");
    });
    assert!(
        queue.snapshot().is_empty(),
        "push A then drop A leaves nothing queued"
    );
    let events = BlockingEmit::lock(&blocked.events).clone();
    assert_eq!(
        messages_of(&events).last().expect("both sections emitted"),
        &[],
        "the final event is the empty queue, not the push's stale row"
    );
}
