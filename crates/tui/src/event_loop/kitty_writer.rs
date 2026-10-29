//! The Kitty image writer thread (`docs/tui.md`, "Images"): large uploads
//! go out in whole chunks while the loop keeps reading input and drawing.
//! The loop hands ordered items over a channel; a frame or other loop
//! write waits behind at most the chunk already in flight. A delete jumps
//! ahead of the queue at the next chunk boundary, so no write ever lands
//! inside another image's escape sequence.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Write};
use std::os::fd::AsFd;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use support::lock;

use crate::Input;

/// How long a barrier or handover ack waits: a dead ack sender fails fast,
/// so at most one bound expires on any path.
const ACK_WAIT: Duration = Duration::from_secs(10);

/// One delete: the image id it clears and its encoded bytes.
#[derive(Clone, Debug)]
pub(crate) struct Delete {
    pub(crate) id: u32,
    pub(crate) bytes: Vec<u8>,
}

/// One queued graphics item: an image's transmit chunks then its place, or
/// a delete. An image's chunks go out whole, in order, with nothing between
/// them but a delete; its place follows its last chunk.
#[derive(Debug)]
pub(crate) enum Item {
    Image {
        id: u32,
        chunks: Vec<Vec<u8>>,
        place: Vec<u8>,
    },
    Delete(Delete),
}

/// What the loop asks the writer to do, in send order.
#[derive(Debug)]
pub(crate) enum Command {
    Push(Item),
    Barrier(Sender<()>),
    Purge,
    Handover(Sender<Vec<u32>>),
    Resume,
    Stop,
}

/// The terminal-output lock: loop writes and writer chunks share the tty,
/// but a loop write never waits on a writer chunk past the chunk already
/// in flight, and a writer chunk never starts while a loop write waits.
/// The writer holds its chunk guard across one chunk's offset-tracking
/// writes and any write-readiness poll and retry, and for each place and
/// delete, so no other tty write can enter a partial escape sequence.
pub(crate) struct TtyLock {
    state: Mutex<State>,
    changed: Condvar,
    /// Test-only: signalled once `loop_write` registers its wait, so a test
    /// orders its drain after the wait the spin cannot order. Always `None`
    /// in production.
    #[cfg(test)]
    queued_hook: Mutex<Option<Sender<()>>>,
}

/// Who holds the tty: the writer for its chunk, place or delete, the loop
/// for its write, or nobody. The writer and the loop never hold it
/// together: one flag per holder could claim it twice.
#[derive(Default, PartialEq, Eq)]
enum TtyOwner {
    #[default]
    Free,
    Writer,
    Loop,
}

#[derive(Default)]
struct State {
    /// Who holds the tty for its current write.
    owner: TtyOwner,
    /// A loop write waits: the writer finishes at most its current chunk,
    /// then yields until the loop write completes.
    loop_waiting: bool,
}

impl TtyLock {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            #[cfg(test)]
            queued_hook: Mutex::new(None),
        }
    }

    /// Holds the tty for one writer chunk, place or delete: waits while a
    /// loop write waits or holds, so a queued loop write beats every later
    /// writer chunk.
    pub(crate) fn writer_chunk(&self) -> WriterChunkGuard<'_> {
        let mut state = lock(&self.state);
        while state.loop_waiting || state.owner != TtyOwner::Free {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.owner = TtyOwner::Writer;
        WriterChunkGuard { lock: self }
    }

    /// Holds the tty for one loop write: waits for at most the writer
    /// chunk already in flight, then holds ahead of every later chunk.
    pub(crate) fn loop_write(&self) -> LoopGuard<'_> {
        let mut state = lock(&self.state);
        state.loop_waiting = true;
        #[cfg(test)]
        if let Some(hook) = lock(&self.queued_hook).take() {
            let _ = hook.send(()).ok();
        }
        while state.owner == TtyOwner::Writer {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.owner = TtyOwner::Loop;
        LoopGuard { lock: self }
    }

    /// Arms the one-shot queued signal the next `loop_write` emits (see
    /// the `queued_hook` field): tests only.
    #[cfg(test)]
    pub(crate) fn set_queued_hook(&self, hook: Sender<()>) {
        *lock(&self.queued_hook) = Some(hook);
    }
}

/// The writer's hold on the tty for one chunk, place or delete.
pub(crate) struct WriterChunkGuard<'a> {
    lock: &'a TtyLock,
}

impl Drop for WriterChunkGuard<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.lock.state);
        state.owner = TtyOwner::Free;
        self.lock.changed.notify_all();
    }
}

/// The loop's hold on the tty for one write.
pub(crate) struct LoopGuard<'a> {
    lock: &'a TtyLock,
}

impl Drop for LoopGuard<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.lock.state);
        state.owner = TtyOwner::Free;
        state.loop_waiting = false;
        self.lock.changed.notify_all();
    }
}

/// A hold point at chunk boundaries: the gate parks the writer after a
/// chunk so a test can prove the loop draws while a payload is pending.
/// Production builds the gate open, so it never blocks; tests build it
/// closed and release it. The wait runs only after the writer-chunk guard
/// is released, so a hold never blocks loop output.
pub(crate) struct KittyChunkGate {
    state: Mutex<bool>,
    changed: Condvar,
}

impl KittyChunkGate {
    /// A gate that holds when `closed`, and never blocks otherwise.
    pub(crate) fn new(closed: bool) -> Self {
        Self {
            state: Mutex::new(closed),
            changed: Condvar::new(),
        }
    }

    /// Parks while the gate is closed.
    pub(crate) fn wait(&self) {
        let mut closed = lock(&self.state);
        while *closed {
            closed = self
                .changed
                .wait(closed)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// Opens the gate, freeing a parked writer.
    #[cfg(test)]
    pub(crate) fn release(&self) {
        *lock(&self.state) = false;
        self.changed.notify_all();
    }
}

/// The writer thread's stack, in bytes: the writer only formats escapes
/// and polls, so 1 MiB holds it with room to spare.
pub(crate) const KITTY_WRITER_STACK_SIZE: usize = 1 << 20;

/// The Kitty writer: a thread sending image uploads in whole chunks. The
/// loop pushes ordered items and deletes; the thread drops every pending
/// payload and exits on a write failure, on Stop, or when every sender is
/// gone. Shutdown joins it before the hub hangs up and the guard drops.
pub(crate) struct KittyWriter {
    tx: Sender<Command>,
    handle: Option<std::thread::JoinHandle<()>>,
    /// Test-only: signalled once after the next blocking-command send
    /// (`pause_for_handover`'s Handover, `stop_and_join`'s Stop), so a test
    /// can order a gate release after the send the blocking call cannot
    /// order (`docs/testing/mutants.md`: no outside seam observes the send;
    /// the gate is the race's pause point). Always `None` in production.
    #[cfg(test)]
    sent_hook: Mutex<Option<Sender<()>>>,
}

impl KittyWriter {
    /// Starts the writer on `tty`, holding `tty_lock` across each chunk.
    /// `failed` is set on a write failure, and `files_out` carries the
    /// failure to the loop before the thread exits. `None` when the thread
    /// cannot start: the loop takes the terminal-failure path.
    pub(crate) fn spawn(
        tty: File,
        gate: Arc<KittyChunkGate>,
        failed: Arc<AtomicBool>,
        files_out: Sender<Input>,
        tty_lock: Arc<TtyLock>,
    ) -> Option<Self> {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::Builder::new()
            // A 1 MiB stack: the writer only formats escapes and polls.
            .stack_size(KITTY_WRITER_STACK_SIZE)
            .name("kitty-writer".to_owned())
            .spawn(move || {
                command_loop(rx, tty, gate, failed, files_out, tty_lock);
            })
            .ok()?;
        Some(Self {
            tx,
            handle: Some(handle),
            #[cfg(test)]
            sent_hook: Mutex::new(None),
        })
    }

    /// Queues `item` behind what is pending, in order. False when the
    /// writer is gone: it already took the terminal-failure path.
    pub(crate) fn push(&self, item: Item) -> bool {
        self.tx.send(Command::Push(item)).is_ok()
    }

    /// Purges queued payloads before later items, keeping the writer's
    /// mandatory abort delete for a partial upload. False when the writer
    /// is gone: it already took the terminal-failure path.
    pub(crate) fn push_purge(&self) -> bool {
        self.tx.send(Command::Purge).is_ok()
    }

    /// Waits until every delete pushed so far is on the wire, so no frame
    /// overtakes a queued delete. False on a dead writer or an expired
    /// bound: the loop takes the terminal-failure path.
    pub(crate) fn barrier(&self) -> bool {
        let (ack, waited) = std::sync::mpsc::channel();
        if self.tx.send(Command::Barrier(ack)).is_err() {
            return false;
        }
        #[allow(
            clippy::disallowed_methods,
            reason = "production ack wait with a fixed 10 s bound; fakes::Deadline is dev-only"
        )]
        waited.recv_timeout(ACK_WAIT).is_ok()
    }

    /// Sends a barrier without waiting: tests order its ack around gate
    /// releases the blocking `barrier` cannot order.
    #[cfg(test)]
    pub(crate) fn barrier_send(&self) -> Receiver<()> {
        let (ack, waited) = std::sync::mpsc::channel();
        drop(self.tx.send(Command::Barrier(ack)));
        waited
    }

    /// Closes any partial upload with the protocol-defined abort delete at
    /// the next chunk boundary, pauses with pending image items retained,
    /// and returns the ids still held for replay. `None` when the writer
    /// is gone: the loop takes the terminal-failure path.
    pub(crate) fn pause_for_handover(&self) -> Option<Vec<u32>> {
        let (ack, waited) = std::sync::mpsc::channel();
        if self.tx.send(Command::Handover(ack)).is_err() {
            return None;
        }
        #[cfg(test)]
        if let Some(hook) = lock(&self.sent_hook).take() {
            let _ = hook.send(()).ok();
        }
        // No wall-clock bound: the ack waits for the current chunk and the
        // boundary abort delete, which wait as long as the blocking write
        // they replace.
        waited.recv().ok()
    }

    /// Arms the one-shot send signal the next blocking command emits (see
    /// the `sent_hook` field): tests only.
    #[cfg(test)]
    pub(crate) fn set_sent_hook(&self, hook: Sender<()>) {
        *lock(&self.sent_hook) = Some(hook);
    }

    /// Restarts the retained items at their first chunk after handover. A
    /// dead writer already took the terminal-failure path, so its result
    /// is ignored.
    pub(crate) fn resume_after_handover(&self) {
        drop(self.tx.send(Command::Resume));
    }

    /// Sends a handover without waiting: tests order its ack around gate
    /// releases the blocking `pause_for_handover` cannot order.
    #[cfg(test)]
    pub(crate) fn handover_send(&self) -> Receiver<Vec<u32>> {
        let (ack, waited) = std::sync::mpsc::channel();
        drop(self.tx.send(Command::Handover(ack)));
        waited
    }

    /// Drops every pending payload, acknowledges pending barriers and joins
    /// the thread. A partial upload emits its mandatory abort delete at the
    /// next boundary first.
    pub(crate) fn stop_and_join(mut self) {
        drop(self.tx.send(Command::Stop));
        #[cfg(test)]
        if let Some(hook) = lock(&self.sent_hook).take() {
            let _ = hook.send(()).ok();
        }
        if let Some(handle) = self.handle.take() {
            drop(handle.join());
        }
    }

    /// Sends Stop without joining: tests release the chunk gate between
    /// the send and the join, which `stop_and_join` cannot order.
    #[cfg(test)]
    pub(crate) fn stop_send(&self) {
        drop(self.tx.send(Command::Stop));
    }

    /// Joins the thread after `stop_send`.
    #[cfg(test)]
    pub(crate) fn join(mut self) {
        if let Some(handle) = self.handle.take() {
            drop(handle.join());
        }
    }
}

/// One image upload in flight: its whole chunks with the next one to send,
/// and its place. `next == chunks.len()` names a completed upload awaiting
/// its place: a delete arriving then drops the place without resending.
struct Active {
    id: u32,
    chunks: Vec<Vec<u8>>,
    next: usize,
    place: Vec<u8>,
}

/// Writes `bytes` whole: short writes and `Interrupted` resume at the byte
/// where they stopped, and `WouldBlock` polls the same tty for
/// `PollFlags::OUT` with no timeout, then resumes at that offset. A poll
/// error, any other write error or a zero-byte write is terminal failure:
/// the loop drops every pending payload and writes nothing further. The
/// caller holds the writer-chunk guard across the whole chunk and every
/// poll and retry, so no loop write can enter a partial escape. The
/// production tty is never set nonblocking and takes no `ioctl`.
fn write_chunk(out: &mut (impl Write + AsFd), bytes: &[u8]) -> Result<(), ()> {
    use rustix::event::{PollFd, PollFlags, poll};
    let mut at = 0;
    while at < bytes.len() {
        let rest = bytes.get(at..).ok_or(())?;
        match out.write(rest) {
            Ok(0) => return Err(()),
            Ok(n) => at += n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                loop {
                    let mut ready = [PollFd::new(&*out, PollFlags::OUT)];
                    match poll(&mut ready, None) {
                        // No timeout, so `Ok` means the single polled tty
                        // is ready: the return counts ready fds, and there
                        // is only one. No `revents` test: an empty `Ok`
                        // cannot happen without a timeout, so no test could
                        // pin one.
                        Ok(_) => break,
                        Err(rustix::io::Errno::INTR) => {}
                        Err(_) => return Err(()),
                    }
                }
            }
            Err(_) => return Err(()),
        }
    }
    Ok(())
}

/// The writer thread's state: the queued and in-flight uploads, the deletes
/// awaiting the next chunk boundary, and the channels to the loop.
struct Runner {
    rx: Receiver<Command>,
    tty: File,
    gate: Arc<KittyChunkGate>,
    failed: Arc<AtomicBool>,
    files_out: Sender<Input>,
    tty_lock: Arc<TtyLock>,
    pending: VecDeque<Active>,
    current: Option<Active>,
    deletes: Vec<Delete>,
    abort: Option<Delete>,
    barriers: Vec<Sender<()>>,
}

/// What the pause wait ended with.
enum Parked {
    Resume(Vec<Command>),
    Stop,
    Gone,
}

impl Runner {
    /// Handles one loop command. Sets `stopping` on Stop, and `handover`
    /// on Handover; everything else queues.
    fn handle(
        &mut self,
        cmd: Command,
        stopping: &mut bool,
        handover: &mut Option<Sender<Vec<u32>>>,
    ) {
        match cmd {
            Command::Push(Item::Image { id, chunks, place }) => {
                self.pending.push_back(Active {
                    id,
                    chunks,
                    next: 0,
                    place,
                });
            }
            Command::Push(Item::Delete(delete)) => {
                apply_delete(
                    delete,
                    &mut self.pending,
                    &mut self.current,
                    &mut self.deletes,
                );
            }
            Command::Barrier(ack) => self.barriers.push(ack),
            Command::Purge => {
                // A forgotten image never transmits from the remaining
                // queue: queued payloads go, while shown deletes still go
                // on the wire. A partial upload closes with its mandatory
                // abort delete at the next boundary; no later chunk or
                // place for it is sent. A later Purge cannot clear this
                // abort.
                self.pending.clear();
                if let Some(active) = self.current.take() {
                    self.abort = Some(Delete {
                        id: active.id,
                        bytes: crate::graphics::delete(active.id),
                    });
                }
            }
            Command::Handover(ack) => {
                *handover = Some(ack);
            }
            Command::Resume => {}
            Command::Stop => {
                *stopping = true;
            }
        }
    }

    /// Emits the mandatory abort delete and every queued delete, in order.
    /// Runs only at chunk boundaries with the writer-chunk guard released
    /// before and after, so a loop write can go between.
    fn emit_pending(&mut self) -> Result<(), ()> {
        if self.abort.is_none() && self.deletes.is_empty() {
            return Ok(());
        }
        let _guard = self.tty_lock.writer_chunk();
        if let Some(abort) = self.abort.take() {
            write_chunk(&mut self.tty, &abort.bytes)?;
        }
        for delete in self.deletes.drain(..) {
            write_chunk(&mut self.tty, &delete.bytes)?;
        }
        Ok(())
    }

    /// Acknowledges every barrier after its deletes reached the wire. An
    /// ack means every delete pushed before it is on the wire, not that
    /// payloads are drained.
    fn ack_barriers(&mut self) -> Result<(), ()> {
        for ack in self.barriers.drain(..) {
            ack.send(()).map_err(|_| ())?;
        }
        Ok(())
    }

    /// Takes the terminal-failure path: the flag the loop checks at frame
    /// top, the failure input waking an idle loop, and one ack for every
    /// pending barrier so no waiter hangs. Emits no cleanup bytes.
    fn fail(&self, barriers: Vec<Sender<()>>) {
        self.failed.store(true, Ordering::SeqCst);
        for ack in barriers {
            let _ = ack.send(()).ok();
        }
        drop(self.files_out.send(Input::KittyFailed));
    }

    /// Closes any partial upload with its abort delete, sends the ids still
    /// held for replay, then parks without writing until Resume or Stop.
    /// Returns what ended the pause.
    fn handover(&mut self, ack: Sender<Vec<u32>>) -> Parked {
        // The pause ack means the abort is on the wire and no upload is
        // open before the foreground program starts.
        if let Some(active) = self.current.take() {
            // `current` is set only immediately before the first chunk write
            // and command handling resumes only after that write advances
            // `next`, so a current item here always has `next > 0`; abort
            // logic needs only `next < chunks.len()` to tell a partial
            // upload from a completed one awaiting its place.
            if active.next < active.chunks.len() {
                let delete = crate::graphics::delete(active.id);
                let _guard = self.tty_lock.writer_chunk();
                if write_chunk(&mut self.tty, &delete).is_err() {
                    let barriers = std::mem::take(&mut self.barriers);
                    self.fail(barriers);
                    return Parked::Gone;
                }
            }
            self.pending.push_front(Active { next: 0, ..active });
        }
        if self.emit_pending().is_err() {
            let barriers = std::mem::take(&mut self.barriers);
            self.fail(barriers);
            return Parked::Gone;
        }
        if self.ack_barriers().is_err() {
            let barriers = std::mem::take(&mut self.barriers);
            self.fail(barriers);
            return Parked::Gone;
        }
        let ids: Vec<u32> = self.pending.iter().map(|active| active.id).collect();
        if ack.send(ids).is_err() {
            self.fail(Vec::new());
            return Parked::Gone;
        }
        // Commands that arrived with or during the pause wait in arrival
        // order: a second pause waits for after the resume, when its ack
        // can close what is open then. Every wait here receives: held
        // commands stay held until Resume or Stop ends the pause.
        let mut held: Vec<Command> = Vec::new();
        loop {
            match self.rx.recv() {
                Ok(Command::Resume) => return Parked::Resume(held),
                Ok(Command::Stop) => return Parked::Stop,
                Ok(
                    queued @ (Command::Push(_)
                    | Command::Barrier(_)
                    | Command::Purge
                    | Command::Handover(_)),
                ) => held.push(queued),
                // Every sender is gone: the loop is gone, so exit
                // silently without flagging it.
                Err(_) => return Parked::Gone,
            }
        }
    }
}

/// Applies a delete at the next chunk boundary. A queued transmit the
/// delete covers is dropped with its place, and nothing of it is sent. An
/// in-flight upload it covers gets no more chunks and its place is dropped.
/// An in-flight upload it does not cover is also aborted by kitty, so it
/// goes back to the front of the queue and is sent again whole after the
/// delete. A delete after the last chunk but before the place sees a
/// completed upload: covering drops the pending place, non-covering goes
/// first and the place follows, and neither resends.
fn apply_delete(
    delete: Delete,
    pending: &mut VecDeque<Active>,
    current: &mut Option<Active>,
    deletes: &mut Vec<Delete>,
) {
    pending.retain(|active| active.id != delete.id);
    if let Some(active) = current {
        if active.id == delete.id {
            *current = None;
        } else if active.next < active.chunks.len() {
            let replay = current.take().map(|mut active| {
                active.next = 0;
                active
            });
            if let Some(replay) = replay {
                pending.push_front(replay);
            }
        }
    }
    deletes.push(delete);
}

/// The writer thread: sends each image's chunks then its place, FIFO, with
/// deletes jumping ahead at chunk boundaries. Drops every pending payload
/// and exits on a write failure, on Stop, or when every sender is gone.
fn command_loop(
    rx: Receiver<Command>,
    tty: File,
    gate: Arc<KittyChunkGate>,
    failed: Arc<AtomicBool>,
    files_out: Sender<Input>,
    tty_lock: Arc<TtyLock>,
) {
    let mut runner = Runner {
        rx,
        tty,
        gate,
        failed,
        files_out,
        tty_lock,
        pending: VecDeque::new(),
        current: None,
        deletes: Vec::new(),
        abort: None,
        barriers: Vec::new(),
    };
    let mut held: Vec<Command> = Vec::new();
    loop {
        let mut stopping = false;
        let mut handover: Option<Sender<Vec<u32>>> = None;
        let mut cmds = std::mem::take(&mut held);
        cmds.extend(runner.rx.try_iter());
        for cmd in cmds {
            runner.handle(cmd, &mut stopping, &mut handover);
        }
        // The handover closes its upload with the boundary abort delete
        // before parking, so its pending deletes go out first. Stop skips
        // that emission: its teardown sends only the mandatory abort
        // delete, then drops the queued payloads and deletes.
        if handover.is_some() {
            if runner.emit_pending().is_err() {
                let barriers = std::mem::take(&mut runner.barriers);
                runner.fail(barriers);
                return;
            }
            if runner.ack_barriers().is_err() {
                let barriers = std::mem::take(&mut runner.barriers);
                runner.fail(barriers);
                return;
            }
        }
        if stopping {
            // Stop during a partial upload emits its mandatory abort
            // delete at this boundary, as Purge does, then drops the
            // queued payloads, acknowledges pending barriers and exits. It
            // sends no later chunk or place.
            // As in `Runner::handover`: a current item here always has
            // `next > 0`, so only `next < chunks.len()` distinguishes a
            // partial upload (mandatory abort delete) from a completed one
            // awaiting its place (no abort).
            if let Some(active) = runner.current.take()
                && active.next < active.chunks.len()
            {
                let delete = crate::graphics::delete(active.id);
                let _guard = runner.tty_lock.writer_chunk();
                if write_chunk(&mut runner.tty, &delete).is_err() {
                    let barriers = std::mem::take(&mut runner.barriers);
                    runner.fail(barriers);
                    return;
                }
            }
            runner.pending.clear();
            runner.deletes.clear();
            runner.abort = None;
            for ack in runner.barriers.drain(..) {
                let _ = ack.send(()).ok();
            }
            return;
        }
        if let Some(ack) = handover {
            match runner.handover(ack) {
                Parked::Resume(cmds) => {
                    held = cmds;
                }
                Parked::Stop => {
                    runner.pending.clear();
                    runner.deletes.clear();
                    runner.abort = None;
                    for ack in runner.barriers.drain(..) {
                        let _ = ack.send(()).ok();
                    }
                    return;
                }
                Parked::Gone => return,
            }
            continue;
        }
        if runner.emit_pending().is_err() {
            let barriers = std::mem::take(&mut runner.barriers);
            runner.fail(barriers);
            return;
        }
        if runner.ack_barriers().is_err() {
            let barriers = std::mem::take(&mut runner.barriers);
            runner.fail(barriers);
            return;
        }
        if runner.current.is_none() {
            runner.current = runner.pending.pop_front();
        }
        if runner.current.is_none() {
            // Nothing queued: park in the channel receive until the loop
            // hands more over. Every sender gone ends the thread.
            match runner.rx.recv() {
                Ok(cmd) => {
                    held.push(cmd);
                }
                Err(_) => return,
            }
            continue;
        }
        let Some(active) = runner.current.as_mut() else {
            continue;
        };
        if active.next < active.chunks.len() {
            let Some(chunk) = active.chunks.get(active.next) else {
                let barriers = std::mem::take(&mut runner.barriers);
                runner.fail(barriers);
                return;
            };
            let _guard = runner.tty_lock.writer_chunk();
            if write_chunk(&mut runner.tty, chunk).is_err() {
                let barriers = std::mem::take(&mut runner.barriers);
                runner.fail(barriers);
                return;
            }
            active.next += 1;
        } else {
            let _guard = runner.tty_lock.writer_chunk();
            if write_chunk(&mut runner.tty, &active.place).is_err() {
                let barriers = std::mem::take(&mut runner.barriers);
                runner.fail(barriers);
                return;
            }
            runner.current = None;
        }
        // The gate is consulted after each chunk before the next chunk or
        // place, never before an image's first chunk: the writer falls
        // through to the loop-top dispatch, which re-reads pending commands
        // before the next piece, so deletes and Purge take effect at that
        // boundary. The wait runs after the writer-chunk guard is released,
        // so a hold never blocks loop output.
        // `current` is `Some` here only after a chunk write: the place
        // path clears it above, and a fresh item writes its first chunk
        // in this same pass, so `next > 0` always holds with it.
        if runner.current.is_some() {
            runner.gate.wait();
        }
    }
}

#[cfg(test)]
#[path = "kitty_writer_tests.rs"]
mod tests;
