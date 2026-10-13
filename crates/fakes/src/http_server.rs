//! One bounded HTTP/1.1 server core (`docs/testing.md`, "Fakes"): the
//! listener, request budgets and transports every HTTP fake builds on.
//! Each accept attempt runs inside one budget, made before the attempt:
//! accept, head and body charge the same budget, never restarted. The
//! budget runs on the injected clock, with a real hang guard besides it.
//! Expiry names the request; shutdown ends every wait stopped.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc, Condvar, Mutex, MutexGuard, PoisonError,
    atomic::{AtomicBool, AtomicU8, Ordering},
    mpsc,
};
use std::thread::{self, JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake};

use crate::clock::SystemClock;
use crate::deadline::Deadline;

pub(crate) mod request;

const REQUEST_WITHIN: Duration = Duration::from_secs(30);
const HANG_GUARD: Duration = Duration::from_secs(45);
const POLL: Duration = Duration::from_millis(1);
/// How long shutdown waits for the serving threads to stop.
pub(crate) const SHUTDOWN_WITHIN: Duration = Duration::from_secs(2);
const READ_CHUNK: usize = 8192;

/// The process clock behind every real hang guard.
static PROCESS: SystemClock = SystemClock;

/// One request's bounds and caps: thirty injected-clock seconds behind a
/// forty-five real-second guard; 64 KiB of request line, 64 KiB of one
/// header or chunk line, 1 MiB over at most 4096 headers, 64 MiB of
/// decoded body, 1 MiB over at most 4096 trailers. Terminators count.
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) request: Duration,
    pub(crate) guard: Duration,
    pub(crate) poll: Duration,
    pub(crate) line: usize,
    pub(crate) frame: usize,
    pub(crate) headers: usize,
    pub(crate) header_count: usize,
    pub(crate) body: usize,
    pub(crate) trailers: usize,
    pub(crate) trailer_count: usize,
}

impl Limits {
    pub(crate) fn default() -> Self {
        Self {
            request: REQUEST_WITHIN,
            guard: HANG_GUARD,
            poll: POLL,
            line: 64 * 1024,
            frame: 64 * 1024,
            headers: 1024 * 1024,
            header_count: 4096,
            body: 64 * 1024 * 1024,
            trailers: 1024 * 1024,
            trailer_count: 4096,
        }
    }
}

/// Whether the listener waits for one named request or renews idle
/// windows: matched explicitly at both expiry edges.
pub(crate) enum AcceptMode {
    Persistent,
    Expected {
        /// The request the wait names on expiry.
        name: String,
    },
}

impl AcceptMode {
    /// The request every failure on this mode names.
    fn name(&self) -> &str {
        match self {
            AcceptMode::Persistent => "the persistent request",
            AcceptMode::Expected { name } => name,
        }
    }
}

/// One request's two clocks, both started before its accept attempt and
/// never restarted: the injected bound the test advances, and the
/// wall-clock guard ending a hang no advance rescues.
pub(crate) struct RequestBudget {
    clock: Arc<dyn Clock>,
    start: Instant,
    duration: Duration,
    guard_start: Instant,
    guard_for: Duration,
}

impl RequestBudget {
    fn start_on(clock: &Arc<dyn Clock>, duration: Duration, guard: Duration) -> Self {
        let cloned = Arc::clone(clock);
        let start = clock.now();
        let guard_start = PROCESS.now();
        Self {
            clock: cloned,
            start,
            duration,
            guard_start,
            guard_for: guard,
        }
    }

    /// Exact expiry counts; one tick before does not.
    fn expired(&self) -> bool {
        self.clock.now().saturating_duration_since(self.start) >= self.duration
    }

    /// The bound for clock parks: `None` only past instant overflow.
    fn until(&self) -> Option<Instant> {
        self.start.checked_add(self.duration)
    }

    fn guard_left(&self) -> Duration {
        self.guard_for
            .saturating_sub(PROCESS.now().saturating_duration_since(self.guard_start))
    }
}

/// One clock-wait acknowledgment, announced after the waiter's clock
/// check: its phase, thread and own sequence, so the test advances only
/// the announced sequence. `until` is the parked bound
/// [`FakeClock::await_thread_parked`](crate::clock::FakeClock::await_thread_parked)
/// matches.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WaitAck {
    pub(crate) phase: &'static str,
    pub(crate) thread: ThreadId,
    pub(crate) seq: u64,
    pub(crate) until: Option<Instant>,
}

/// The test-only check-to-park pause: while armed, a waiter holds after
/// its check, subscription and acknowledgment but before its park. The
/// test owns it like a release channel: no deadline of its own applies
/// while held.
pub(crate) struct CheckGate {
    armed: Mutex<bool>,
    changed: Condvar,
}

impl CheckGate {
    fn new() -> Self {
        Self {
            armed: Mutex::new(false),
            changed: Condvar::new(),
        }
    }

    fn hold(&self) {
        let mut armed = lock(&self.armed);
        while *armed {
            armed = self
                .changed
                .wait(armed)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    #[cfg(test)]
    pub(crate) fn arm(&self) {
        *lock(&self.armed) = true;
    }

    #[cfg(test)]
    pub(crate) fn release(&self) {
        *lock(&self.armed) = false;
        self.changed.notify_all();
    }
}

/// Bumps the server's wake generation whenever the injected clock moves.
struct ClockWaker {
    shared: std::sync::Weak<Shared>,
}

impl Wake for ClockWaker {
    fn wake(&self) {
        if let Some(shared) = self.shared.upgrade() {
            *lock(&shared.wake) += 1;
            shared.wake_changed.notify_all();
        }
    }
}

/// How one bounded wait ended. The verdict is authoritative: a check
/// before it is only a fast path, so the caller maps expiry itself.
pub(crate) enum WaitEnd {
    Waited,
    Expired,
    Stopped,
}

/// What one socket read found.
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

/// One accept attempt's end.
enum AcceptOutcome {
    Accepted(TcpStream),
    /// The window's budget ran out with no connection.
    Idle,
    /// Shutdown ended the wait.
    Stopped,
}

/// One persistent window's end.
pub(crate) enum WindowOutcome {
    Accepted(Connection),
    /// The window's budget ran out with no connection.
    Idle,
    /// Shutdown ended the wait.
    Stopped,
}

/// The core's open CONNECT tunnels: shutdown breaks every one.
struct TunnelState {
    /// The next tunnel's id.
    next: u64,
    /// Each open tunnel's sockets, broken on shutdown.
    live: Vec<(u64, TcpStream, TcpStream)>,
}

struct Shared {
    /// The loopback listener, polled nonblocking: shutdown takes and
    /// closes it, so the port refuses connections even while stuck
    /// connections keep the server alive.
    listener: Mutex<Option<TcpListener>>,
    clock: Arc<dyn Clock>,
    limits: Limits,
    /// Set by shutdown: every wait ends stopped.
    stopping: AtomicBool,
    accept: Mutex<Option<JoinHandle<()>>>,
    conns: Mutex<Vec<JoinHandle<()>>>,
    /// The wake generation every waiter parks on.
    wake: Mutex<u64>,
    wake_changed: Condvar,
    _waker: Arc<ClockWaker>,
    acks: Mutex<Option<mpsc::Sender<WaitAck>>>,
    gate: Mutex<Option<Arc<CheckGate>>>,
    idle: Mutex<usize>,
    idle_changed: Condvar,
    /// Started windows, test-only: the test advances only past a window's
    /// start, so no advance lands before its budget exists.
    #[cfg(test)]
    windows: Mutex<usize>,
    #[cfg(test)]
    windows_changed: Condvar,
    /// Open tunnels, broken by shutdown: each entry's sockets end every
    /// blocked copy when the server stops.
    tunnels: Mutex<TunnelState>,
}

impl Shared {
    /// Parks once until the clock wakes it, a poll ends, or shutdown
    /// lands. The caller maps expiry: idle for a window, a named failure
    /// for a request. Only the real guard panics here.
    #[allow(
        clippy::panic,
        reason = "a fake's wait; the panic names the request whose real guard ran out"
    )]
    fn wait(
        &self,
        budget: &RequestBudget,
        name: &str,
        phase: &'static str,
        seq: &mut u64,
        acked: &mut Option<u64>,
    ) -> WaitEnd {
        // Read under the mutex the wake bumps, before every check, so a
        // wake between here and the park still ends the park at once.
        let seen = *lock(&self.wake);
        if self.stopping.load(Ordering::SeqCst) {
            return WaitEnd::Stopped;
        }
        if budget.expired() {
            return WaitEnd::Expired;
        }
        let guard_left = budget.guard_left();
        if guard_left.is_zero() {
            panic!(
                "fakes: the HTTP core's real guard of {:?} ran out {phase} {name}",
                budget.guard_for
            );
        }
        if *acked != Some(seen) {
            *seq += 1;
            if let Some(acks) = lock(&self.acks).as_ref() {
                let ack = WaitAck {
                    phase,
                    thread: thread::current().id(),
                    seq: *seq,
                    until: budget.until(),
                };
                match acks.send(ack) {
                    Ok(()) | Err(_) => {}
                }
            }
            *acked = Some(seen);
        }
        if let Some(gate) = lock(&self.gate).as_ref().map(Arc::clone) {
            gate.hold();
        }
        // One park, bounded by the shorter of the poll and the guard. The
        // predicate holds the generation read above: a wake past that read
        // returns at once.
        let guard = lock(&self.wake);
        let parked = support::clock::park(
            budget.clock.as_ref(),
            budget.until(),
            Some(self.limits.poll.min(guard_left)),
            &self.wake_changed,
            guard,
            |wake| *wake != seen,
        );
        drop(parked);
        WaitEnd::Waited
    }
}

/// One accepted connection, still charging its accept window's budget.
pub(crate) struct Connection {
    stream: TcpStream,
    budget: RequestBudget,
    /// The request every failure names.
    name: String,
    shared: Arc<Shared>,
    /// Bytes read past what the caller consumed.
    ahead: VecDeque<u8>,
    seq: u64,
    /// The last acknowledged wake generation: polls stay silent.
    acked: Option<u64>,
}

impl Connection {
    /// One wait on a would-block socket: expiry names the request.
    fn wait(&mut self, phase: &'static str) -> WaitEnd {
        self.shared.wait(
            &self.budget,
            &self.name,
            phase,
            &mut self.seq,
            &mut self.acked,
        )
    }

    /// Expiry fails naming the request and phase.
    fn expired(&self, phase: &str) -> ! {
        expired(&self.budget, &self.name, phase)
    }

    /// One byte, buffered first: `Eof` when the peer closed, `Stopped`
    /// when shutdown ended the wait.
    pub(crate) fn read_byte(&mut self, phase: &'static str) -> ReadOutcome {
        if let Some(byte) = self.ahead.pop_front() {
            return ReadOutcome::Byte(byte);
        }
        loop {
            let mut byte = [0u8; 1];
            match self.stream.read(&mut byte) {
                Ok(0) => return ReadOutcome::Eof,
                Ok(_) => return ReadOutcome::Byte(byte[0]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return ReadOutcome::Eof,
            }
            match self.wait(phase) {
                WaitEnd::Waited => {}
                WaitEnd::Expired => self.expired(phase),
                WaitEnd::Stopped => return ReadOutcome::Stopped,
            }
        }
    }

    /// One socket read past the bytes held: how many are held now, `Eof`
    /// on peer close, `Stopped` on shutdown.
    pub(crate) fn fill(&mut self, phase: &'static str) -> FillOutcome {
        loop {
            let mut chunk = [0u8; READ_CHUNK];
            match self.stream.read(&mut chunk) {
                Ok(0) => return FillOutcome::Eof,
                Ok(read) => {
                    self.ahead.extend(chunk.into_iter().take(read));
                    return FillOutcome::Held(self.ahead.len());
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return FillOutcome::Eof,
            }
            match self.wait(phase) {
                WaitEnd::Waited => {}
                WaitEnd::Expired => self.expired(phase),
                WaitEnd::Stopped => return FillOutcome::Stopped,
            }
        }
    }

    /// One buffered byte without touching the socket.
    pub(crate) fn buffered(&mut self) -> Option<u8> {
        self.ahead.pop_front()
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
    fn parts<'a>(&'a mut self, op: &'a RequestBudget) -> (&'a mut TcpStream, WaitCtx<'a>) {
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

    /// Writes the whole head and body, then the connection closes behind
    /// the caller: the scripted bytes go as supplied.
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
        ReplyEnd::Done
    }

    /// Streams `pieces` under `head`: chunked framing for a recorded
    /// stream, raw bytes for SSE. Pauses before the reply and between
    /// pieces when held.
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
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return StallEnd::Closed,
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
    pub(crate) fn stall(&mut self, head: &[u8], prefix: &[u8], pause: Option<&Pause>) -> StallEnd {
        if !self.stall_send(head, prefix, pause) {
            return StallEnd::Stopped;
        }
        self.stall_wait()
    }

    /// Runs a CONNECT tunnel to `target` over this connection: the 200
    /// handshake, the bytes read past the head, then bytes both ways
    /// until either side closes. The handshake is bounded; the copy ends
    /// on close or shutdown, reporting which side closed first. Targets
    /// outside the trust boundary never connect.
    /// Dials a CONNECT target within the trust boundary: a loopback
    /// address or localhost, connected with the remaining handshake
    /// guard. Anything else, or a refusal, is a silent error before any
    /// record, as a failed dial always was.
    pub(crate) fn dial(&self, target: &str) -> io::Result<TcpStream> {
        let op = self.op_budget();
        let addr = HttpServer::check_target(target)?;
        TcpStream::connect_timeout(&addr, op.guard_left().min(self.shared.limits.request))
    }

    pub(crate) fn tunnel(self, target: &str, origin: TcpStream) -> TunnelEnd {
        let op = RequestBudget::start_on(
            &self.shared.clock,
            self.shared.limits.request,
            self.shared.limits.guard,
        );
        let Self {
            mut stream,
            ahead,
            shared,
            ..
        } = self;
        // The copies below block; the handshake bounds itself through
        // the pump either way.
        if stream.set_nonblocking(false).is_err() {
            return TunnelEnd::OriginClosed;
        }
        let ahead: Vec<u8> = ahead.into();
        let mut origin = origin;
        // The 200 and the coalesced bytes within the handshake's budget.
        let mut seq = 0;
        let mut acked = None;
        let mut ctx = WaitCtx {
            budget: &op,
            name: target,
            shared: &shared,
            seq: &mut seq,
            acked: &mut acked,
        };
        if !pump(
            &mut stream,
            b"HTTP/1.1 200 Connection Established\r\n\r\n",
            "to write the tunnel handshake",
            &mut ctx,
        ) || !pump(
            &mut origin,
            &ahead,
            "to forward the bytes past the head",
            &mut ctx,
        ) {
            return TunnelEnd::OriginClosed;
        }
        // Copies of both ends for shutdown to break: the tunnel owns
        // the originals below.
        let id = {
            let mut tunnels = lock(&shared.tunnels);
            let id = tunnels.next;
            tunnels.next = tunnels.next.wrapping_add(1);
            match (stream.try_clone(), origin.try_clone()) {
                (Ok(break_client), Ok(break_origin)) => {
                    tunnels.live.push((id, break_client, break_origin));
                }
                _ => return TunnelEnd::OriginClosed,
            }
            id
        };
        // Whoever's copy ends first names the end; both directions
        // break on the first end. 1 names the client, 2 the origin.
        let first = Arc::new(AtomicU8::new(0));
        let first_pump = Arc::clone(&first);
        let mut origin_pump = match origin.try_clone() {
            Ok(origin) => origin,
            Err(_) => return TunnelEnd::OriginClosed,
        };
        let mut client_pump = match stream.try_clone() {
            Ok(client) => client,
            Err(_) => return TunnelEnd::OriginClosed,
        };
        let pump_end = thread::Builder::new()
            .name("fakes-http-tunnel".to_owned())
            .spawn(move || {
                let _ = io::copy(&mut origin_pump, &mut client_pump);
                first_pump
                    .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
                    .ok();
                match client_pump.shutdown(std::net::Shutdown::Both) {
                    Ok(()) | Err(_) => {}
                }
                match origin_pump.shutdown(std::net::Shutdown::Both) {
                    Ok(()) | Err(_) => {}
                }
            });
        let _ = io::copy(&mut stream, &mut origin);
        first
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .ok();
        match stream.shutdown(std::net::Shutdown::Both) {
            Ok(()) | Err(_) => {}
        }
        match origin.shutdown(std::net::Shutdown::Both) {
            Ok(()) | Err(_) => {}
        }
        if let Ok(pump_end) = pump_end {
            match pump_end.join() {
                Ok(()) | Err(_) => {}
            }
        }
        lock(&shared.tunnels)
            .live
            .retain(|(live, _, _)| *live != id);
        if shared.stopping.load(Ordering::SeqCst) {
            TunnelEnd::Stopped
        } else if first.load(Ordering::SeqCst) == 1 {
            TunnelEnd::ClientClosed
        } else {
            TunnelEnd::OriginClosed
        }
    }

    /// Hands the connection to a tunnel: the stream, blocking again,
    /// with the bytes read past the head.
    pub(crate) fn into_handoff(self) -> io::Result<(TcpStream, Vec<u8>)> {
        self.stream.set_nonblocking(false)?;
        let Self { stream, ahead, .. } = self;
        Ok((stream, ahead.into()))
    }
}

/// Fails naming the request whose bound ran out waiting for `phase`.
#[allow(
    clippy::panic,
    reason = "a fake's wait; the panic names the request whose bound ran out"
)]
fn expired(budget: &RequestBudget, name: &str, phase: &str) -> ! {
    panic!(
        "fakes: the HTTP core waited {:?} {phase} {name}",
        budget.duration
    );
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
pub(crate) enum Framing {
    /// Each piece carries its hex length: a recorded stream.
    Chunked,
    /// Pieces go raw: server-sent events.
    Raw,
}

/// How one tunnel ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TunnelEnd {
    /// The client side closed first.
    ClientClosed,
    /// The origin side closed first.
    OriginClosed,
    /// Shutdown ended the tunnel.
    Stopped,
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
pub(crate) struct PauseControl {
    reached: mpsc::Receiver<()>,
    release: mpsc::Sender<()>,
}

impl Pause {
    /// A pause and its control.
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

impl PauseControl {
    /// Waits for the next arrival, failing the test on expiry.
    pub(crate) fn wait_hit(&self, deadline: &Deadline, what: &str) {
        deadline.recv_or_fail(&self.reached, what);
    }

    /// Releases the holder.
    pub(crate) fn release(&self) {
        match self.release.send(()) {
            Ok(()) | Err(_) => {}
        }
    }
}

/// One write's wait context: its operation budget and acknowledgment
/// state.
struct WaitCtx<'a> {
    budget: &'a RequestBudget,
    name: &'a str,
    shared: &'a Arc<Shared>,
    seq: &'a mut u64,
    acked: &'a mut Option<u64>,
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

/// One bounded HTTP/1.1 core on a loopback port. Clone shares the server;
/// dropping any clone stops it, so a thread needing the server past its
/// own end borrows it under a scope instead of holding a clone.
#[derive(Clone)]
pub(crate) struct HttpServer {
    shared: Arc<Shared>,
}

impl HttpServer {
    /// Binds a loopback port on `clock` with `limits`: it accepts when
    /// this returns.
    pub(crate) fn bind(clock: Arc<dyn Clock>, limits: Limits) -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let shared = Arc::new_cyclic(|shared| Shared {
            listener: Mutex::new(Some(listener)),
            clock: Arc::clone(&clock),
            limits,
            stopping: AtomicBool::new(false),
            accept: Mutex::new(None),
            conns: Mutex::new(Vec::new()),
            wake: Mutex::new(0),
            wake_changed: Condvar::new(),
            _waker: Arc::new(ClockWaker {
                shared: shared.clone(),
            }),
            acks: Mutex::new(None),
            gate: Mutex::new(None),
            idle: Mutex::new(0),
            idle_changed: Condvar::new(),
            #[cfg(test)]
            windows: Mutex::new(0),
            #[cfg(test)]
            windows_changed: Condvar::new(),
            tunnels: Mutex::new(TunnelState {
                next: 0,
                live: Vec::new(),
            }),
        });
        // Subscribed before any waiter checks: no advance slips between a
        // check and its park.
        clock.subscribe(Arc::downgrade(&shared._waker) as std::sync::Weak<dyn Wake>);
        Ok(Self { shared })
    }

    /// The loopback address the core listens on.
    pub(crate) fn addr(&self) -> SocketAddr {
        lock(&self.shared.listener)
            .as_ref()
            .and_then(|listener| listener.local_addr().ok())
            .unwrap_or_else(|| SocketAddr::from(([127, 0, 0, 1], 0)))
    }

    /// Waits for one connection under `mode` within a fresh budget, made
    /// before the attempt: `None` on shutdown. A persistent window's
    /// connection charges that window's budget; an empty expected window
    /// fails naming its request, an empty persistent one ends idle.
    #[allow(
        clippy::panic,
        reason = "a fake's wait; the panic names the expected request whose bound ran out"
    )]
    fn accept(&self, mode: &AcceptMode) -> Option<Connection> {
        let budget = RequestBudget::start_on(
            &self.shared.clock,
            self.shared.limits.request,
            self.shared.limits.guard,
        );
        let mut seq = 0;
        let mut acked = None;
        loop {
            match self.accept_once(mode, &budget, &mut seq, &mut acked) {
                AcceptOutcome::Accepted(stream) => {
                    return Some(Connection {
                        stream,
                        budget,
                        name: mode.name().to_owned(),
                        shared: Arc::clone(&self.shared),
                        ahead: VecDeque::new(),
                        seq,
                        acked: None,
                    });
                }
                // The wait's verdict, not a second check: an advance
                // between the two would fail an idle window that expired.
                AcceptOutcome::Idle => match mode {
                    AcceptMode::Expected { name } => {
                        panic!(
                            "fakes: the HTTP core waited {:?} to accept {name}",
                            self.shared.limits.request
                        );
                    }
                    AcceptMode::Persistent => return None,
                },
                AcceptOutcome::Stopped => return None,
            }
        }
    }

    /// Waits for the one connection `name` names: `None` on shutdown.
    pub(crate) fn accept_expected(&self, name: &str) -> Option<Connection> {
        self.accept(&AcceptMode::Expected {
            name: name.to_owned(),
        })
    }

    /// One persistent window: an accepted connection, an idle end, or a
    /// stopped wait.
    pub(crate) fn accept_window(&self) -> WindowOutcome {
        #[cfg(test)]
        self.note_window();
        match self.accept(&AcceptMode::Persistent) {
            Some(conn) => WindowOutcome::Accepted(conn),
            // An empty window ends idle; shutdown ends stopped. The
            // accept above tells them apart everywhere except here, where
            // either restarts the loop: stopping is re-checked on the next
            // attempt, so shutdown still ends the window within one poll.
            None => {
                if self.shared.stopping.load(Ordering::SeqCst) {
                    WindowOutcome::Stopped
                } else {
                    WindowOutcome::Idle
                }
            }
        }
    }

    /// One nonblocking accept attempt inside the window's budget.
    fn accept_once(
        &self,
        mode: &AcceptMode,
        budget: &RequestBudget,
        seq: &mut u64,
        acked: &mut Option<u64>,
    ) -> AcceptOutcome {
        loop {
            if self.shared.stopping.load(Ordering::SeqCst) {
                return AcceptOutcome::Stopped;
            }
            // Taken by shutdown: no more accepts after it.
            let accepted = lock(&self.shared.listener)
                .as_ref()
                .map(|listener| listener.accept());
            match accepted {
                Some(Ok((stream, _))) => {
                    if stream.set_nonblocking(true).is_err() {
                        continue;
                    }
                    return AcceptOutcome::Accepted(stream);
                }
                Some(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => {}
                Some(Err(_)) | None => return AcceptOutcome::Stopped,
            }
            if budget.expired() {
                return AcceptOutcome::Idle;
            }
            match self
                .shared
                .wait(budget, mode.name(), "to accept the connection", seq, acked)
            {
                WaitEnd::Waited => {}
                WaitEnd::Expired => return AcceptOutcome::Idle,
                WaitEnd::Stopped => return AcceptOutcome::Stopped,
            }
        }
    }

    /// Serves every connection with `handler` on core-owned threads, all
    /// reaped by shutdown. An idle window renews its budget and accepts
    /// again.
    pub(crate) fn serve_persistent(&self, handler: impl Fn(Connection) + Send + Sync + 'static) {
        let handler = Arc::new(handler);
        let serving = self.clone();
        let accept = thread::Builder::new()
            .name("fakes-http-accept".to_owned())
            .spawn(move || {
                loop {
                    match serving.accept_window() {
                        WindowOutcome::Accepted(conn) => {
                            let handler = Arc::clone(&handler);
                            match thread::Builder::new()
                                .name("fakes-http-conn".to_owned())
                                .spawn(move || {
                                    handler(conn);
                                }) {
                                Ok(conn) => lock(&serving.shared.conns).push(conn),
                                Err(_) => continue,
                            }
                        }
                        WindowOutcome::Idle => {
                            *lock(&serving.shared.idle) += 1;
                            serving.shared.idle_changed.notify_all();
                        }
                        WindowOutcome::Stopped => break,
                    }
                }
                // Release every connection thread before joining it: each
                // ends its core wait stopped on its own. Bound before the
                // loop, so the mutex is free while joining.
                let conns = std::mem::take(&mut *lock(&serving.shared.conns));
                for conn in conns {
                    match conn.join() {
                        Ok(()) | Err(_) => {}
                    }
                }
            });
        if let Ok(accept) = accept {
            *lock(&self.shared.accept) = Some(accept);
        }
    }

    /// Stops the server: waits end stopped, open tunnels break, and
    /// every serving thread is joined. True when all stopped within the
    /// shutdown bound.
    pub(crate) fn shutdown(&self) -> bool {
        self.shared.stopping.store(true, Ordering::SeqCst);
        *lock(&self.shared.wake) += 1;
        self.shared.wake_changed.notify_all();
        self.shared.idle_changed.notify_all();
        // Take and close the listener: the port refuses connections
        // even while stuck connections outlive the server.
        let _listener = lock(&self.shared.listener).take();
        // Break every blocked tunnel copy before joining its thread.
        for (_, client, origin) in std::mem::take(&mut lock(&self.shared.tunnels).live) {
            match client.shutdown(std::net::Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
            match origin.shutdown(std::net::Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
        // Bound before matching: a match scrutinee temporary would hold
        // the mutex through the whole join below, blocking every drop.
        let accept = lock(&self.shared.accept).take();
        match accept {
            None => true,
            Some(accept) => join_within(accept, SHUTDOWN_WITHIN),
        }
    }

    /// Receives every clock-wait acknowledgment from here on, test-only.
    #[cfg(test)]
    pub(crate) fn test_acknowledgments(&self) -> mpsc::Receiver<WaitAck> {
        let (tx, rx) = mpsc::channel();
        *lock(&self.shared.acks) = Some(tx);
        rx
    }

    /// Holds check-to-park pauses from here on, test-only.
    #[cfg(test)]
    pub(crate) fn test_gate(&self) -> Arc<CheckGate> {
        let gate = Arc::new(CheckGate::new());
        *lock(&self.shared.gate) = Some(Arc::clone(&gate));
        gate
    }

    /// Counts one started window, test-only.
    #[cfg(test)]
    fn note_window(&self) {
        *lock(&self.shared.windows) += 1;
        self.shared.windows_changed.notify_all();
    }

    /// Checks a CONNECT target against the fake's trust boundary: a
    /// loopback numeric address or `localhost:<port>`, mapped without a
    /// blocking DNS lookup. Anything else is a named `InvalidInput`
    /// before any connect, so no fake ever initiates public-network I/O.
    pub(crate) fn check_target(target: &str) -> io::Result<SocketAddr> {
        let (host, port) = match target.rsplit_once(':') {
            Some((host, port)) => (host, port),
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("fakes: the CONNECT target has no port: {target}"),
                ));
            }
        };
        // Bracketed IPv6 loopback first; localhost maps to loopback;
        // other names never resolve here.
        let host = host
            .strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host);
        if host.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("fakes: the CONNECT target has no host: {target}"),
            ));
        }
        let port: u16 = match port.parse() {
            Ok(port) => port,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("fakes: the CONNECT target has no port: {target}"),
                ));
            }
        };
        if host.eq_ignore_ascii_case("localhost") {
            return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
        }
        match host.parse::<std::net::IpAddr>() {
            Ok(addr) if addr.is_loopback() => Ok(SocketAddr::new(addr, port)),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("fakes: the CONNECT target is not a loopback fake: {target}"),
            )),
        }
    }
}

impl Drop for HttpServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Joins `accept` within `within`, leaving a stuck thread blocked rather
/// than hanging the drop. Builds abort on panic, so a join never carries
/// one.
pub(crate) fn join_within(accept: JoinHandle<()>, within: Duration) -> bool {
    let (done, joined) = mpsc::channel::<()>();
    thread::spawn(move || {
        match accept.join() {
            Ok(()) | Err(_) => {}
        }
        match done.send(()) {
            Ok(()) | Err(_) => {}
        }
    });
    Deadline::after(within).recv(&joined).is_ok()
}

/// A lock that outlives a panicked holder: plain data, and a panicked
/// holder has already failed its test.
fn lock<T>(state: &Mutex<T>) -> MutexGuard<'_, T> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "http_server_tests.rs"]
mod tests;
