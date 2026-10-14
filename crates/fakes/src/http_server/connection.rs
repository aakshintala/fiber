//! One accepted connection's bounded transport (`docs/testing.md`,
//! "Fakes"): request reads on the accept window's budget, replies,
//! stalls, pauses and the CONNECT tunnel. It owns no listener, registry
//! or adapter script state.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::fd::AsFd;
#[cfg(test)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex, atomic::Ordering, mpsc};
use std::time::Duration;

#[cfg(test)]
use crate::deadline::Deadline;

use super::{READ_CHUNK, RequestBudget, Shared, WaitEnd, expired, guard_expired, lock};

/// Maximum injected-clock time a full reply waits for the client to finish
/// its write side.
pub(crate) const REPLY_DRAIN_WITHIN: Duration = Duration::from_millis(100);
const REPLY_DRAIN_PHASE: &str = "to drain the full reply";

/// The one-shot test seam after a full reply and before the bounded EOF drain.
#[cfg(test)]
static POST_REPLY_PAUSE: OnceLock<Mutex<Option<Pause>>> = OnceLock::new();

/// What one socket read found.
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReadOutcome {
    Byte(u8),
    /// The peer closed.
    Eof,
    /// Shutdown ended the wait.
    Stopped,
}

/// What one lookahead fill found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FillOutcome {
    /// Bytes are held now: how many in total.
    Held(usize),
    /// The peer closed.
    Eof,
    /// Shutdown ended the wait.
    Stopped,
}

/// What a socket-read error means for a core read: waiting for more
/// ends in a clock park, an interruption reads again at once, and
/// anything else is the peer's end. A pure function, so a unit test pins
/// each kind directly: socket tests cannot, since an interruption lands
/// nondeterministically beside the error under test.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReadErr {
    Wait,
    Retry,
    End,
}

pub(crate) fn classify(error: &io::Error) -> ReadErr {
    if error.kind() == io::ErrorKind::WouldBlock {
        ReadErr::Wait
    } else if error.kind() == io::ErrorKind::Interrupted {
        ReadErr::Retry
    } else {
        ReadErr::End
    }
}

/// One accepted connection, still charging its accept window's budget.
pub(crate) struct Connection {
    stream: TcpStream,
    budget: RequestBudget,
    /// The request every failure names.
    name: String,
    pub(crate) shared: Arc<Shared>,
    /// Bytes read past what the caller consumed.
    ahead: VecDeque<u8>,
    seq: u64,
    /// The last acknowledged wake generation and phase: polls stay silent.
    acked: Option<(u64, &'static str)>,
}

impl Connection {
    /// One accepted connection on `budget`, named `name`: the accept
    /// window's budget follows the socket, never restarted.
    pub(crate) fn new(
        stream: TcpStream,
        budget: RequestBudget,
        name: String,
        shared: Arc<Shared>,
    ) -> Self {
        Self {
            stream,
            budget,
            name,
            shared,
            ahead: VecDeque::new(),
            seq: 0,
            acked: None,
        }
    }

    /// One wait on a would-block socket: expiry names the request.
    /// Test-only: production reads wait through [`Shared::wait_ready`],
    /// which ends the park at once when the bytes arrive.
    #[cfg(test)]
    fn wait(&mut self, phase: &'static str) -> WaitEnd {
        self.shared.wait(
            &self.budget,
            &self.name,
            phase,
            &mut self.seq,
            &mut self.acked,
        )
    }

    /// Whether the next byte may be consumed for `phase`: false when
    /// shutdown stopped the read. An expired budget or a spent real
    /// guard panics naming the request and phase, as after a wait.
    /// Checked before buffered bytes are consumed and after successful
    /// socket reads, so an expired budget or a racing shutdown never
    /// completes a request from bytes already held.
    pub(crate) fn consume_check(&self, phase: &str) -> bool {
        if self.shared.stopping.load(Ordering::SeqCst) {
            return false;
        }
        if self.budget.expired() {
            self.expired(phase);
        }
        if self.budget.guard_left().is_zero() {
            guard_expired(&self.budget, &self.name, phase);
        }
        true
    }

    /// Expiry fails naming the request and phase.
    fn expired(&self, phase: &str) -> ! {
        expired(&self.budget, &self.name, phase)
    }

    /// One byte, buffered first: `Eof` when the peer closed, `Stopped`
    /// when shutdown ended the wait.
    #[cfg(test)]
    pub(crate) fn read_byte(&mut self, phase: &'static str) -> ReadOutcome {
        if !self.consume_check(phase) {
            return ReadOutcome::Stopped;
        }
        if let Some(byte) = self.ahead.pop_front() {
            return ReadOutcome::Byte(byte);
        }
        loop {
            let mut byte = [0u8; 1];
            match self.stream.read(&mut byte) {
                Ok(0) => return ReadOutcome::Eof,
                Ok(_) => {
                    if !self.consume_check(phase) {
                        return ReadOutcome::Stopped;
                    }
                    return ReadOutcome::Byte(byte[0]);
                }
                Err(error) => match classify(&error) {
                    ReadErr::Wait => {}
                    ReadErr::Retry => continue,
                    ReadErr::End => return ReadOutcome::Eof,
                },
            }
            match self.wait(phase) {
                WaitEnd::Waited => {
                    if !self.consume_check(phase) {
                        return ReadOutcome::Stopped;
                    }
                }
                WaitEnd::Expired => self.expired(phase),
                WaitEnd::Stopped => return ReadOutcome::Stopped,
            }
        }
    }

    /// One readiness park on the socket: true once bytes may have
    /// arrived, false when shutdown stopped the wait. The bound still
    /// checks past every park, as for one byte: expiry names the request
    /// and phase. The one wait behind [`Connection::fill`] and
    /// [`Connection::fill_into`].
    fn wait_readable(&mut self, phase: &'static str) -> bool {
        match self.shared.wait_ready(
            &self.budget,
            &self.name,
            phase,
            &mut self.seq,
            &mut self.acked,
            Some(self.stream.as_fd()),
            true,
        ) {
            // Readiness parks end at once when the bytes arrive
            // instead of sleeping a whole poll past them; the bound
            // still checks past every park, as for one byte.
            WaitEnd::Waited => self.consume_check(phase),
            WaitEnd::Expired => self.expired(phase),
            WaitEnd::Stopped => false,
        }
    }

    /// One socket read past the bytes held: how many are held now, `Eof`
    /// on peer close, `Stopped` on shutdown.
    pub(crate) fn fill(&mut self, phase: &'static str) -> FillOutcome {
        if !self.consume_check(phase) {
            return FillOutcome::Stopped;
        }
        loop {
            let mut chunk = [0u8; READ_CHUNK];
            match self.stream.read(&mut chunk) {
                Ok(0) => return FillOutcome::Eof,
                Ok(read) => {
                    if !self.consume_check(phase) {
                        return FillOutcome::Stopped;
                    }
                    self.ahead.extend(chunk.into_iter().take(read));
                    self.shared.lookahead_count(read);
                    return FillOutcome::Held(self.ahead.len());
                }
                Err(error) => match classify(&error) {
                    ReadErr::Wait => {}
                    ReadErr::Retry => continue,
                    ReadErr::End => return FillOutcome::Eof,
                },
            }
            if !self.wait_readable(phase) {
                return FillOutcome::Stopped;
            }
        }
    }

    /// One buffered byte without touching the socket.
    pub(crate) fn buffered(&mut self) -> Option<u8> {
        self.ahead.pop_front()
    }

    /// Discards bytes already on the socket, without waiting for more:
    /// after a fixed body, the pipelined bytes the capped reads left
    /// behind. A close with unread bytes resets the connection, so what
    /// already arrived is consumed, as the old over-read did; what has
    /// not arrived yet is never waited for. Each pass re-checks the
    /// request budget and shutdown, so a feeding peer cannot hold the
    /// discard past either and the bounded full-reply drain is reached.
    #[allow(
        clippy::unused_io_amount,
        reason = "a discard read: pipelined bytes past the body are counted nowhere"
    )]
    pub(crate) fn drain_socket(&mut self) {
        const PHASE: &str = "to discard pipelined bytes";
        loop {
            if !self.consume_check(PHASE) {
                return;
            }
            let mut chunk = [0u8; READ_CHUNK];
            match self.stream.read(&mut chunk) {
                // The peer closed: nothing more can arrive.
                Ok(0) => return,
                // Discarded: more may have arrived behind them.
                Ok(_) => continue,
                Err(error) => match classify(&error) {
                    ReadErr::Wait => return,
                    ReadErr::Retry => continue,
                    ReadErr::End => return,
                },
            }
        }
    }

    /// Reads socket bytes straight into `out`, up to `max` of them: the
    /// fixed-body path's bulk read, so body bytes never transit the
    /// lookahead one byte at a time. The read is capped at the remaining
    /// room, never past the body's bytes, so pipelined bytes stay on the
    /// socket exactly as with a capped drain. Same bound, wait and
    /// shutdown semantics as [`Connection::fill`]: the caller drains the
    /// lookahead first, so no buffered byte is reordered past, and
    /// passes the remaining room, never zero, since its loop breaks
    /// once complete.
    pub(crate) fn fill_into(
        &mut self,
        out: &mut Vec<u8>,
        max: usize,
        phase: &'static str,
    ) -> FillOutcome {
        if !self.consume_check(phase) {
            return FillOutcome::Stopped;
        }
        loop {
            let mut chunk = [0u8; READ_CHUNK];
            // Capped at the remaining room: the read never takes a byte
            // past the body, so no pipelined byte is consumed as body.
            // The split never panics: the cap is at most the chunk.
            let end = chunk.len().min(max);
            let (buf, _) = chunk.split_at_mut(end);
            match self.stream.read(buf) {
                Ok(0) => return FillOutcome::Eof,
                Ok(read) => {
                    if !self.consume_check(phase) {
                        return FillOutcome::Stopped;
                    }
                    // The read never reports more than its buffer holds.
                    let (seen, _) = chunk.split_at(read);
                    out.extend_from_slice(seen);
                    return FillOutcome::Held(out.len());
                }
                Err(error) => match classify(&error) {
                    ReadErr::Wait => {}
                    ReadErr::Retry => continue,
                    ReadErr::End => return FillOutcome::Eof,
                },
            }
            if !self.wait_readable(phase) {
                return FillOutcome::Stopped;
            }
        }
    }

    /// Moves up to `max` buffered bytes into `out` without touching the
    /// socket. A bulk drain, so a fixed-length body pays one bound check
    /// per fill instead of one per byte; the caller checks the bound
    /// before consuming buffered bytes, as for one byte.
    pub(crate) fn drain_ahead(&mut self, out: &mut Vec<u8>, max: usize) {
        let take = self.ahead.len().min(max);
        out.extend(self.ahead.drain(..take));
    }

    /// The stream, its bytes read past the head, and the shared state:
    /// the CONNECT handoff's parts. The accept window's budget does not
    /// follow it: the tunnel bounds each of its own operations.
    pub(crate) fn into_tunnel_parts(self) -> (TcpStream, Vec<u8>, Arc<Shared>) {
        let Self {
            stream,
            ahead,
            shared,
            ..
        } = self;
        (stream, ahead.into(), shared)
    }

    /// Writes every byte and flushes: false when shutdown or the peer
    /// ended the connection first. Response I/O runs its own operation
    /// budget, not the request's read budget.
    pub(crate) fn write_all(&mut self, bytes: &[u8], phase: &'static str) -> bool {
        let op = self.op_budget();
        let (stream, mut ctx) = self.parts(&op);
        pump(stream, bytes, phase, &mut ctx)
    }

    /// A fresh operation budget for response I/O: a reply, a stall, a
    /// pause or a tunnel handshake each bounds itself, never reusing
    /// the read budget.
    pub(crate) fn op_budget(&self) -> RequestBudget {
        RequestBudget::start_on(
            &self.shared.clock,
            self.shared.limits.request,
            self.shared.limits.guard,
        )
    }

    /// Holds `pause` for `phase` under `op`: released when the test
    /// releases or drops its sender, stopped on shutdown or expiry.
    pub(crate) fn pause(
        &mut self,
        pause: &Pause,
        op: &RequestBudget,
        phase: &'static str,
    ) -> PauseEnd {
        match pause.reached.send(()) {
            Ok(()) | Err(_) => {}
        }
        loop {
            match lock(&pause.release).try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return PauseEnd::Released,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match self
                .shared
                .wait(op, &self.name, phase, &mut self.seq, &mut self.acked)
            {
                WaitEnd::Waited => {}
                WaitEnd::Expired => expired(op, &self.name, phase),
                WaitEnd::Stopped => return PauseEnd::Stopped,
            }
        }
    }

    /// The stream and wait context for one operation's writes.
    pub(crate) fn parts<'a>(
        &'a mut self,
        op: &'a RequestBudget,
    ) -> (&'a mut TcpStream, WaitCtx<'a>) {
        let Self {
            stream,
            name,
            shared,
            seq,
            acked,
            ..
        } = self;
        (
            stream,
            WaitCtx {
                budget: op,
                name,
                shared,
                seq,
                acked,
            },
        )
    }

    /// Writes the whole head and body, then shuts down the write half and
    /// drains the client's side until EOF or the injected-clock bound:
    /// the scripted bytes go as supplied, and bytes pipelined before the
    /// client's FIN are consumed before the connection closes.
    pub(crate) fn reply_full(
        &mut self,
        head: &[u8],
        body: &[u8],
        pause: Option<&Pause>,
    ) -> ReplyEnd {
        let op = self.op_budget();
        if let Some(pause) = pause
            && self.pause(pause, &op, "to pause before the response") == PauseEnd::Stopped
        {
            return ReplyEnd::Stopped;
        }
        for (bytes, phase) in [
            (head, "to write the response head"),
            (body, "to write the response body"),
        ] {
            let (stream, mut ctx) = self.parts(&op);
            if !pump(stream, bytes, phase, &mut ctx) {
                return ReplyEnd::Stopped;
            }
        }
        #[cfg(test)]
        self.test_post_reply_park();
        self.shutdown_write_and_drain(REPLY_DRAIN_WITHIN);
        ReplyEnd::Done
    }

    /// Shuts down the write half, then drains client bytes until EOF,
    /// shutdown or `within`: the full-reply close path only. A socket
    /// timeout bounds one read, not the entire drain if a client keeps
    /// sending, so the operation budget uses the injected clock. Its
    /// readiness wait also ends as soon as server shutdown lands.
    pub(crate) fn shutdown_write_and_drain(&mut self, within: Duration) {
        match self.stream.shutdown(Shutdown::Write) {
            Ok(()) | Err(_) => {}
        }
        let op = RequestBudget::start_on(&self.shared.clock, within, self.shared.limits.guard);
        loop {
            if self.shared.stopping.load(Ordering::SeqCst) || op.expired() {
                return;
            }
            if op.guard_left().is_zero() {
                guard_expired(&op, &self.name, REPLY_DRAIN_PHASE);
            }
            let mut chunk = [0u8; READ_CHUNK];
            match self.stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) => match classify(&error) {
                    ReadErr::Wait => {
                        // One readiness park: the loop top maps shutdown
                        // and expiry, so no second check here duplicates it.
                        match self.shared.wait_ready(
                            &op,
                            &self.name,
                            REPLY_DRAIN_PHASE,
                            &mut self.seq,
                            &mut self.acked,
                            Some(self.stream.as_fd()),
                            true,
                        ) {
                            WaitEnd::Waited => {}
                            WaitEnd::Expired | WaitEnd::Stopped => return,
                        }
                    }
                    ReadErr::Retry => continue,
                    ReadErr::End => return,
                },
            }
        }
    }

    /// Installs the one-shot post-reply park for a deterministic close test.
    #[cfg(test)]
    pub(crate) fn set_test_post_reply_pause(pause: Pause) {
        *lock(POST_REPLY_PAUSE.get_or_init(|| Mutex::new(None))) = Some(pause);
    }

    /// Parks after writing the reply. The first signal lets the test send
    /// bytes; the second follows a peek that proves they reached the socket,
    /// while leaving them unread for the close path under test.
    #[cfg(test)]
    fn test_post_reply_park(&mut self) {
        let Some(pause) = lock(POST_REPLY_PAUSE.get_or_init(|| Mutex::new(None))).take() else {
            return;
        };
        const PHASE: &str = "to park after the full reply";
        match pause.reached.send(()) {
            Ok(()) | Err(_) => {}
        }
        loop {
            let mut byte = [0u8; 1];
            match self.stream.peek(&mut byte) {
                Ok(0) => return,
                Ok(_) => break,
                Err(error) => match classify(&error) {
                    ReadErr::Wait => {
                        if !self.wait_readable(PHASE) {
                            return;
                        }
                    }
                    ReadErr::Retry => continue,
                    ReadErr::End => return,
                },
            }
        }
        match pause.reached.send(()) {
            Ok(()) | Err(_) => {}
        }
        let op = self.op_budget();
        loop {
            match lock(&pause.release).try_recv() {
                Ok(()) | Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => {}
            }
            match self
                .shared
                .wait(&op, &self.name, PHASE, &mut self.seq, &mut self.acked)
            {
                WaitEnd::Waited => {}
                WaitEnd::Expired | WaitEnd::Stopped => return,
            }
        }
    }

    /// Streams `pieces` under `head`: chunked framing for a recorded
    /// stream, raw bytes for SSE. Pauses before the reply and between
    /// pieces when held.
    #[cfg(test)]
    pub(crate) fn reply_stream(
        &mut self,
        head: &[u8],
        pieces: &[Vec<u8>],
        framing: Framing,
        pause: Option<&Pause>,
    ) -> ReplyEnd {
        let op = self.op_budget();
        if let Some(pause) = pause
            && self.pause(pause, &op, "to pause before the response") == PauseEnd::Stopped
        {
            return ReplyEnd::Stopped;
        }
        let (stream, mut ctx) = self.parts(&op);
        if !pump(stream, head, "to write the stream head", &mut ctx) {
            return ReplyEnd::Stopped;
        }
        for (index, piece) in pieces.iter().enumerate() {
            if index != 0
                && let Some(pause) = pause
                && self.pause(pause, &op, "to pause between stream pieces") == PauseEnd::Stopped
            {
                return ReplyEnd::Stopped;
            }
            if framing == Framing::Chunked {
                let size = format!("{:x}\r\n", piece.len());
                let (stream, mut ctx) = self.parts(&op);
                if !pump(stream, size.as_bytes(), "to write a piece size", &mut ctx) {
                    return ReplyEnd::Stopped;
                }
            }
            let (stream, mut ctx) = self.parts(&op);
            if !pump(stream, piece, "to write a stream piece", &mut ctx) {
                return ReplyEnd::Stopped;
            }
            if framing == Framing::Chunked {
                let (stream, mut ctx) = self.parts(&op);
                if !pump(stream, b"\r\n", "to end a stream piece", &mut ctx) {
                    return ReplyEnd::Stopped;
                }
            }
        }
        if framing == Framing::Chunked {
            let (stream, mut ctx) = self.parts(&op);
            if !pump(stream, b"0\r\n\r\n", "to end the chunked stream", &mut ctx) {
                return ReplyEnd::Stopped;
            }
        }
        ReplyEnd::Done
    }

    /// Sends the head with its declared length and the prefix it
    /// carries: true once flushed, so the adapter records the partial
    /// send before the close wait below.
    pub(crate) fn stall_send(&mut self, head: &[u8], prefix: &[u8], pause: Option<&Pause>) -> bool {
        let op = self.op_budget();
        if let Some(pause) = pause
            && self.pause(pause, &op, "to pause before the stalled response") == PauseEnd::Stopped
        {
            return false;
        }
        let (stream, mut ctx) = self.parts(&op);
        if !pump(stream, head, "to write the stalled head", &mut ctx) {
            return false;
        }
        let (stream, mut ctx) = self.parts(&op);
        pump(stream, prefix, "to write the stalled prefix", &mut ctx)
    }

    /// Holds until the peer closes: `Closed` on its close, `Stopped` on
    /// shutdown, expiry naming the stall past its bound.
    pub(crate) fn stall_wait(&mut self) -> StallEnd {
        let op = self.op_budget();
        loop {
            let mut one = [0u8; 1024];
            match self.stream.read(&mut one) {
                Ok(0) => return StallEnd::Closed,
                Ok(_) => continue,
                Err(error) => match classify(&error) {
                    ReadErr::Wait => {}
                    ReadErr::Retry => continue,
                    ReadErr::End => return StallEnd::Closed,
                },
            }
            match self.shared.wait(
                &op,
                &self.name,
                "to wait for the stalled peer",
                &mut self.seq,
                &mut self.acked,
            ) {
                WaitEnd::Waited => {}
                WaitEnd::Expired => expired(&op, &self.name, "to wait for the stalled peer"),
                WaitEnd::Stopped => return StallEnd::Stopped,
            }
        }
    }

    /// Sends the head with its declared length and the prefix it
    /// carries, then holds until the peer closes: `Closed` on its close,
    /// `Stopped` on shutdown, expiry naming the stall past its bound.
    #[cfg(test)]
    pub(crate) fn stall(&mut self, head: &[u8], prefix: &[u8], pause: Option<&Pause>) -> StallEnd {
        if !self.stall_send(head, prefix, pause) {
            return StallEnd::Stopped;
        }
        self.stall_wait()
    }

    /// Hands the connection to a tunnel: the stream, blocking again,
    /// with the bytes read past the head.
    #[cfg(test)]
    pub(crate) fn into_handoff(self) -> io::Result<(TcpStream, Vec<u8>)> {
        self.stream.set_nonblocking(false)?;
        let Self { stream, ahead, .. } = self;
        Ok((stream, ahead.into()))
    }
}

/// How one reply ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReplyEnd {
    /// Every byte flushed.
    Done,
    /// Shutdown or the peer ended the connection first.
    Stopped,
}

/// How one stalled response ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StallEnd {
    /// The peer closed after the prefix.
    Closed,
    /// Shutdown ended the wait.
    Stopped,
}

/// How one pause ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PauseEnd {
    /// The test released it, or dropped its sender.
    Released,
    /// Shutdown ended the wait.
    Stopped,
}

/// How one stream frames its pieces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg(test)]
pub(crate) enum Framing {
    /// Each piece carries its hex length: a recorded stream.
    Chunked,
    /// Pieces go raw: server-sent events.
    Raw,
}

/// A response pause: the core signals each arrival, then holds until
/// the test releases or drops its sender. Dropping the control
/// releases every holder, including failure paths. The release side
/// sits behind a mutex, so a pause is safe to share with serving
/// threads.
pub(crate) struct Pause {
    reached: mpsc::Sender<()>,
    release: Mutex<mpsc::Receiver<()>>,
}

/// The test's end of a pause: wait for arrivals, release the holder.
/// Dropping this releases the holder too.
#[cfg(test)]
pub(crate) struct PauseControl {
    reached: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

impl Pause {
    /// A pause and its control.
    #[cfg(test)]
    pub(crate) fn new() -> (Pause, PauseControl) {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        (
            Pause {
                reached: reached_tx,
                release: Mutex::new(release_rx),
            },
            PauseControl {
                reached: reached_rx,
                release: release_tx,
            },
        )
    }
}

#[cfg(test)]
impl PauseControl {
    /// Waits for the next arrival, failing the test on expiry.
    #[cfg(test)]
    pub(crate) fn wait_hit(&self, deadline: &Deadline, what: &str) {
        deadline.recv_or_fail(&self.reached, what);
    }

    /// Releases the holder.
    #[cfg(test)]
    pub(crate) fn release(&self) {
        match self.release.send(()) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// One write's wait context: its operation budget and acknowledgment
/// state.
pub(crate) struct WaitCtx<'a> {
    pub(crate) budget: &'a RequestBudget,
    pub(crate) name: &'a str,
    pub(crate) shared: &'a Arc<Shared>,
    pub(crate) seq: &'a mut u64,
    pub(crate) acked: &'a mut Option<(u64, &'static str)>,
}

/// Writes every byte through `writer` and flushes within the wait
/// context's budget: false when shutdown or the peer ended it first.
/// Expiry panics naming the request and phase. The test drives this
/// with a finite writer past its capacity; production passes the
/// connection's stream.
pub(crate) fn pump(
    writer: &mut dyn Write,
    bytes: &[u8],
    phase: &'static str,
    ctx: &mut WaitCtx<'_>,
) -> bool {
    let mut written = 0;
    while written != bytes.len() {
        let Some(next) = bytes.get(written..) else {
            return false;
        };
        match writer.write(next) {
            Ok(0) => return false,
            Ok(n) => written += n,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
        if written != bytes.len() {
            match ctx
                .shared
                .wait(ctx.budget, ctx.name, phase, ctx.seq, ctx.acked)
            {
                WaitEnd::Waited => {}
                WaitEnd::Expired => expired(ctx.budget, ctx.name, phase),
                WaitEnd::Stopped => return false,
            }
        }
    }
    loop {
        match writer.flush() {
            Ok(()) => return true,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
        match ctx
            .shared
            .wait(ctx.budget, ctx.name, phase, ctx.seq, ctx.acked)
        {
            WaitEnd::Waited => {}
            WaitEnd::Expired => expired(ctx.budget, ctx.name, phase),
            WaitEnd::Stopped => return false,
        }
    }
}
