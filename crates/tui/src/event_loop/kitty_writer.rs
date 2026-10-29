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
}

#[derive(Default)]
struct State {
    /// The writer holds the tty for its current chunk, place or delete.
    writer_in_chunk: bool,
    /// A loop write waits: the writer finishes at most its current chunk,
    /// then yields until the loop write completes.
    loop_waiting: bool,
    /// A loop write holds the tty.
    loop_holding: bool,
}

impl TtyLock {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
        }
    }

    /// Holds the tty for one writer chunk, place or delete: waits while a
    /// loop write waits or holds, so a queued loop write beats every later
    /// writer chunk.
    pub(crate) fn writer_chunk(&self) -> WriterChunkGuard<'_> {
        let mut state = lock(&self.state);
        while state.loop_waiting || state.loop_holding {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.writer_in_chunk = true;
        WriterChunkGuard { lock: self }
    }

    /// Holds the tty for one loop write: waits for at most the writer
    /// chunk already in flight, then holds ahead of every later chunk.
    pub(crate) fn loop_write(&self) -> LoopGuard<'_> {
        let mut state = lock(&self.state);
        state.loop_waiting = true;
        while state.writer_in_chunk {
            state = self
                .changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.loop_holding = true;
        LoopGuard { lock: self }
    }

    /// Whether a loop write waits or holds: tests prove the waiter parked
    /// before draining the pipe, so the writer's next chunk meets it.
    #[cfg(test)]
    pub(crate) fn loop_waited(&self) -> bool {
        let state = lock(&self.state);
        state.loop_waiting || state.loop_holding
    }
}

/// The writer's hold on the tty for one chunk, place or delete.
pub(crate) struct WriterChunkGuard<'a> {
    lock: &'a TtyLock,
}

impl Drop for WriterChunkGuard<'_> {
    fn drop(&mut self) {
        let mut state = lock(&self.lock.state);
        state.writer_in_chunk = false;
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
        state.loop_holding = false;
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
        // The gate is consulted after each chunk before the next chunk or
        // place, never before an image's first chunk. After release the
        // writer re-reads pending commands before the next piece, so
        // deletes and Purge take effect at that boundary. The wait runs
        // after the writer-chunk guard is released.
        if runner
            .current
            .as_ref()
            .is_some_and(|active| active.next > 0)
        {
            runner.gate.wait();
            let mut stopping = false;
            let mut handover: Option<Sender<Vec<u32>>> = None;
            let cmds: Vec<Command> = runner.rx.try_iter().collect();
            for cmd in cmds {
                runner.handle(cmd, &mut stopping, &mut handover);
            }
            if stopping || handover.is_some() {
                if let Some(ack) = handover {
                    held.push(Command::Handover(ack));
                }
                if stopping {
                    held.push(Command::Stop);
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
    }
}

#[cfg(test)]
mod tests {
    //! Tests for the Kitty writer thread: FIFO transmit-then-place order,
    //! delete coverage at chunk boundaries, barrier acks, purge, Stop,
    //! handover replay, the offset-tracking chunk write, and loop-writer
    //! priority on the terminal-output lock.

    use super::*;
    use std::collections::VecDeque;
    use std::os::fd::{AsFd, BorrowedFd};

    /// One named wall-clock deadline for every blocking wait.
    const DEADLINE: Duration = Duration::from_secs(10);

    /// A pipe's read and write ends as files.
    fn pipe() -> (File, File) {
        let (read, write) = std::io::pipe().unwrap_or_else(|err| panic!("pipe: {err}"));
        (
            File::from(std::os::fd::OwnedFd::from(read)),
            File::from(std::os::fd::OwnedFd::from(write)),
        )
    }

    /// A writer on `tty` with `gate`, sharing the failure flag and the
    /// failure input with the test.
    fn spawn_writer(
        tty: File,
        gate: &Arc<KittyChunkGate>,
        tty_lock: &Arc<TtyLock>,
    ) -> (KittyWriter, Arc<AtomicBool>, Receiver<Input>) {
        let failed = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let writer = KittyWriter::spawn(
            tty,
            Arc::clone(gate),
            Arc::clone(&failed),
            tx,
            Arc::clone(tty_lock),
        )
        .expect("the writer starts");
        (writer, failed, rx)
    }

    /// An upload item for `id`: `full` 4096-byte chunks plus a `tail`-byte
    /// last chunk, with its place, and every byte the writer must send.
    fn image(id: u32, full: usize, tail: usize) -> (Item, Vec<u8>) {
        let payload = "B".repeat(full * 4096 + tail);
        let chunks = crate::graphics::transmit(id, &payload);
        let place = crate::graphics::place(id, 7, 3);
        let mut bytes = Vec::new();
        for chunk in &chunks {
            bytes.extend_from_slice(chunk);
        }
        bytes.extend_from_slice(&place);
        (Item::Image { id, chunks, place }, bytes)
    }

    /// The first chunk's length: what reaches the pipe before a held gate
    /// parks the writer.
    fn first_chunk_len(item: &Item) -> usize {
        match item {
            Item::Image { chunks, .. } => chunks[0].len(),
            Item::Delete(_) => panic!("an image's first chunk, not a delete"),
        }
    }

    /// A delete item for `id`.
    fn delete(id: u32) -> Item {
        Item::Delete(Delete {
            id,
            bytes: crate::graphics::delete(id),
        })
    }

    /// Reads exactly `n` bytes, then returns them with the read end so the
    /// test can close it. The test joins nothing: the thread only drops and
    /// exits after its send.
    fn read_exact(read: File, n: usize) -> Receiver<(Vec<u8>, File)> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("kitty-test-read".to_owned())
            .spawn(move || {
                let mut read = read;
                let mut buf = vec![0u8; n];
                std::io::Read::read_exact(&mut read, &mut buf)
                    .unwrap_or_else(|err| panic!("read {n}: {err}"));
                tx.send((buf, read)).unwrap_or(());
            })
            .unwrap_or_else(|err| panic!("spawn: {err}"));
        rx
    }

    /// The read bytes within the deadline.
    fn recv_bytes(rx: &Receiver<(Vec<u8>, File)>) -> (Vec<u8>, File) {
        match fakes::Deadline::after(DEADLINE).recv(rx) {
            Ok(done) => done,
            Err(err) => panic!("waited {DEADLINE:?} for tty bytes: {err}"),
        }
    }

    /// Sends a barrier and waits for its ack: every delete pushed before it
    /// is on the wire when this returns.
    fn flush(writer: &KittyWriter) {
        assert!(writer.barrier(), "the writer acks the barrier");
    }

    #[test]
    fn images_transmit_then_place_in_order() {
        let gate = Arc::new(KittyChunkGate::new(false));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (first, first_bytes) = image(1, 1, 10);
        let (second, second_bytes) = image(2, 0, 5);
        assert!(writer.push(first));
        assert!(writer.push(second));
        // Both images land whole, in order, with no other graphics command
        // between one image's chunks: the pipe holds the exact bytes.
        let rx = read_exact(read, first_bytes.len() + second_bytes.len());
        let (bytes, _read) = recv_bytes(&rx);
        assert_eq!(bytes, [first_bytes, second_bytes].concat());
    }

    #[test]
    fn a_delete_covering_a_queued_image_drops_it_whole() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (first, first_bytes) = image(1, 1, 10);
        let (second, _) = image(2, 0, 5);
        let head = first_chunk_len(&first);
        assert!(writer.push(first));
        // The first chunk goes out despite the held gate; the gate parks
        // the writer after it, so the queued image and the delete meet the
        // in-flight one at the next boundary.
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert!(writer.push(second));
        assert!(writer.push(delete(2)));
        gate.release();
        // The second image sends nothing: no chunk and no place of it
        // reaches the wire. The delete aborts the in-flight upload too, so
        // it goes back to the front and is sent again whole after it.
        let deleted = crate::graphics::delete(2);
        let rx = read_exact(read, deleted.len() + first_bytes.len());
        let (rest, read) = recv_bytes(&rx);
        assert_eq!(bytes, first_bytes[..head].to_vec());
        assert_eq!(rest, [deleted, first_bytes].concat());
        // Nothing else was kept: stopping joins the thread, so EOF proves
        // the total. A retain mutant resends the covered image after.
        writer.stop_and_join();
        assert!(read_rest(read).is_empty());
    }

    #[test]
    fn a_delete_covering_a_partial_upload_sends_no_more_chunks_and_no_place() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (first, first_bytes) = image(1, 1, 10);
        let head = first_chunk_len(&first);
        let chunk0 = first_bytes[..head].to_vec();
        assert!(writer.push(first));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, chunk0);
        assert!(writer.push(delete(1)));
        gate.release();
        // The covered upload gets no more chunks and its place is dropped:
        // only the delete follows the first chunk.
        let deleted = crate::graphics::delete(1);
        let rx = read_exact(read, deleted.len());
        let (rest, read) = recv_bytes(&rx);
        assert_eq!(rest, deleted);
        // No replay follows: stopping joins the thread, so EOF proves the
        // total. An inverted cover check resends the upload here.
        writer.stop_and_join();
        assert!(read_rest(read).is_empty());
    }

    #[test]
    fn a_delete_missing_a_partial_upload_replays_it_whole_after_the_delete() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (first, first_bytes) = image(1, 1, 10);
        let head = first_chunk_len(&first);
        assert!(writer.push(first));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        // The delete names no queued or in-flight image: the partial upload
        // is aborted by kitty too, so it goes back to the front of the
        // queue and is sent again whole after the delete.
        assert!(writer.push(delete(9)));
        gate.release();
        let deleted = crate::graphics::delete(9);
        let rx = read_exact(read, deleted.len() + first_bytes.len());
        let (rest, _read) = recv_bytes(&rx);
        assert_eq!(
            [bytes, rest].concat(),
            [first_bytes[..head].to_vec(), deleted, first_bytes].concat()
        );
    }

    #[test]
    fn a_covering_delete_before_the_place_drops_the_place() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (only, only_bytes) = image(1, 0, 5);
        let head = first_chunk_len(&only);
        assert_eq!(
            head,
            only_bytes.len() - crate::graphics::place(1, 7, 3).len()
        );
        assert!(writer.push(only));
        // The only chunk goes out; the held gate parks the writer before
        // its place.
        let rx = read_exact(read, head);
        let (_bytes, read) = recv_bytes(&rx);
        assert!(writer.push(delete(1)));
        gate.release();
        flush(&writer);
        // The covering delete drops the pending place: the place never
        // reaches the wire, and nothing is resent.
        let deleted = crate::graphics::delete(1);
        let rx = read_exact(read, deleted.len());
        let (rest, read) = recv_bytes(&rx);
        assert_eq!(rest, deleted);
        // No replay follows: stopping joins the thread, so EOF proves the
        // total. A boundary mutant replays the completed upload here.
        writer.stop_and_join();
        assert!(read_rest(read).is_empty());
    }

    #[test]
    fn a_non_covering_delete_before_the_place_goes_first() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (only, only_bytes) = image(1, 0, 5);
        let head = first_chunk_len(&only);
        assert!(writer.push(only));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert!(writer.push(delete(9)));
        gate.release();
        flush(&writer);
        // The delete goes first, then the place follows: no resend of the
        // completed upload.
        let deleted = crate::graphics::delete(9);
        let place = crate::graphics::place(1, 7, 3);
        let rx = read_exact(read, deleted.len() + place.len());
        let (rest, _read) = recv_bytes(&rx);
        assert_eq!(
            [bytes, rest].concat(),
            [only_bytes[..head].to_vec(), deleted, place].concat()
        );
    }

    #[test]
    fn queued_deletes_with_several_ids_emit_in_order_and_drop_covered() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        // Ids other than 0 or 1, so a constant-id mutant fails the exact
        // bytes. Behind the held first chunk queue two images; deletes cover
        // the partial upload and one queued image but not the other. Both
        // deletes go out in push order at the boundary, each covered image
        // sends nothing more, and the uncovered queued image still transmits
        // whole after them.
        let (seventh, seventh_bytes) = image(7, 1, 10);
        let (ninth, ninth_bytes) = image(9, 0, 5);
        let (eleventh, _) = image(11, 0, 5);
        let head = first_chunk_len(&seventh);
        assert!(writer.push(seventh));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, seventh_bytes[..head].to_vec());
        assert!(writer.push(ninth));
        assert!(writer.push(eleventh));
        assert!(writer.push(delete(7)));
        assert!(writer.push(delete(11)));
        gate.release();
        // The barrier ack proves both deletes reached the wire.
        flush(&writer);
        let seventh_deleted = crate::graphics::delete(7);
        let eleventh_deleted = crate::graphics::delete(11);
        let rx = read_exact(
            read,
            seventh_deleted.len() + eleventh_deleted.len() + ninth_bytes.len(),
        );
        let (rest, read) = recv_bytes(&rx);
        assert_eq!(
            rest,
            [seventh_deleted, eleventh_deleted, ninth_bytes].concat()
        );
        // Nothing else was kept: stopping joins the thread, so EOF proves
        // the total. A retain mutant flipping `!=` to `==` drops the
        // uncovered survivor instead of the covered images, so its replay
        // never arrives here.
        writer.stop_and_join();
        assert!(read_rest(read).is_empty());
    }

    #[test]
    fn a_barrier_acks_only_after_pending_deletes_emit() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (only, _) = image(1, 0, 5);
        let head = first_chunk_len(&only);
        assert!(writer.push(only));
        let rx = read_exact(read, head);
        let (_bytes, read) = recv_bytes(&rx);
        // The held gate parks the writer before the place; the covering
        // delete drops it, so the writer idles after the delete.
        assert!(writer.push(delete(1)));
        gate.release();
        // The ack means the delete is on the wire: it reads back at once,
        // with no wait.
        flush(&writer);
        rustix::fs::fcntl_setfl(&read, rustix::fs::OFlags::NONBLOCK)
            .unwrap_or_else(|err| panic!("nonblocking: {err}"));
        let mut rest = Vec::new();
        let mut chunk = [0u8; 64];
        loop {
            match std::io::Read::read(&mut &read, &mut chunk) {
                Ok(0) => break,
                Ok(n) => rest.extend_from_slice(&chunk[..n]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("tty: {error}"),
            }
        }
        assert_eq!(rest, crate::graphics::delete(1));
    }

    #[test]
    fn purge_mid_upload_aborts_once_and_sends_no_later_chunk_or_place() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (first, first_bytes) = image(1, 1, 10);
        let head = first_chunk_len(&first);
        let chunk0 = first_bytes[..head].to_vec();
        assert!(writer.push(first));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, chunk0);
        // Two forgets before the next frame: the second purge cannot clear
        // the abort the first recorded.
        assert!(writer.push_purge());
        assert!(writer.push_purge());
        gate.release();
        let aborted = crate::graphics::delete(1);
        let rx = read_exact(read, aborted.len());
        let (rest, _read) = recv_bytes(&rx);
        // Exactly one abort delete for the in-flight image, and no later
        // chunk or place.
        assert_eq!(rest, aborted);
    }

    #[test]
    fn purge_with_nothing_queued_writes_nothing() {
        let gate = Arc::new(KittyChunkGate::new(false));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        assert!(writer.push_purge());
        // The purge processes: the barrier ack proves it.
        flush(&writer);
        rustix::fs::fcntl_setfl(&read, rustix::fs::OFlags::NONBLOCK)
            .unwrap_or_else(|err| panic!("nonblocking: {err}"));
        let mut rest = Vec::new();
        let mut chunk = [0u8; 64];
        match std::io::Read::read(&mut &read, &mut chunk) {
            Ok(0) => {}
            Ok(n) => rest.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("tty: {error}"),
        }
        assert!(rest.is_empty());
    }

    #[test]
    fn stop_aborts_a_partial_upload() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (first, first_bytes) = image(1, 1, 10);
        let head = first_chunk_len(&first);
        let chunk0 = first_bytes[..head].to_vec();
        assert!(writer.push(first));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, chunk0);
        // Stop rides behind the held gate, after the barrier send: the
        // teardown meets the partial upload at the boundary, emits its
        // mandatory abort delete, acks the barrier and exits.
        let ack = writer.barrier_send();
        writer.stop_send();
        gate.release();
        match fakes::Deadline::after(DEADLINE).recv(&ack) {
            Ok(()) => {}
            Err(error) => panic!("waited {DEADLINE:?} for the barrier ack: {error}"),
        }
        let aborted = crate::graphics::delete(1);
        let rx = read_exact(read, aborted.len());
        let (rest, _read) = recv_bytes(&rx);
        assert_eq!(rest, aborted);
        writer.join();
    }

    #[test]
    fn stop_with_a_queued_payload_behind_a_held_gate_writes_nothing_further() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (first, first_bytes) = image(1, 1, 10);
        let (second, _) = image(2, 0, 5);
        let head = first_chunk_len(&first);
        let chunk0 = first_bytes[..head].to_vec();
        assert!(writer.push(first));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, chunk0);
        // A queued payload behind the held gate, and a delete covering the
        // partial upload: Stop frees the queue and returns with no abort
        // and no later chunk or place.
        assert!(writer.push(second));
        assert!(writer.push(delete(1)));
        let ack = writer.barrier_send();
        writer.stop_send();
        gate.release();
        match fakes::Deadline::after(DEADLINE).recv(&ack) {
            Ok(()) => {}
            Err(error) => panic!("waited {DEADLINE:?} for the barrier ack: {error}"),
        }
        writer.join();
        // Nothing follows the first chunk: the queue is freed.
        let mut read = read;
        rustix::fs::fcntl_setfl(&mut read, rustix::fs::OFlags::NONBLOCK)
            .unwrap_or_else(|err| panic!("nonblocking: {err}"));
        let mut rest = Vec::new();
        let mut chunk = [0u8; 64];
        match std::io::Read::read(&mut read, &mut chunk) {
            Ok(0) => {}
            Ok(n) => rest.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("tty: {error}"),
        }
        assert!(rest.is_empty());
    }

    #[test]
    fn handover_pauses_at_boundary_and_restarts_whole() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let marker_tty = write
            .try_clone()
            .unwrap_or_else(|err| panic!("clone: {err}"));
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (upload, full) = image(1, 2, 10);
        let head = first_chunk_len(&upload);
        assert!(writer.push(upload));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, full[..head].to_vec());
        // The pause waits for the boundary abort delete, then returns the
        // ids the writer still holds. The send rides ahead of the release
        // on this thread, so the pause meets the partial upload at the
        // boundary with no later chunk.
        let paused = writer.handover_send();
        gate.release();
        let ids = match fakes::Deadline::after(DEADLINE).recv(&paused) {
            Ok(ids) => ids,
            Err(error) => panic!("waited {DEADLINE:?} for the pause ack: {error}"),
        };
        assert_eq!(ids, vec![1]);
        // The foreground program runs between the pause and the resume:
        // its bytes land on the same sink in order.
        let mut marker_tty = marker_tty;
        marker_tty
            .write_all(b"\x1b[?1049l")
            .unwrap_or_else(|err| panic!("1049l: {err}"));
        marker_tty
            .write_all(b"\x1b[?1049h")
            .unwrap_or_else(|err| panic!("1049h: {err}"));
        marker_tty
            .write_all(b"FRAME")
            .unwrap_or_else(|err| panic!("frame: {err}"));
        writer.resume_after_handover();
        // The writer sends the retained image whole from chunk zero, with
        // no abandoned remainder.
        let aborted = crate::graphics::delete(1);
        let markers = b"\x1b[?1049l\x1b[?1049hFRAME".to_vec();
        let rx = read_exact(read, aborted.len() + markers.len() + full.len());
        let (rest, _read) = recv_bytes(&rx);
        assert_eq!(
            [bytes, rest].concat(),
            [full[..head].to_vec(), aborted, markers, full].concat()
        );
        writer.stop_and_join();
    }

    #[test]
    fn a_failed_write_drops_payloads_and_reports_failure() {
        let gate = Arc::new(KittyChunkGate::new(false));
        let lock = Arc::new(TtyLock::new());
        // A read-only tty: every write fails, like a dead terminal.
        let tty = File::open("/dev/null").unwrap_or_else(|err| panic!("/dev/null: {err}"));
        let (writer, failed, rx) = spawn_writer(tty, &gate, &lock);
        let (first, _) = image(1, 1, 10);
        let (second, _) = image(2, 0, 5);
        assert!(writer.push(first));
        assert!(writer.push(second));
        // The failure input arrives, and the flag is set: the loop takes
        // the terminal-failure path.
        match fakes::Deadline::after(DEADLINE).recv(&rx) {
            Ok(Input::KittyFailed) => {}
            Ok(_) => panic!("a failure, not KittyFailed"),
            Err(err) => panic!("waited {DEADLINE:?} for the failure: {err}"),
        }
        assert!(failed.load(Ordering::SeqCst));
        // A dead ack receiver takes the terminal-failure path too.
        assert!(!writer.barrier());
    }

    /// A scripted tty for the offset write: pops a canned result per write,
    /// records every byte handed over, and polls on `/dev/null`, which is
    /// always ready.
    struct Scripted {
        script: VecDeque<io::Result<usize>>,
        written: Vec<u8>,
        probe: File,
    }

    impl Scripted {
        fn new(script: Vec<io::Result<usize>>) -> Self {
            Self {
                script: script.into(),
                written: Vec::new(),
                probe: File::open("/dev/null").unwrap_or_else(|err| panic!("/dev/null: {err}")),
            }
        }
    }

    impl Write for Scripted {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            match self.script.pop_front().expect("the script has a step") {
                Ok(n) => {
                    let n = n.min(bytes.len());
                    self.written.extend_from_slice(&bytes[..n]);
                    Ok(n)
                }
                Err(error) => Err(error),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl AsFd for Scripted {
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.probe.as_fd()
        }
    }

    #[test]
    fn write_chunk_resumes_short_writes_and_interrupts_at_the_offset() {
        let bytes = b"HELLO WORLD";
        let mut tty = Scripted::new(vec![
            Err(io::ErrorKind::Interrupted.into()),
            Ok(3),
            Ok(2),
            Err(io::ErrorKind::Interrupted.into()),
            Ok(11),
        ]);
        assert!(write_chunk(&mut tty, bytes).is_ok());
        // Every byte lands once, in order: no duplicated prefix and no lost
        // tail, whatever the interruptions. Treating `Interrupted` as a
        // failure, or restarting the chunk instead of resuming at the
        // offset, fails this.
        assert_eq!(tty.written, bytes);
        assert!(tty.script.is_empty());
    }

    #[test]
    fn write_chunk_zero_byte_write_is_terminal_failure() {
        let mut tty = Scripted::new(vec![Ok(3), Ok(0)]);
        assert!(write_chunk(&mut tty, b"HELLO").is_err());
    }

    #[test]
    fn write_chunk_polls_on_wouldblock_and_resumes_where_it_stopped() {
        let (read, write) = pipe();
        rustix::fs::fcntl_setfl(&write, rustix::fs::OFlags::NONBLOCK)
            .unwrap_or_else(|err| panic!("nonblocking: {err}"));
        // More than the pipe holds: the first write takes a partial count,
        // the next would block, and draining the pipe wakes the poll for
        // `PollFlags::OUT`, which resumes at the same byte.
        let bytes = vec![b'W'; 70 * 1024];
        let len = bytes.len();
        let (done, finished) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("kitty-test-drain".to_owned())
            .spawn(move || {
                let mut read = read;
                let mut got = vec![0u8; len];
                std::io::Read::read_exact(&mut read, &mut got)
                    .unwrap_or_else(|err| panic!("drain: {err}"));
                done.send(got).unwrap_or(());
            })
            .unwrap_or_else(|err| panic!("spawn: {err}"));
        let mut write = write;
        assert!(write_chunk(&mut write, &bytes).is_ok());
        // The exact bytes land, with no duplicated prefix: a resume from
        // the chunk's start instead of the offset fails this.
        match fakes::Deadline::after(DEADLINE).recv(&finished) {
            Ok(got) => assert_eq!(got, bytes),
            Err(err) => panic!("waited {DEADLINE:?} for the drain: {err}"),
        }
    }

    #[test]
    fn kitty_writer_yields_to_waiting_loop() {
        let lock = Arc::new(TtyLock::new());
        let gate = Arc::new(KittyChunkGate::new(false));
        let (mut read, mut write) = pipe();
        // Fill the pipe with zeros, leaving exactly 100 bytes free: the
        // writer's first chunk writes what fits, then blocks holding its
        // chunk guard while the loop waiter queues behind it.
        rustix::fs::fcntl_setfl(&write, rustix::fs::OFlags::NONBLOCK)
            .unwrap_or_else(|err| panic!("nonblocking: {err}"));
        let filler = vec![0u8; 65536];
        let mut filled = 0;
        loop {
            match write.write(&filler) {
                Ok(n) => filled += n,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("fill: {error}"),
            }
        }
        let mut free = [0u8; 100];
        std::io::Read::read_exact(&mut read, &mut free).unwrap_or_else(|err| panic!("free: {err}"));
        let frame_tty = write
            .try_clone()
            .unwrap_or_else(|err| panic!("clone: {err}"));
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        let (upload, full) = image(1, 1, 10);
        assert!(writer.push(upload));
        // Give the writer first scheduling: it blocks mid-chunk holding
        // the guard.
        for _ in 0..8 {
            std::thread::yield_now();
        }
        // Queue the loop waiter before draining: with no drain the writer
        // cannot complete its chunk, so the waiter either parks behind it
        // or, if the writer has not started, writes first. Either way the
        // marker precedes the writer's next chunk. The spin ends when the
        // waiter parked or finished; the cap fails the test instead of
        // hanging it.
        let marker = b"<<FRAME>>";
        let (parked, waiter_done) = std::sync::mpsc::channel();
        let waiter = std::thread::Builder::new()
            .name("kitty-test-waiter".to_owned())
            .spawn({
                let lock = Arc::clone(&lock);
                move || {
                    let _guard = lock.loop_write();
                    let mut frame_tty = frame_tty;
                    frame_tty
                        .write_all(marker)
                        .unwrap_or_else(|err| panic!("marker: {err}"));
                    let _ = parked.send(()).ok();
                }
            })
            .unwrap_or_else(|err| panic!("spawn: {err}"));
        let mut queued = false;
        for _ in 0..10_000_000 {
            if waiter_done.try_recv().is_ok() || lock.loop_waited() {
                queued = true;
                break;
            }
            std::thread::yield_now();
        }
        assert!(queued, "the waiter queued before the drain");
        // Drain: whatever order the threads reached, the marker lands
        // atomically between whole pieces, and priority puts it before the
        // writer's next chunk.
        let rx = read_exact(read, filled - 100 + full.len() + marker.len());
        let (tail, _read) = recv_bytes(&rx);
        let at_marker = tail
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("the marker is on the wire");
        let second = b"\x1b_Gm=0;";
        let at_second = tail
            .windows(second.len())
            .position(|window| window == second)
            .expect("the second chunk is on the wire");
        assert!(
            at_marker < at_second,
            "the frame marker precedes the writer's next chunk"
        );
        waiter.join().expect("the waiter joined");
        writer.stop_and_join();
    }

    /// A queued loop write beats every later writer chunk: the chunk
    /// parks while a loop waiter is queued, even before the waiter
    /// holds, so a steady stream of chunks can never starve a loop
    /// write. Each round parks a waiter behind the main-held chunk
    /// guard, drops it, and asks for the next chunk on the same
    /// running thread: the woken waiter still needs scheduling, so the
    /// reacquire meets the queued wait. Without the waiter check the
    /// reacquire succeeds while the waiter has not finished, and the
    /// round fails.
    #[test]
    fn tty_lock_queued_loop_write_beats_later_writer_chunk() {
        let lock = Arc::new(TtyLock::new());
        let (_read, write) = pipe();
        let marker = b"<<FRAME>>";
        for _ in 0..20 {
            let guard = lock.writer_chunk();
            let frame_tty = write
                .try_clone()
                .unwrap_or_else(|err| panic!("clone: {err}"));
            let (done, finished) = std::sync::mpsc::channel();
            let waiter = std::thread::Builder::new()
                .name("kitty-test-waiter".to_owned())
                .spawn({
                    let lock = Arc::clone(&lock);
                    move || {
                        let _guard = lock.loop_write();
                        let mut frame_tty = frame_tty;
                        frame_tty
                            .write_all(marker)
                            .unwrap_or_else(|err| panic!("marker: {err}"));
                        let _ = done.send(()).ok();
                    }
                })
                .unwrap_or_else(|err| panic!("spawn: {err}"));
            // The waiter parked behind the main-held chunk: waiting is
            // set, and it cannot have finished while the guard is held.
            // The cap fails the test instead of hanging it.
            let mut queued = false;
            for _ in 0..10_000_000 {
                if finished.try_recv().is_ok() {
                    panic!("the waiter finished before it could park");
                }
                if lock.loop_waited() {
                    queued = true;
                    break;
                }
                std::thread::yield_now();
            }
            assert!(queued, "the waiter queued before the drop");
            // Drop on this running thread, then ask for the next chunk
            // at once: the woken waiter still has to be scheduled, so
            // the reacquire meets the queued wait. Without the waiter
            // check it succeeds while the waiter has not finished.
            drop(guard);
            let _next = lock.writer_chunk();
            assert!(
                finished.try_recv().is_ok(),
                "the next chunk waited for the queued loop write"
            );
            drop(_next);
            waiter.join().expect("the waiter joined");
        }
    }

    #[test]
    fn kitty_writer_stack_is_one_mib() {
        // The plan names the exact stack: a `<<` to `>>` mutant leaves 0.
        assert_eq!(KITTY_WRITER_STACK_SIZE, 1_048_576);
    }

    /// Reads to end-of-file: only joined writers reach here, so the
    /// thread's tty clone is gone and the read end sees EOF. A live
    /// thread would hang this instead.
    fn read_rest(read: File) -> Vec<u8> {
        let mut rest = Vec::new();
        std::io::Read::read_to_end(&mut &read, &mut rest)
            .unwrap_or_else(|err| panic!("tty eof: {err}"));
        rest
    }

    #[test]
    fn pause_for_handover_returns_empty_when_nothing_is_held() {
        let gate = Arc::new(KittyChunkGate::new(false));
        let lock = Arc::new(TtyLock::new());
        let (_read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        // Nothing pushed: the pause acks at once with no ids. `None` and
        // the wrong singletons fail this; the empty-vector mutant is
        // killed by the retained-ids test below.
        assert_eq!(writer.pause_for_handover(), Some(vec![]));
        writer.resume_after_handover();
        writer.stop_and_join();
    }

    #[test]
    fn pause_for_handover_retains_queued_ids_in_order() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        // Ids other than 0 or 1: a constant-ids mutant fails the exact
        // vector, and `None` fails the `Some`.
        let (seventh, seventh_bytes) = image(7, 1, 10);
        let (ninth, ninth_bytes) = image(9, 0, 5);
        let head = first_chunk_len(&seventh);
        assert!(writer.push(seventh));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, seventh_bytes[..head].to_vec());
        // The second image queues behind the parked first: both are
        // retained, in order. The send hook orders the release after the
        // Handover the blocking pause cannot order, so the pause meets the
        // partial upload at the boundary on every run.
        assert!(writer.push(ninth));
        let (sent, sent_rx) = std::sync::mpsc::channel();
        writer.set_sent_hook(sent);
        let ids = std::thread::scope(|scope| {
            let paused = scope.spawn(|| writer.pause_for_handover());
            match fakes::Deadline::after(DEADLINE).recv(&sent_rx) {
                Ok(()) => {}
                Err(error) => panic!("waited {DEADLINE:?} for the handover send: {error}"),
            }
            gate.release();
            paused.join().expect("the pause joined")
        });
        assert_eq!(ids, Some(vec![7, 9]));
        writer.resume_after_handover();
        // The abort closes the partial upload, then both images go whole
        // from their first chunks.
        let aborted = crate::graphics::delete(7);
        let rx = read_exact(
            read,
            aborted.len() + seventh_bytes.len() + ninth_bytes.len(),
        );
        let (rest, _read) = recv_bytes(&rx);
        assert_eq!(rest, [aborted, seventh_bytes, ninth_bytes].concat());
        writer.stop_and_join();
    }

    #[test]
    fn handover_of_a_completed_upload_sends_no_abort() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        // One chunk, parked before its place: `next == chunks.len()`, so
        // no upload is open. A `<` to `<=` mutant emits a spurious abort
        // delete here.
        let (only, only_bytes) = image(5, 0, 5);
        let head = first_chunk_len(&only);
        assert!(writer.push(only));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, only_bytes[..head].to_vec());
        let paused = writer.handover_send();
        gate.release();
        let ids = match fakes::Deadline::after(DEADLINE).recv(&paused) {
            Ok(ids) => ids,
            Err(error) => panic!("waited {DEADLINE:?} for the pause ack: {error}"),
        };
        assert_eq!(ids, vec![5]);
        writer.resume_after_handover();
        // No abort: the retained image goes whole from its first chunk.
        let rx = read_exact(read, only_bytes.len());
        let (rest, _read) = recv_bytes(&rx);
        assert_eq!(rest, only_bytes);
        writer.stop_and_join();
    }

    #[test]
    fn stop_after_a_completed_upload_sends_no_abort() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        // Parked before the place: Stop drops the queue with no abort
        // delete. A `<` to `<=` mutant writes one for the completed
        // upload instead.
        let (only, only_bytes) = image(5, 0, 5);
        let head = first_chunk_len(&only);
        assert!(writer.push(only));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, only_bytes[..head].to_vec());
        let ack = writer.barrier_send();
        writer.stop_send();
        gate.release();
        match fakes::Deadline::after(DEADLINE).recv(&ack) {
            Ok(()) => {}
            Err(error) => panic!("waited {DEADLINE:?} for the barrier ack: {error}"),
        }
        writer.join();
        // Nothing follows the first chunk: no abort, no place. The join
        // closed the tty, so the read end sees EOF.
        assert!(read_rest(read).is_empty());
    }

    #[test]
    fn stop_and_join_aborts_and_joins() {
        let gate = Arc::new(KittyChunkGate::new(true));
        let lock = Arc::new(TtyLock::new());
        let (read, write) = pipe();
        let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
        // A partial upload behind the held gate: Stop's abort delete is
        // the last thing the thread sends before it is joined. A
        // `stop_and_join` to `()` mutant sends nothing and joins nothing.
        let (first, first_bytes) = image(3, 1, 10);
        let head = first_chunk_len(&first);
        assert!(writer.push(first));
        let rx = read_exact(read, head);
        let (bytes, read) = recv_bytes(&rx);
        assert_eq!(bytes, first_bytes[..head].to_vec());
        // The send hook orders the release after the Stop the blocking
        // join cannot order, so Stop meets the partial upload at the
        // boundary on every run.
        let (sent, sent_rx) = std::sync::mpsc::channel();
        writer.set_sent_hook(sent);
        std::thread::scope(|scope| {
            let stopping = scope.spawn(|| writer.stop_and_join());
            match fakes::Deadline::after(DEADLINE).recv(&sent_rx) {
                Ok(()) => {}
                Err(error) => panic!("waited {DEADLINE:?} for the stop send: {error}"),
            }
            gate.release();
            stopping.join().expect("stop joined");
        });
        // Exactly the abort delete follows, then EOF: the thread was
        // joined, so its tty clone is gone.
        let aborted = crate::graphics::delete(3);
        let rx = read_exact(read, aborted.len());
        let (rest, read) = recv_bytes(&rx);
        assert_eq!(rest, aborted);
        assert!(read_rest(read).is_empty());
    }
}
