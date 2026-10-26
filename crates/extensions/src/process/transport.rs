//! Framed pipe I/O to one process extension (`docs/extensions.md`,
//! "Process extensions").
//!
//! The session starts the program with pipes on its stdin and stdout, in
//! the session's workspace. The pipe carries JSON lines: the child reads
//! Fiber's lines from its stdin and writes its own to its stdout. One
//! writer thread owns stdin and sends lines in queue order, so concurrent
//! callers cannot interleave bytes. One reader thread owns stdout and
//! posts complete lines; one drain owns stderr, so a child that logs
//! cannot block. The child runs in its own process group, listed with
//! `support::group`, so a stop reaches the child and everything it
//! started.
//!
//! A blocked read ends on demand: the reader polls the pipe beside a wake
//! pipe, and stopping closes the wake pipe's write end, so a descendant
//! holding the stdout pipe open cannot delay the end. A blocked write ends
//! when the child dies: the shutdown kills the group, and the write fails.
//! No registry, state or emitter lock is held over pipe I/O.

use std::io::{self, Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use contract::CommandId;
use rustix::event::{PollFd, PollFlags, poll};
use support::framing::MAX_LINE;
use support::group::{self, Listing};

/// How many outbound lines wait for the writer thread. A full queue means
/// the child stopped reading while calls keep coming: the child counts as
/// gone, as an MCP server's does.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) const WRITER_QUEUE: usize = 64;

/// How much of the child's stderr is kept, the tail: a flood cannot grow
/// memory without bound, and the end of the log names the failure.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) const STDERR_TAIL: usize = 64 * 1024;

/// How many received bytes may wait for the owner before the child counts
/// as flooding: backpressure when the owner stops draining, bounding the
/// queue the reader posts to without blocking the reader.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) const MAX_QUEUED_BYTES: usize = 16 << 20;

/// The limits a test injects; production uses the defaults above.
#[derive(Debug, Clone, Copy)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct Limits {
    /// The most bytes one line may hold before its line feed.
    pub max_line: usize,
    /// How many received bytes may wait for the owner.
    pub max_queued: usize,
    /// How many outbound lines wait for the writer thread.
    pub writer_queue: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_line: MAX_LINE,
            max_queued: MAX_QUEUED_BYTES,
            writer_queue: WRITER_QUEUE,
        }
    }
}

/// One line the reader posts, or how the reader ended. The reader posts
/// exactly one terminal `Closed`.
#[derive(Debug)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum Incoming {
    /// One complete line, without its line-feed bytes.
    Line(Vec<u8>),
    /// The reader ended; no more lines follow.
    Closed(CloseReason),
}

/// How the reader ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum CloseReason {
    /// End of input with nothing unread.
    Eof,
    /// One line longer than the cap: the pipe closes and the extension
    /// counts as dead.
    TooLong,
    /// Received bytes waiting for the owner passed the flood bound.
    Flooded,
    /// Stopped before end of input.
    Stopped,
    /// The read failed.
    Io(String),
}

/// What `send` refused.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum SendError {
    /// The line does not fit under the cap: it is answered `too_large` in
    /// place of the result, never queued.
    TooLarge,
    /// The writer left or its queue filled: the child counts as gone.
    Gone,
}

/// One supervised line the child wrote, parsed through
/// `contract::commands::CommandLine`. A line that is no JSON object is
/// ignored, as on the sibling child-pipe transports: without an id no
/// answer can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum ParsedLine {
    /// A `call`: a registration, a `host` call or a `state` call.
    Call {
        /// The line's id, answering it.
        id: CommandId,
        /// The call's name as Lua writes it.
        call: String,
        /// The Lua call's arguments.
        args: Vec<serde_json::Value>,
        /// The request answered, when made inside that request's callback.
        request_id: Option<contract::RequestId>,
    },
    /// The `ready` ending the registrations.
    Ready {
        /// The line's id, answering it.
        id: CommandId,
    },
    /// A callback `reply`, answering a request Fiber sent.
    Reply {
        /// The line's id, answering it.
        id: CommandId,
        /// The request answered.
        request_id: contract::RequestId,
        /// The answer: the value, or the failure's code and message.
        answer: ReplyAnswer,
    },
    /// Any other driver command: sent through the `Drive` seam.
    Forward {
        /// The line's id, answering it.
        id: CommandId,
        /// The driver command, such as `steer`.
        command: String,
        /// Its arguments.
        args: serde_json::Map<String, serde_json::Value>,
    },
    /// A command this Fiber does not build: answered `unknown_command`
    /// with the line's own command name.
    Unknown {
        /// The line's id, answering it.
        id: CommandId,
        /// The line's own command name.
        command: String,
    },
    /// A callback `reply` whose answer is no value and no failure:
    /// answered `invalid_arguments`, since only what the Lua function
    /// returns, or its error, answers a request.
    BadReply {
        /// The line's id, answering it.
        id: CommandId,
    },
    /// A JSON object with an id that reads as no command: answered
    /// `malformed`.
    Malformed {
        /// The line's id, answering it.
        id: CommandId,
    },
    /// A built command whose arguments do not fit: answered
    /// `invalid_arguments`.
    Unfit {
        /// The line's id, answering it.
        id: CommandId,
    },
    /// No JSON object with an id: ignored, and the pipe stays open.
    Ignored,
}

/// A callback reply's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) enum ReplyAnswer {
    /// What the Lua function returns; any JSON value, null included.
    Value(serde_json::Value),
    /// The failure, failing the call as a Lua error does.
    Failure {
        /// The failure's code.
        code: contract::ErrorCode,
        /// Fiber's own sentence.
        message: String,
    },
}

/// The sentence a line that reads as no command is rejected with,
/// shared with the doors: it names the line's shape, never its bytes.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) const MALFORMED: &str =
    "A command is one JSON object per line, with a string `id` and `command`.";

/// The sentence a command whose arguments do not fit is rejected with,
/// shared with the doors.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) const UNFIT: &str = "The arguments do not fit this command.";

mod parse;
pub(crate) use parse::parse_line;

/// Ends a blocked pipe read on demand: closing the wake pipe's write end
/// makes its read end ready on every later poll. The pipe is never read
/// from and never written to.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
struct PipeStop {
    wake: Mutex<Option<io::PipeWriter>>,
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl PipeStop {
    fn pipe() -> io::Result<(io::PipeReader, Self)> {
        let (read, write) = io::pipe()?;
        Ok((
            read,
            Self {
                wake: Mutex::new(Some(write)),
            },
        ))
    }

    /// Ends the reader's next poll. Idempotent; neither call fails.
    fn stop(&self) {
        drop(support::lock(&self.wake).take());
    }
}

/// Reads complete lines from `stdout` until end of input, the cap, the
/// flood bound, a read failure or the stop, posting each line and exactly
/// one terminal `Closed`. A partial line at end of input posts before the
/// `Eof`, so a child that exits without a final line feed loses nothing.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn read_loop(
    stdout: ChildStdout,
    max_line: usize,
    max_queued: usize,
    queued: Arc<AtomicUsize>,
    tx: mpsc::Sender<Incoming>,
    woken: io::PipeReader,
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0_u8; 8 * 1024];
    let post = |bytes: Vec<u8>| {
        queued.fetch_add(bytes.len(), Ordering::Relaxed);
        // The owner drains until the terminal close: a full queue ends the
        // pipe through the flood bound before a send can fail.
        match tx.send(Incoming::Line(bytes)) {
            Ok(()) | Err(_) => {}
        }
    };
    let close = |reason: CloseReason| {
        // One terminal close per reader: the owner drains, so only its
        // own stop can race it, and the race reads stopped either way.
        match tx.send(Incoming::Closed(reason)) {
            Ok(()) | Err(_) => {}
        }
    };
    let mut current = stdout;
    loop {
        let mut fds = [
            PollFd::new(&woken, PollFlags::IN),
            PollFd::new(&current, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => {
                close(CloseReason::Io(error.to_string()));
                return;
            }
        }
        // Any readiness on the wake pipe is the stop: nothing is ever
        // written to it, so only closing its write end wakes it. The stop
        // wins over pending data, so a peer that keeps writing cannot
        // delay it. Bytes already read are kept.
        if !fds[0].revents().is_empty() {
            close(CloseReason::Stopped);
            return;
        }
        let read = current.read(&mut chunk);
        match read {
            Ok(0) => {
                if !buf.is_empty() {
                    post(std::mem::take(&mut buf));
                }
                close(CloseReason::Eof);
                return;
            }
            Ok(got) => {
                let mut at = 0;
                while at < got {
                    let Some(rest) = chunk.get(at..got) else {
                        break;
                    };
                    let Some(end) = rest.iter().position(|byte| *byte == b'\n') else {
                        break;
                    };
                    // No line's bytes pass the cap: a line feed ends the
                    // line, and a line longer than the cap ends the pipe.
                    if buf.len() + end > max_line {
                        close(CloseReason::TooLong);
                        return;
                    }
                    buf.extend_from_slice(rest.get(..end).unwrap_or_default());
                    strip_trailing_cr(&mut buf);
                    let line = std::mem::take(&mut buf);
                    if queued.load(Ordering::Relaxed) + line.len() > max_queued {
                        close(CloseReason::Flooded);
                        return;
                    }
                    post(line);
                    at += end + 1;
                }
                if at < got {
                    let Some(tail) = chunk.get(at..got) else {
                        close(CloseReason::Io("the pipe read short".into()));
                        return;
                    };
                    if buf.len() + tail.len() > max_line {
                        close(CloseReason::TooLong);
                        return;
                    }
                    buf.extend_from_slice(tail);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => {
                close(CloseReason::Io(error.to_string()));
                return;
            }
        }
    }
}

/// Strips trailing carriage returns, so a CRLF line reads as its text.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn strip_trailing_cr(line: &mut Vec<u8>) {
    while line.last().is_some_and(|byte| *byte == b'\r') {
        line.pop();
    }
}

/// Writes queued lines to the child's stdin in queue order, then closes
/// it, so the child sees end of input. A write that never finishes blocks
/// only this thread: killing the child fails it, and the thread ends.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn write_loop(mut stdin: ChildStdin, rx: mpsc::Receiver<Vec<u8>>) {
    for mut line in rx {
        line.push(b'\n');
        if stdin.write_all(&line).is_err() {
            return;
        }
    }
}

/// Keeps the tail of the child's stderr, so a child that logs cannot block
/// and a flood cannot grow memory without bound.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
fn stderr_loop(mut stderr: ChildStderr, tail: Arc<Mutex<Vec<u8>>>, woken: io::PipeReader) {
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let mut fds = [
            PollFd::new(&woken, PollFlags::IN),
            PollFd::new(&stderr, PollFlags::IN),
        ];
        match poll(&mut fds, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return,
        }
        if !fds[0].revents().is_empty() {
            return;
        }
        match stderr.read(&mut chunk) {
            Ok(0) => return,
            Ok(got) => {
                let Some(bytes) = chunk.get(..got) else {
                    continue;
                };
                let mut kept = support::lock(&tail);
                kept.extend_from_slice(bytes);
                let over = kept.len().saturating_sub(STDERR_TAIL);
                if over > 0 {
                    kept.drain(..over);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// One process extension's pipes: its child in its own listed group, one
/// ordered writer, one line reader and one stderr drain.
#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
pub(crate) struct Transport {
    writer: Mutex<Option<mpsc::SyncSender<Vec<u8>>>>,
    incoming: Mutex<mpsc::Receiver<Incoming>>,
    queued: Arc<AtomicUsize>,
    stderr: Arc<Mutex<Vec<u8>>>,
    pgid: u32,
    child: Mutex<Option<Child>>,
    listing: Mutex<Option<Listing>>,
    reader_stop: PipeStop,
    stderr_stop: PipeStop,
    max_line: usize,
    threads: Mutex<Vec<thread::JoinHandle<()>>>,
    /// Which child spawn this transport holds: bumped on every respawn,
    /// so an observed death attributes to the spawn that died.
    spawn_id: std::sync::atomic::AtomicU64,
    /// Whether the child exited, set by the reap and never cleared: a
    /// reap consumes the child handle, so only a latch keeps the death
    /// observable after `poll_exit` or `wait_exit` takes it.
    exited: std::sync::atomic::AtomicBool,
    /// Whether the owner stopped the stream: once stopped, every end of
    /// the stream reads `Stopped`, even past the one terminal close.
    stopped: std::sync::atomic::AtomicBool,
}

#[allow(
    dead_code,
    reason = "asserted by tests; production use lands in a later part"
)]
impl Transport {
    /// Starts `program` with `args` in `cwd` and wires its pipes, with the
    /// default limits. The child runs in its own listed process group.
    pub(crate) fn spawn(program: &Path, args: &[String], cwd: &Path) -> io::Result<Self> {
        Self::spawn_with(program, args, cwd, Limits::default())
    }

    /// Starts `program` as [`spawn`](Self::spawn) does, with `limits`.
    pub(crate) fn spawn_with(
        program: &Path,
        args: &[String],
        cwd: &Path,
        limits: Limits,
    ) -> io::Result<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.process_group(0);
        let (child, listing) = group::spawn(&mut cmd).map_err(io::Error::from)?;
        let pgid = listing.pgid();
        let mut child = child;
        let stdin: ChildStdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("the process extension's stdin is missing"))?;
        let stdout: ChildStdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("the process extension's stdout is missing"))?;
        let stderr: ChildStderr = child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("the process extension's stderr is missing"))?;
        let (writer_tx, writer_rx) = mpsc::sync_channel(limits.writer_queue);
        let (incoming_tx, incoming_rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let (reader_woken, reader_stop) = PipeStop::pipe()?;
        let (stderr_woken, stderr_stop) = PipeStop::pipe()?;
        let stderr_tail: Arc<Mutex<Vec<u8>>> = Arc::default();
        let mut threads = Vec::with_capacity(3);
        let writer = thread::Builder::new()
            .name("process writer".into())
            .spawn(move || write_loop(stdin, writer_rx));
        let reader_queued = Arc::clone(&queued);
        let reader = thread::Builder::new()
            .name("process reader".into())
            .spawn(move || {
                read_loop(
                    stdout,
                    limits.max_line,
                    limits.max_queued,
                    reader_queued,
                    incoming_tx,
                    reader_woken,
                );
            });
        let drain_tail = Arc::clone(&stderr_tail);
        let drain = thread::Builder::new()
            .name("process stderr".into())
            .spawn(move || stderr_loop(stderr, drain_tail, stderr_woken));
        for spawned in [writer, reader, drain] {
            match spawned {
                Ok(handle) => threads.push(handle),
                Err(error) => {
                    reader_stop.stop();
                    stderr_stop.stop();
                    return Err(error);
                }
            }
        }
        Ok(Self {
            writer: Mutex::new(Some(writer_tx)),
            incoming: Mutex::new(incoming_rx),
            queued,
            stderr: stderr_tail,
            pgid,
            child: Mutex::new(Some(child)),
            listing: Mutex::new(Some(listing)),
            reader_stop,
            stderr_stop,
            max_line: limits.max_line,
            threads: Mutex::new(threads),
            spawn_id: std::sync::atomic::AtomicU64::new(0),
            exited: std::sync::atomic::AtomicBool::new(false),
            stopped: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The framing cap this transport enforces.
    pub(crate) fn max_line(&self) -> usize {
        self.max_line
    }

    /// The child's process group id.
    pub(crate) fn pgid(&self) -> u32 {
        self.pgid
    }

    /// Which child spawn this transport holds.
    pub(crate) fn spawn_id(&self) -> u64 {
        self.spawn_id.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Moves to the next spawn id after a respawn: ids never repeat
    /// across the owner's spawns, so one observation counts once.
    pub(crate) fn set_spawn_id(&self, id: u64) {
        self.spawn_id.store(id, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the child exited, without reaping it: the exit stays for
    /// `poll_exit` or `wait_exit`, so observing death never consumes the
    /// status a later reap reports. Sticky: a reap takes the child
    /// handle, so the latch keeps the death observable afterwards.
    pub(crate) fn exited(&self) -> bool {
        if self.exited.load(Ordering::Relaxed) {
            return true;
        }
        matches!(
            support::lock(&self.child)
                .as_mut()
                .and_then(|child| child.try_wait().ok()),
            Some(Some(_))
        )
    }

    /// Sends one complete line, without its line feed, in queue order. A
    /// line past the cap is refused `TooLarge` and never queued; a full
    /// writer queue or a writer that left is `Gone`.
    pub(crate) fn send(&self, line: Vec<u8>) -> Result<(), SendError> {
        if line.len() > self.max_line {
            return Err(SendError::TooLarge);
        }
        let writer = support::lock(&self.writer);
        match writer.as_ref().map(|writer| writer.try_send(line)) {
            Some(Ok(())) => Ok(()),
            Some(Err(mpsc::TrySendError::Full(_))) | None => Err(SendError::Gone),
            Some(Err(mpsc::TrySendError::Disconnected(_))) => Err(SendError::Gone),
        }
    }

    /// Receives the next line or close, accounting queued bytes down. The
    /// reader posts exactly one terminal `Closed`, so a stop wakes this
    /// even when the child never closes its end. Past that one close the
    /// reader is gone: the hangup reads `Stopped` when the owner stopped,
    /// since the stop owns the stream's end, and `Io` when it did not.
    /// Only the pump thread receives; the lock only makes the shared
    /// owner compile.
    pub(crate) fn recv(&self) -> Incoming {
        match support::lock(&self.incoming).recv() {
            Ok(Incoming::Line(bytes)) => {
                self.queued.fetch_sub(bytes.len(), Ordering::Relaxed);
                Incoming::Line(bytes)
            }
            Ok(Incoming::Closed(reason)) => Incoming::Closed(reason),
            Err(_) if self.stopped.load(Ordering::Relaxed) => {
                Incoming::Closed(CloseReason::Stopped)
            }
            Err(_) => Incoming::Closed(CloseReason::Io("the reader ended".into())),
        }
    }

    /// The child's stderr so far, oldest first, capped at the tail.
    pub(crate) fn take_stderr(&self) -> Vec<u8> {
        support::lock(&self.stderr).clone()
    }

    /// Stops the reader and the stderr drain and closes the child's
    /// stdin: the child sees end of input, and `recv` wakes with
    /// `Stopped` even when a descendant holds the pipes open. Idempotent.
    pub(crate) fn stop(&self) {
        self.reader_stop.stop();
        self.stderr_stop.stop();
        drop(support::lock(&self.writer).take());
        self.stopped.store(true, Ordering::Relaxed);
    }

    /// Sends `signal` to the child's process group: the child and
    /// everything it started. A group already gone is `Ok(())`.
    pub(crate) fn signal(&self, signal: rustix::process::Signal) -> Result<(), group::Error> {
        group::signal(self.pgid, signal)
    }

    /// Whether the child's group still holds a process.
    pub(crate) fn group_alive(&self) -> bool {
        group::alive(self.pgid)
    }

    /// Reaps the child once it exits, unlisting its group. Returns the
    /// exit when the child already exited or exits now.
    pub(crate) fn poll_exit(&self) -> Option<ExitStatus> {
        let mut child = support::lock(&self.child);
        let status = child.as_mut()?.try_wait().ok()??;
        drop(child.take());
        self.unlist();
        self.exited.store(true, Ordering::Relaxed);
        Some(status)
    }

    /// Waits for the child to exit and reaps it, unlisting its group.
    pub(crate) fn wait_exit(&self) -> io::Result<ExitStatus> {
        let mut child = support::lock(&self.child);
        let Some(mut taken) = child.take() else {
            return Err(io::Error::other("the process extension was already reaped"));
        };
        let status = taken.wait();
        drop(child);
        self.unlist();
        self.exited.store(true, Ordering::Relaxed);
        status
    }

    /// Unlists the child's group, so a later shutdown never signals a
    /// reused id. Runs once: a second call finds no listing.
    fn unlist(&self) {
        if let Some(listing) = support::lock(&self.listing).take() {
            group::live().unlist(listing);
        }
    }

    /// Stops the pipes and joins every thread. The caller stops first and
    /// has killed the child or seen it exit: the stop ends the reader and
    /// the drain at once, and the dead child fails a blocked write, so
    /// every thread ends and the join cannot hang.
    pub(crate) fn join(&self) {
        self.stop();
        let threads = std::mem::take(&mut *support::lock(&self.threads));
        for thread in threads {
            match thread.join() {
                Ok(()) | Err(_) => {}
            }
        }
    }
}
