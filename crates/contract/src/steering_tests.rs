//! The shared steering queue's contract: push, drop-by-id, snapshot
//! order, duplicate rejection, and the forced interleaving that proves the
//! final event matches the final queue.

use std::sync::{Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use super::{Duplicate, EntryGate, Queue, Staged};
use crate::CommandId;
use crate::SessionId;
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
        // to prove blockedness. `tests` is a child module of `steering`
        // (`#[path]`), so it reads the private `inner` field with no
        // production-visible seam; `try_lock` never blocks and bypasses
        // the armed one-shot gate. Under the old emit-after-unlock shape
        // the append fires after the release and this fails.
        assert!(
            queued.inner.try_lock().is_err(),
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

fn session(id: &str, from: &str) -> (CommandId, Sender) {
    let id = CommandId(id.to_owned());
    (
        id.clone(),
        Sender {
            origin: Origin::Session {
                from_session_id: SessionId(from.to_owned()),
            },
            command_id: Some(id),
        },
    )
}

fn staged(id: &str, log: &Recorded, queue: &Queue) {
    let (pushed, sender) = driver(id);
    queue
        .push(pushed, text(id), sender, log)
        .expect("a fresh id queues");
    assert!(
        queue.stage(&CommandId(id.to_owned()), text(id), Staged::default(), log),
        "the pushed entry stages"
    );
}

#[test]
fn admit_hooked_stages_from_the_start() {
    let log = Recorded::default();
    let queue = Queue::new();
    let (id, sender) = driver("c_a");
    queue
        .admit_hooked(id.clone(), text("a"), sender, Staged::default(), &log)
        .expect("a fresh id admits");
    assert!(
        queue.is_staged(&id, &Origin::Driver),
        "a hooked admit is staged at once"
    );
    assert!(
        queue.unstaged(&id).is_none(),
        "a hooked admit never needs a hook pass"
    );
    assert_eq!(queue.snapshot().len(), 1, "the hooked entry is queued");
}

#[test]
fn admit_hooked_rejects_a_second_admit_of_the_same_id() {
    let log = Recorded::default();
    let queue = Queue::new();
    let (id, sender) = driver("c_a");
    queue
        .admit_hooked(
            id.clone(),
            text("a"),
            sender.clone(),
            Staged::default(),
            &log,
        )
        .expect("the first admit of a fresh id lands");
    assert_eq!(
        queue.admit_hooked(id, text("again"), sender, Staged::default(), &log),
        Err(Duplicate),
        "the id stays with its first entry"
    );
    assert_eq!(queue.snapshot().len(), 1, "the duplicate queues nothing");
}

#[test]
fn admit_session_hooked_stages_from_the_start_without_tracking_the_id() {
    let log = Recorded::default();
    let queue = Queue::new();
    let (id, sender) = session("c_m", "s_1");
    queue
        .admit_session_hooked(
            id.clone(),
            text("m"),
            sender.clone(),
            Staged::default(),
            &log,
        )
        .expect("a fresh session id admits");
    assert!(
        queue.is_staged(&id, &sender.origin),
        "a session-hooked admit is staged at once"
    );
    assert!(
        queue.unstaged(&id).is_none(),
        "a session-hooked admit never needs a hook pass"
    );
    assert!(
        !queue.is_admitted(&id),
        "a session id stays invisible to is_admitted"
    );
    let taken = queue.take_staged(&log);
    assert_eq!(taken.len(), 1, "take_staged drains the session entry");
    assert!(
        taken[0].staged.is_some(),
        "the drained session entry carries its staged verdict"
    );
    assert!(
        !queue.is_admitted(&id),
        "a drained session id stays invisible to is_admitted"
    );
}

#[test]
fn admit_session_hooked_rejects_a_queued_same_sender_id_but_keeps_a_same_id_steer_live() {
    let log = Recorded::default();
    let queue = Queue::new();
    let (id, sender) = driver("c_a");
    queue
        .push(id.clone(), text("a"), sender, &log)
        .expect("the steer push lands");
    let (same_id, other_sender) = session("c_a", "s_1");
    queue
        .admit_session_hooked(
            same_id.clone(),
            text("m"),
            other_sender,
            Staged::default(),
            &log,
        )
        .expect("a same-id session entry from another sender still admits");
    let (dup_id, dup_sender) = session("c_a", "s_1");
    assert_eq!(
        queue.admit_session_hooked(dup_id, text("again"), dup_sender, Staged::default(), &log),
        Err(Duplicate),
        "a second session admit of the same id and sender is a duplicate"
    );
    let taken = queue.take_staged(&log);
    assert_eq!(taken.len(), 1, "only the session entry drains");
    assert!(
        queue.is_admitted(&id),
        "draining a same-id session entry never clears the steer's admission"
    );
    assert_eq!(queue.snapshot().len(), 1, "the unstaged steer stays queued");
}

#[test]
fn is_staged_ignores_an_id_that_is_not_queued() {
    let log = Recorded::default();
    let queue = Queue::new();
    staged("c_a", &log, &queue);
    assert!(
        !queue.is_staged(&CommandId("c_absent".into()), &Origin::Driver),
        "an absent id is not staged while another entry is"
    );
}

#[test]
fn is_staged_matches_the_exact_id_and_origin() {
    let log = Recorded::default();
    let queue = Queue::new();
    staged("c_a", &log, &queue);
    let (id, _) = driver("c_a");
    assert!(
        queue.is_staged(&id, &Origin::Driver),
        "the staged entry matches its own id and origin"
    );
}

#[test]
fn a_hooked_entry_needs_no_hook_pass() {
    let log = Recorded::default();
    let queue = Queue::new();
    let (id, sender) = driver("c_a");
    queue
        .admit_hooked(id.clone(), text("a"), sender, Staged::default(), &log)
        .expect("a fresh id admits");
    assert!(
        queue.unstaged(&id).is_none(),
        "a staged entry is never hook-passed twice"
    );
}

#[test]
fn a_pushed_entry_waits_for_its_hook_pass() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    let (content, _) = queue
        .unstaged(&CommandId("c_a".into()))
        .expect("a fresh reader entry waits unstaged");
    assert_eq!(content, text("a"), "the hook pass sees what was sent");
}

#[test]
fn unstaged_skips_session_messages_but_keeps_driver_entries() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    assert!(
        queue.unstaged(&CommandId("c_a".into())).is_some(),
        "a normal driver entry waits for its hook pass"
    );
    let (id, sender) = session("c_m", "s_other");
    queue
        .admit_session_hooked(id.clone(), text("m"), sender, Staged::default(), &log)
        .expect("a fresh session id admits");
    assert!(
        queue.unstaged(&id).is_none(),
        "a session entry never takes the hook pass"
    );
}

#[test]
fn take_removes_and_forgets_the_entry() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    let taken = queue.take(&CommandId("c_a".into()), &log);
    assert!(
        taken.is_some_and(|entry| entry.id == CommandId("c_a".into())),
        "take returns the named entry"
    );
    assert!(
        queue.unstaged(&CommandId("c_a".into())).is_none(),
        "the taken entry is gone"
    );
    assert!(
        queue.snapshot().is_empty(),
        "the snapshot drops the taken entry"
    );
    assert!(
        !queue.is_admitted(&CommandId("c_a".into())),
        "a later drop for the taken id is stale"
    );
    assert_eq!(
        messages(&log).last().expect("the take emits"),
        &[],
        "the take emits the queue as the section left it"
    );
}

#[test]
fn take_staged_drains_only_staged_entries() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    queued(&queue, "c_b", "b", &log);
    assert!(
        queue.stage(&CommandId("c_a".into()), text("a"), Staged::default(), &log),
        "the first entry stages"
    );
    assert!(
        queue.stage(&CommandId("c_b".into()), text("b"), Staged::default(), &log),
        "the second entry stages"
    );
    let mut taken = queue
        .take_staged(&log)
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    taken.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        taken,
        [CommandId("c_a".into()), CommandId("c_b".into())],
        "both staged entries drain"
    );
    assert!(
        queue.snapshot().is_empty(),
        "no unstaged entry stayed behind"
    );
    assert_eq!(
        messages(&log).last().expect("the drain emits"),
        &[],
        "the emit lists the remainder"
    );
}

#[test]
fn an_admitted_id_drops_only_through_forget() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    assert!(
        queue.is_admitted(&CommandId("c_a".into())),
        "a pushed entry is admitted at once"
    );
    // The entry already left through the reader drop, which keeps its
    // id so the loop's follow-up delivery is a no-op; only `forget`
    // drops the id itself, emitting nothing.
    assert!(queue.remove(&CommandId("c_a".into()), &log));
    assert!(
        queue.is_admitted(&CommandId("c_a".into())),
        "a removed entry keeps its id"
    );
    queue.forget(&CommandId("c_a".into()));
    assert!(
        !queue.is_admitted(&CommandId("c_a".into())),
        "forget drops the id the entry left behind"
    );
    assert!(queue.snapshot().is_empty(), "forget touches no entry");
}

#[test]
fn take_staged_forgets_taken_person_ids() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_p", "person", &log);
    assert!(
        queue.stage(
            &CommandId("c_p".into()),
            text("person"),
            Staged::default(),
            &log
        ),
        "the person entry stages"
    );
    let (message_id, message_sender) = session("c_m", "s_other");
    queue
        .admit_session_hooked(
            message_id.clone(),
            text("m"),
            message_sender,
            Staged::default(),
            &log,
        )
        .expect("a fresh session id admits");
    let mut taken: Vec<CommandId> = queue
        .take_staged(&log)
        .into_iter()
        .map(|entry| entry.id)
        .collect();
    taken.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        taken,
        [CommandId("c_m".into()), CommandId("c_p".into())],
        "staged person and session entries both drain"
    );
    assert!(
        !queue.is_admitted(&CommandId("c_p".into())),
        "a drained person entry forgets its id, so a later drop is stale"
    );
    assert!(
        !queue.is_admitted(&message_id),
        "a session id was never admitted"
    );
}

#[test]
fn stage_emits_only_on_replacement() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    assert_eq!(messages(&log).len(), 1, "the push emits once");
    assert!(
        queue.stage(&CommandId("c_a".into()), text("a"), Staged::default(), &log),
        "staging the unchanged entry still applies"
    );
    assert_eq!(
        messages(&log).len(),
        1,
        "an unchanged staging re-emits nothing"
    );
    assert!(
        queue.stage(
            &CommandId("c_a".into()),
            text("changed"),
            Staged::default(),
            &log
        ),
        "staging the replacement still applies"
    );
    assert_eq!(messages(&log).len(), 2, "a replacement re-emits the queue");
    assert_eq!(
        messages(&log).last().expect("the replacement emits"),
        &queue.snapshot(),
        "the last event matches the final queue"
    );
}

#[test]
fn move_out_keeps_the_id_admitted() {
    let log = Recorded::default();
    let queue = Queue::new();
    queued(&queue, "c_a", "a", &log);
    let moved = queue.move_out(&CommandId("c_a".into()), &log);
    assert_eq!(
        moved.map(|entry| entry.id),
        Some(CommandId("c_a".into())),
        "move_out returns the queued entry"
    );
    assert!(queue.snapshot().is_empty(), "the entry leaves the queue");
    assert!(
        queue.is_admitted(&CommandId("c_a".into())),
        "the id stays admitted, so a same-wait drop still finds it"
    );
    assert_eq!(
        messages(&log).last().expect("the move emits"),
        &[],
        "the move emits the queue as it stands"
    );
}

#[test]
fn has_session_holds_only_while_a_session_message_waits() {
    let log = Recorded::default();
    let queue = Queue::new();
    assert!(
        !queue.has_session(),
        "an empty queue holds no session message"
    );
    queued(&queue, "c_a", "a", &log);
    queued(&queue, "c_b", "b", &log);
    assert!(
        !queue.has_session(),
        "persons-only entries are no session message"
    );
    let (id, sender) = session("c_m", "s_other");
    queue
        .admit_session_hooked(id, text("m"), sender, Staged::default(), &log)
        .expect("a fresh session id admits");
    assert!(queue.has_session(), "an admitted message holds");
}

#[test]
fn take_for_ending_keeps_only_session_messages() {
    let log = Recorded::default();
    let queue = Queue::new();
    let (message_id, message_sender) = session("c_m", "s_other");
    queue
        .admit_session_hooked(
            message_id,
            text("m"),
            message_sender,
            Staged::default(),
            &log,
        )
        .expect("a fresh session id admits");
    queued(&queue, "c_p", "person", &log);
    let taken = queue.take_for_ending(&log);
    assert_eq!(taken.len(), 1, "exactly the session message moves");
    assert_eq!(
        taken[0].content,
        text("m"),
        "the message comes out oldest-first"
    );
    assert!(
        !queue.is_admitted(&CommandId("c_p".into())),
        "the dropped person steer forgets its id"
    );
    assert!(
        queue.snapshot().is_empty(),
        "nothing stays queued after the ending take"
    );
    assert_eq!(
        messages(&log).last().expect("the ending take emits"),
        &[],
        "the emit lists the emptied queue"
    );
}
