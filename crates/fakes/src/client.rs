//! A second client on a session's socket (`docs/testing.md`, "Fakes"),
//! including one that reads nothing until told.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

/// How long `Drop` waits for the reader thread to stop, in real time.
///
/// The `shutdown(Both)` just before wakes the reader's `read` at once, so
/// the bound only reports a fake that missed the wake. 2 s matches the
/// client's own test deadline (`client_tests.rs` `DEADLINE`). A passing run
/// never waits on it; it only bounds a hang, adding 2 s per dropped client
/// on the failure path only.
const READER_STOP: Duration = Duration::from_secs(2);

/// The most [`Client::send_by`] writes under one reading of its deadline.
const CHUNK: usize = 4096;

/// A client connected to a session socket. It sends command lines and reads
/// the JSON lines that come back. [`Client::slow`] stops it reading, so the
/// session's writer blocks instead of the test.
pub struct Client {
    state: Arc<Mutex<State>>,
    ready: Arc<Condvar>,
    write: Mutex<UnixStream>,
    shutdown: Mutex<Option<UnixStream>>,
    pending: Mutex<Option<UnixStream>>,
    reader: Mutex<Option<JoinHandle<()>>>,
}

struct State {
    lines: VecDeque<String>,
    slow: bool,
    stop: bool,
    closed: bool,
}

impl Client {
    /// Connects to the session socket at `path`. Nothing is read until
    /// [`Client::recv`] or [`Client::slow`]`(false)`.
    pub fn connect(path: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(path)?;
        let write = stream.try_clone()?;
        let shutdown = stream.try_clone()?;
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                lines: VecDeque::new(),
                slow: false,
                stop: false,
                closed: false,
            })),
            ready: Arc::new(Condvar::new()),
            write: Mutex::new(write),
            shutdown: Mutex::new(Some(shutdown)),
            pending: Mutex::new(Some(stream)),
            reader: Mutex::new(None),
        })
    }

    /// Sends one command line. A missing trailing newline is added, so the
    /// session sees the line as the client typed it.
    pub fn send(&self, line: &str) -> std::io::Result<()> {
        let mut stream = lock(&self.write);
        stream.write_all(line.as_bytes())?;
        if !line.ends_with('\n') {
            stream.write_all(b"\n")?;
        }
        stream.flush()
    }

    /// Sends one command line as [`Client::send`] does, every blocking write
    /// bounded by what `left` says remains of the caller's deadline.
    ///
    /// The line goes in chunks of at most [`CHUNK`] bytes. Before each, `left`
    /// is read once and becomes that write's timeout, so partial progress
    /// never renews the deadline. At zero it returns a `TimedOut` error
    /// without writing; a zero timeout never reaches the socket, which takes
    /// it as an error. On return the socket has no write timeout, so a later
    /// `send` is unbounded as before.
    pub fn send_by(&self, line: &str, left: &dyn Fn() -> Duration) -> std::io::Result<()> {
        let mut bytes = line.as_bytes().to_vec();
        if !line.ends_with('\n') {
            bytes.push(b'\n');
        }
        let mut stream = lock(&self.write);
        let mut rest = bytes.as_slice();
        let sent = loop {
            if rest.is_empty() {
                break Ok(());
            }
            let within = left();
            if within.is_zero() {
                break Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "the deadline passed before the line was written",
                ));
            }
            if let Err(err) = stream.set_write_timeout(Some(within)) {
                break Err(err);
            }
            let chunk = rest.get(..CHUNK).unwrap_or(rest);
            match stream.write(chunk) {
                Ok(0) => break Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => rest = rest.get(n..).unwrap_or_default(),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => break Err(err),
            }
        };
        let unbounded = stream.set_write_timeout(None);
        sent.and(unbounded)
    }

    /// The first line `matches` accepts, parsed as JSON, or as a string when it
    /// is not JSON. The lines before it are dropped. `None` when none arrives
    /// within `within` or the socket closes first; then nothing is consumed.
    pub fn recv_until(&self, within: Duration, matches: impl Fn(&Value) -> bool) -> Option<Value> {
        if !lock(&self.state).slow {
            self.ensure_reader();
        }
        let guard = lock(&self.state);
        let (mut guard, _) = self
            .ready
            .wait_timeout_while(guard, within, |state| {
                !state.lines.iter().any(|line| {
                    matches(&serde_json::from_str(line).unwrap_or(Value::String(line.clone())))
                }) && !state.closed
            })
            .unwrap_or_else(PoisonError::into_inner);
        let at = guard.lines.iter().position(|line| {
            matches(&serde_json::from_str(line).unwrap_or(Value::String(line.clone())))
        })?;
        for _ in 0..at {
            guard.lines.pop_front();
        }
        let line = guard.lines.pop_front()?;
        drop(guard);
        Some(serde_json::from_str(&line).unwrap_or(Value::String(line)))
    }

    /// The next line, parsed as JSON, or `None` when none arrives within
    /// `within` or the socket is closed. A line that is not JSON is returned
    /// as a string, so a test can see what arrived.
    pub fn recv(&self, within: Duration) -> Option<Value> {
        self.recv_until(within, |_| true)
    }

    /// When `paused`, reads nothing from the socket until `slow(false)`.
    /// One read already in progress may finish; a client paused before its
    /// first [`Client::recv`] has not started one.
    pub fn slow(&self, paused: bool) {
        lock(&self.state).slow = paused;
        self.ready.notify_all();
        if !paused {
            self.ensure_reader();
        }
    }

    fn ensure_reader(&self) {
        let mut slot = lock(&self.reader);
        if slot.is_some() {
            return;
        }
        let Some(stream) = lock(&self.pending).take() else {
            return;
        };
        let state = Arc::clone(&self.state);
        let ready = Arc::clone(&self.ready);
        match std::thread::Builder::new()
            .name("fake-client".to_owned())
            .spawn(move || read_lines(stream, state, ready))
        {
            Ok(handle) => *slot = Some(handle),
            Err(_) => {
                lock(&self.state).closed = true;
                self.ready.notify_all();
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        {
            let mut state = lock(&self.state);
            state.stop = true;
            state.slow = false;
        }
        self.ready.notify_all();
        if let Some(stream) = lock(&self.shutdown).take() {
            match stream.shutdown(std::net::Shutdown::Both) {
                Ok(()) | Err(_) => {}
            }
        }
        if let Some(handle) = lock(&self.reader).take() {
            // The shutdown above wakes the reader's `read` at once
            // (`docs/testing.md`, "Waits and timeouts"); the bound only
            // reports a reader that missed the wake.
            match crate::within("the fake client's reader to stop", READER_STOP, move || {
                handle.join()
            }) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

fn read_lines(stream: UnixStream, state: Arc<Mutex<State>>, ready: Arc<Condvar>) {
    let mut read = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        {
            let mut guard = lock(&state);
            while guard.slow && !guard.stop {
                guard = ready.wait(guard).unwrap_or_else(PoisonError::into_inner);
            }
            if guard.stop {
                return;
            }
        }
        buf.clear();
        match read.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => {
                lock(&state).closed = true;
                ready.notify_all();
                return;
            }
            Ok(_) => {
                while buf
                    .last()
                    .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
                {
                    buf.pop();
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
                lock(&state).lines.push_back(text);
                ready.notify_all();
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
