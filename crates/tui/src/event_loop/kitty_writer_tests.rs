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
    // docs/tui.md:2529-2530 ("Images"): images transmit and place in
    // the order queued, with no other image command between one
    // image's chunks.
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
    // docs/tui.md:2529-2532 ("Images"): the delete goes at the next
    // chunk boundary; the covered image sends nothing more, and the
    // interrupted upload is sent again whole after it.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
    assert_eq!(bytes, chunk0);
    assert!(writer.push(delete(1)));
    gate.release();
    // The covered upload gets no more chunks and its place is dropped:
    // only the delete follows the first chunk.
    let deleted = crate::graphics::delete(1);
    let rx = read_exact(read, deleted.len());
    let (rest, read) = recv_bytes(&rx);
    // docs/tui.md:2531-2532 ("Images"): an image the delete removes
    // sends nothing more.
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
    // docs/tui.md:2531-2532 ("Images"): an unrelated upload the delete
    // interrupts is sent again whole after it.
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
    // docs/tui.md:2531-2532 ("Images"): an image the delete removes
    // sends nothing more.
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
    // docs/tui.md:2529-2532 ("Images"): the delete goes at the next
    // chunk boundary, then the place follows: no resend of the
    // completed upload.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
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
    // docs/tui.md:2529-2532 ("Images"): both deletes go out in push
    // order at the boundary, covered images send nothing more, and the
    // uncovered image transmits whole after them.
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
    // with no wait. docs/tui.md:2530-2531 ("Images"): the delete goes
    // at the next chunk boundary.
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
    // chunk or place. docs/tui.md:2529-2532 ("Images"): a forgotten
    // image's partial upload closes with its delete at the next chunk
    // boundary, and nothing more of it is sent.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
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
    // docs/tui.md:2532-2534 ("Images"): quitting while an image
    // transmits sends its delete at the next chunk boundary and nothing
    // more of it.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
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
    // docs/tui.md:2529 ("Images"): the retained ids keep the queue order.
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
    // docs/tui.md:2548-2551 ("Images"): handover closes the partial
    // upload with its delete at the boundary, and the retained image is
    // sent again whole from chunk zero.
    assert_eq!(
        [bytes, rest].concat(),
        [full[..head].to_vec(), aborted, markers, full].concat()
    );
    writer.stop_and_join();
}

#[test]
fn a_failed_write_drops_payloads_and_reports_failure() {
    let gate = Arc::new(KittyChunkGate::new(true));
    let lock = Arc::new(TtyLock::new());
    let (read, write) = pipe();
    let (writer, failed, rx) = spawn_writer(write, &gate, &lock);
    let (first, first_bytes) = image(1, 1, 10);
    let (second, _) = image(2, 0, 5);
    let head = first_chunk_len(&first);
    assert!(writer.push(first));
    // The first chunk goes out despite the held gate; the gate parks
    // the writer after it, so the second image queues before any
    // failure.
    let chunk_rx = read_exact(read, head);
    let (bytes, read) = recv_bytes(&chunk_rx);
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
    assert_eq!(bytes, first_bytes[..head].to_vec());
    assert!(writer.push(second));
    // Closing the read end fails the next write after both images were
    // queued, like a dead terminal.
    drop(read);
    gate.release();
    // The failure input arrives, and the flag is set: the loop takes
    // the terminal-failure path.
    match fakes::Deadline::after(DEADLINE).recv(&rx) {
        Ok(Input::KittyFailed) => {}
        Ok(_) => panic!("a failure, not KittyFailed"),
        Err(err) => panic!("waited {DEADLINE:?} for the failure: {err}"),
    }
    assert!(failed.load(Ordering::SeqCst));
    // A dead ack receiver takes the terminal-failure path too.
    // docs/tui.md:2534-2535 ("Images"): a failed write to the terminal
    // is the terminal failing.
    assert!(!writer.barrier());
    writer.join();
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
    // A closed gate parks the writer after its first chunk: the
    // first-chunk read below is the pause point that orders writer
    // progress before the waiter, with no thread-scheduling hold. The
    // pipe stays blocking with room to spare, so the marker never meets
    // a full pipe (`Resource temporarily unavailable`, os error 11).
    let gate = Arc::new(KittyChunkGate::new(true));
    let (read, write) = pipe();
    let marker_tty = write
        .try_clone()
        .unwrap_or_else(|err| panic!("clone: {err}"));
    let (writer, _failed, _rx) = spawn_writer(write, &gate, &lock);
    let (upload, full) = image(1, 1, 10);
    let head = first_chunk_len(&upload);
    assert!(writer.push(upload));
    // Reader progress before any marker write: this read proves the
    // writer reached the boundary and parked. The deadline fails the
    // test instead of hanging it.
    let rx = read_exact(read, head);
    let (first, read) = recv_bytes(&rx);
    assert_eq!(first, full[..head].to_vec());
    // Queue the loop waiter behind the parked writer: it holds the loop
    // write across the marker, so the writer's next chunk parks behind
    // it. The queued signal proves the waiter registered its loop write
    // and the acquired signal proves it holds ahead of the next chunk;
    // the deadlines fail the test instead of hanging it.
    let marker = b"<<FRAME>>";
    let (queued_tx, queued_rx) = std::sync::mpsc::channel();
    lock.set_queued_hook(queued_tx);
    let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let waiter = std::thread::Builder::new()
        .name("kitty-test-waiter".to_owned())
        .spawn({
            let lock = Arc::clone(&lock);
            move || {
                let _guard = lock.loop_write();
                acquired_tx.send(()).unwrap_or(());
                // Hold the loop guard while waiting for the drain below:
                // the writer's next chunk parks behind this queued write.
                match fakes::Deadline::after(DEADLINE).recv(&go_rx) {
                    Ok(()) => {}
                    Err(error) => panic!("waited {DEADLINE:?} for the drain: {error}"),
                }
                let mut marker_tty = marker_tty;
                marker_tty
                    .write_all(marker)
                    .unwrap_or_else(|err| panic!("marker: {err}"));
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match fakes::Deadline::after(DEADLINE).recv(&queued_rx) {
        Ok(()) => {}
        Err(error) => panic!("waited {DEADLINE:?} for the waiter to queue: {error}"),
    }
    match fakes::Deadline::after(DEADLINE).recv(&acquired_rx) {
        Ok(()) => {}
        Err(error) => panic!("waited {DEADLINE:?} for the waiter to hold: {error}"),
    }
    // Drain: the drain thread exists before the marker write — spawn
    // order plus the `go` handshake put reader before marker with no
    // scheduling assumption — and blocks for the marker and the writer's
    // remainder. Priority puts the marker before the writer's next chunk.
    let rx = read_exact(read, marker.len() + full.len() - head);
    go_tx.send(()).unwrap_or(());
    gate.release();
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
    // docs/tui.md:2525-2528 ("Images"): frames go out between whole
    // chunks, so the queued loop write beats the writer's next chunk.
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
        let (queued_tx, queued_rx) = std::sync::mpsc::channel();
        lock.set_queued_hook(queued_tx);
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
        // The queued signal proves the waiter parked behind the
        // main-held chunk: it cannot have finished while the guard is
        // held. The deadline fails the test instead of hanging it.
        match fakes::Deadline::after(DEADLINE).recv(&queued_rx) {
            Ok(()) => {}
            Err(error) => panic!("waited {DEADLINE:?} for the waiter to queue: {error}"),
        }
        assert!(
            finished.try_recv().is_err(),
            "the waiter finished before it could park"
        );
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

/// `loop_write` on a free tty acquires it at once: the wait parks
/// only behind a writer chunk. The wait names the writer-held flag
/// directly, so no comparison flip can park it here; the deadline
/// fails the test instead of hanging it.
#[test]
fn loop_write_acquires_a_free_tty_without_waiting() {
    let lock = Arc::new(TtyLock::new());
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("kitty-test-free".to_owned())
        .spawn({
            let lock = Arc::clone(&lock);
            move || {
                let _guard = lock.loop_write();
                let _ = done.send(()).ok();
            }
        })
        .unwrap_or_else(|err| panic!("spawn: {err}"));
    match fakes::Deadline::after(DEADLINE).recv(&finished) {
        Ok(()) => {}
        Err(error) => panic!("waited {DEADLINE:?} for loop_write on a free tty: {error}"),
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
    // docs/tui.md:2548-2551 ("Images"): nothing held means no ids to
    // replay whole on return.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
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
    // docs/tui.md:2529 ("Images"): the retained ids keep the queue order.
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
    // docs/tui.md:2548-2551 ("Images"): handover closes the partial
    // upload with its delete at the boundary, and both retained images
    // go whole from their first chunks.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
    assert_eq!(bytes, only_bytes[..head].to_vec());
    let paused = writer.handover_send();
    gate.release();
    let ids = match fakes::Deadline::after(DEADLINE).recv(&paused) {
        Ok(ids) => ids,
        Err(error) => panic!("waited {DEADLINE:?} for the pause ack: {error}"),
    };
    // docs/tui.md:2529 ("Images"): the retained ids keep the queue order.
    assert_eq!(ids, vec![5]);
    writer.resume_after_handover();
    // No abort: the retained image goes whole from its first chunk.
    let rx = read_exact(read, only_bytes.len());
    let (rest, _read) = recv_bytes(&rx);
    // docs/tui.md:2550-2551 ("Images"): every image on screen is sent
    // again whole: no abort for the completed upload.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
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
    // docs/tui.md:2529 ("Images"): images transmit in the order queued.
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
    // docs/tui.md:2532-2534 ("Images"): quitting while an image
    // transmits sends its delete at the next chunk boundary.
    assert_eq!(rest, aborted);
    assert!(read_rest(read).is_empty());
}
