//! The reading side: a session's log as it stands, and watchers that receive
//! each event as it is written. Readers take no lock: the log is append-only,
//! and a reader stops at the last complete line (`docs/events.md`, "Writing").

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use contract::Envelope;

use crate::offsets::{After, Offsets, Page};
use crate::{EVENTS, Error, io_at};

/// Every durable line in the session directory `dir`, in order, up to the
/// last complete line. A torn tail, a line cut short by a crash, is skipped;
/// a complete line that is not an event line is an error naming it.
pub fn read(dir: &Path) -> Result<Vec<Envelope>, Error> {
    lines(dir)?.collect()
}

/// The durable lines in the session directory `dir`, read one at a time as
/// [`read`] reads them, holding one line and never the whole file. A missing
/// log is [`Error::NotFound`] here; after the first error the lines end.
pub fn lines(dir: &Path) -> Result<Lines, Error> {
    let path = dir.join(EVENTS);
    let file = File::open(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            Error::NotFound(dir.to_owned())
        } else {
            io_at(&path)(e)
        }
    })?;
    Ok(Lines {
        reader: BufReader::new(file),
        path,
        buf: Vec::new(),
        number: 0,
        offset: 0,
        done: false,
    })
}

/// A session's durable lines, read one at a time: see [`lines`].
pub struct Lines {
    reader: BufReader<File>,
    path: PathBuf,
    /// The line being read.
    buf: Vec<u8>,
    /// How many complete lines have been read.
    number: usize,
    /// The byte offset just past the last complete line read.
    offset: u64,
    /// An error was returned or the complete lines ran out.
    done: bool,
}

impl Lines {
    /// The byte offset just past the last complete line returned: where the
    /// next line starts, and, once the lines end, where a torn tail starts.
    pub(crate) fn offset(&self) -> u64 {
        self.offset
    }

    /// The next complete line's bytes, newline included: `None` at the end
    /// or at a torn tail. An I/O error ends the lines. Each complete line
    /// advances the line number and the offset.
    pub(crate) fn next_raw(&mut self) -> Option<Result<&[u8], Error>> {
        if self.done {
            return None;
        }
        self.buf.clear();
        let read = match self.reader.read_until(b'\n', &mut self.buf) {
            Ok(read) => read,
            Err(e) => {
                self.done = true;
                return Some(Err(io_at(&self.path)(e)));
            }
        };
        // The end of the file, or a torn tail: a line with no newline.
        if self.buf.last() != Some(&b'\n') {
            self.done = true;
            return None;
        }
        self.number += 1;
        self.offset += u64::try_from(read).unwrap_or(u64::MAX);
        Some(Ok(&self.buf))
    }

    /// A complete line that did not parse is an error naming it: the number
    /// of the last complete line returned.
    pub(crate) fn unreadable(&self, source: serde_json::Error) -> Error {
        Error::Unreadable {
            path: self.path.clone(),
            line: self.number,
            source,
        }
    }
}

impl Iterator for Lines {
    type Item = Result<Envelope, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = match self.next_raw()? {
            Ok(bytes) => bytes,
            Err(error) => return Some(Err(error)),
        };
        let parsed = serde_json::from_slice(bytes);
        let done = parsed.is_err();
        let parsed = parsed.map_err(|source| self.unreadable(source));
        self.done = done;
        Some(parsed)
    }
}

/// The length of `bytes` up to and including its last newline: everything
/// before a torn tail.
pub(crate) fn complete_len(bytes: &[u8]) -> usize {
    bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1)
}

// debt: 1,024 is picked, not measured. The queue grows only as lines
// wait in it. The busy-session memory budget (`docs/performance.md`) and a
// slow watcher's measured lag would set it.
/// How many events a watcher's queue holds before it falls behind, and how
/// many durable lines one page reads at most. A page is also bounded by
/// [`PAGE_BYTES`], so it may hold fewer.
pub(crate) const CAPACITY: usize = 1024;

// debt: 1 MiB is picked, not measured. The resume rows of the benchmark
// (`docs/performance.md`) would move it: a larger page reads a long log in
// fewer reads, a smaller one holds less of it at once.
/// How many bytes of log one page reads at most, so a watcher never holds a
/// log's worth of lines. A line longer than this is a page of its own.
pub(crate) const PAGE_BYTES: u64 = 1024 * 1024;

/// One watcher's bounded queue, shared by the log that fills it and the
/// watcher that drains it. The log holds it weakly, so a dropped watcher's
/// queue is freed at once.
#[derive(Default)]
pub(crate) struct Queue {
    state: Mutex<State>,
    ready: Condvar,
}

#[derive(Default)]
struct State {
    /// Ordinary lines and kept lines in one order. A kept line stays where
    /// it was pushed, including ahead of a catch-up.
    queue: VecDeque<Envelope>,
    /// The queue filled and lines were dropped since the watcher last caught
    /// up from the log.
    lagged: bool,
    end: End,
}

/// Whether the log will send more.
#[derive(Default)]
enum End {
    #[default]
    Open,
    /// The log is gone.
    Closed,
    /// A write failed and the log stopped.
    Failed { session: String, cause: String },
}

impl Queue {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hands the watcher `line` without ever blocking the writer. A watcher
    /// that has fallen behind gets nothing until it catches up: it re-reads
    /// the durable lines it missed from the log, and the ephemeral ones are
    /// lost, which costs nothing (`docs/architecture.md`, "Streaming").
    pub(crate) fn push(&self, line: &Envelope) {
        let mut state = self.lock();
        if state.lagged || state.queue.len() >= CAPACITY {
            state.lagged = true;
        } else {
            state.queue.push_back(line.clone());
        }
        drop(state);
        self.ready.notify_one();
    }

    /// Hands the watcher `line` even when the queue is full or lagged, in
    /// push order with the ordinary lines. [`Queue::push`] would drop it.
    pub(crate) fn push_kept(&self, line: &Envelope) {
        // debt: kept lines are bounded only by the client's own command rate (one acknowledgement per command it sends); a cap on unsent acknowledgements per connection if a flooding client is ever seen.
        let mut state = self.lock();
        state.queue.push_back(line.clone());
        drop(state);
        self.ready.notify_one();
    }

    /// Tells the watcher the log is gone, unless it already knows the log
    /// failed.
    pub(crate) fn close(&self) {
        let mut state = self.lock();
        if matches!(state.end, End::Open) {
            state.end = End::Closed;
        }
        drop(state);
        self.ready.notify_one();
    }

    /// Tells the watcher the log stopped on a failed write. It still gets
    /// every line written before, and catches up first if it fell behind.
    pub(crate) fn fail(&self, session: &str, cause: &str) {
        self.lock().end = End::Failed {
            session: session.to_owned(),
            cause: cause.to_owned(),
        };
        self.ready.notify_one();
    }
}

/// Pushes a line into one watcher's queue. Cloning it does not keep the
/// watcher alive: once the watcher is dropped, [`Injector::push`] does
/// nothing.
#[derive(Clone)]
pub struct Injector {
    queue: Weak<Queue>,
}

impl Injector {
    /// Hands `line` to the watcher, without waiting. A watcher that has
    /// fallen behind drops it, as it drops any other line, and a push after
    /// the watcher is dropped is ignored.
    pub fn push(&self, line: Envelope) {
        if let Some(queue) = self.queue.upgrade() {
            queue.push(&line);
        }
    }

    /// Hands `line` to the watcher even when it has fallen behind, in push
    /// order with the ordinary lines. A push after the watcher is dropped is
    /// ignored. A catch-up re-reads the log and does not drop a kept line
    /// still queued.
    pub fn push_kept(&self, line: Envelope) {
        if let Some(queue) = self.queue.upgrade() {
            queue.push_kept(&line);
        }
    }
}

/// Receives a session's events, durable and ephemeral, as they are written,
/// from the moment [`crate::Log::watch`] made it. It never slows the writer:
/// a watcher that falls behind re-reads the durable lines it missed from the
/// log by `seq`, and loses the ephemeral ones. A watcher over a page that
/// cannot be read returns every whole line before the failure, then the
/// failure once, then nothing: it never reads the page or its queue again.
pub struct Watcher {
    queue: Arc<Queue>,
    /// The log's offset table, which a catch-up reads by `seq` from.
    offsets: Arc<Offsets>,
    /// The `seq` of the next durable line this watcher has not yet returned.
    next: u64,
    /// Positions below this never come from a later page. A full
    /// subscriber's backlog stops here, at the table's line count when its
    /// first page was read, so a line appended later arrives through the
    /// queue, behind the kept lines already in it. A catch-up lifts this
    /// to `u64::MAX` to re-read to the table's current end.
    end: u64,
    /// Lines re-read from the log, not yet returned.
    backlog: VecDeque<Envelope>,
    /// What the watcher reads after its backlog: the queue alone, the next
    /// page first, the failure after a failed page's prefix once, or
    /// nothing.
    reading: Reading,
}

/// What a watcher with an empty backlog reads next.
enum Reading {
    /// Only the queue: no page is due.
    Live,
    /// Read the next page from `next` below `end` before the queue.
    Paging,
    /// The backlog is the readable prefix of a failed page; this error
    /// follows it once.
    Failing(Error),
    /// The error was returned; every later call returns `Ok(None)` without
    /// reading the page or the queue.
    Ended,
}

/// What the watcher takes from its queue.
enum Taken {
    Line(Envelope),
    CatchUp,
    End(End),
}

/// How long [`Watcher::next_line`] waits for its queue.
#[derive(Clone, Copy)]
enum Wait {
    /// Until a line, the need to catch up, or the end arrives.
    Forever,
    /// Not at all: only what is already available.
    Now,
    /// Until `Duration` passes with nothing arriving in the queue. Each
    /// wait on the queue gets the full duration again.
    For(Duration),
}

impl Watcher {
    pub(crate) fn new(queue: Arc<Queue>, offsets: Arc<Offsets>, next: u64) -> Self {
        Self {
            queue,
            offsets,
            next,
            end: u64::MAX,
            backlog: VecDeque::new(),
            reading: Reading::Live,
        }
    }

    /// A watcher from `seq` 0 whose first lines are `first`'s, the log's
    /// first page, then the pages after it while they hold more, then
    /// whatever arrives after it was registered. When the first page
    /// failed, its readable prefix comes first, then the failure once,
    /// then nothing. `end` is the table's line count when the first page
    /// was read: later pages never go past it, so a line appended after it
    /// was registered arrives through the queue. `next` starts at 0 so a
    /// line already queued is skipped once a page has returned it.
    pub(crate) fn starting(
        queue: Arc<Queue>,
        offsets: Arc<Offsets>,
        first: Page,
        end: u64,
    ) -> Self {
        let reading = match first.after {
            After::Done => Reading::Live,
            After::More => Reading::Paging,
            After::Failed(error) => Reading::Failing(error),
        };
        Self {
            queue,
            offsets,
            next: 0,
            end,
            reading,
            backlog: VecDeque::from(first.lines),
        }
    }

    /// A handle that pushes lines into this watcher's queue and no other.
    /// The handle does not keep the watcher alive.
    pub fn injector(&self) -> Injector {
        Injector {
            queue: Arc::downgrade(&self.queue),
        }
    }

    /// The next event, waiting for one to be written. `None` once the log is
    /// dropped and every event written before that has been returned; an
    /// error, after those events, if the log stopped on a failed write. A
    /// page that cannot be read gives every whole line before the failure,
    /// then the failure once, then `None` forever.
    pub fn recv(&mut self) -> Result<Option<Envelope>, Error> {
        // A wait without a deadline never gives up.
        self.next_line(Wait::Forever).unwrap_or(Ok(None))
    }

    /// The next event if one is available now, without waiting: queued
    /// lines first, then, when the watcher fell behind, the durable lines
    /// it missed, re-read from the log. `None` when nothing is available.
    /// A failed page's readable prefix is available, then its failure,
    /// then nothing.
    pub fn try_recv(&mut self) -> Result<Option<Envelope>, Error> {
        self.next_line(Wait::Now).unwrap_or(Ok(None))
    }

    /// What [`Watcher::recv`] would return, if it returns before `timeout`
    /// passes with nothing arriving in this watcher's queue; `None` if it
    /// does not. A timeout consumes nothing: the next call carries on. A
    /// failed page's prefix and failure need no wait and return at once.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<Result<Option<Envelope>, Error>> {
        self.next_line(Wait::For(timeout))
    }

    /// What [`Watcher::recv`] returns, or `None` when nothing is available
    /// within `wait`.
    fn next_line(&mut self, wait: Wait) -> Option<Result<Option<Envelope>, Error>> {
        loop {
            let line = match self.backlog.pop_front() {
                Some(line) => line,
                None => {
                    if matches!(self.reading, Reading::Paging) {
                        self.page();
                        continue;
                    }
                    // The backlog was a failed page's readable prefix: its
                    // error follows once, then the watcher reads as closed
                    // without reading the page or the queue again.
                    match std::mem::replace(&mut self.reading, Reading::Ended) {
                        Reading::Failing(error) => return Some(Err(error)),
                        Reading::Ended => return Some(Ok(None)),
                        reading @ (Reading::Live | Reading::Paging) => {
                            self.reading = reading;
                        }
                    }
                    let taken = match wait {
                        Wait::Forever => Some(self.take()),
                        Wait::Now => poll(&mut self.queue.lock()),
                        Wait::For(timeout) => self.take_timeout(timeout),
                    };
                    match taken {
                        // Nothing arrived in time; nothing was consumed.
                        None => return None,
                        Some(Taken::Line(line)) => line,
                        Some(Taken::CatchUp) => {
                            self.end = u64::MAX;
                            self.reading = Reading::Paging;
                            continue;
                        }
                        Some(Taken::End(End::Failed { session, cause })) => {
                            return Some(Err(Error::Poisoned { session, cause }));
                        }
                        Some(Taken::End(End::Open | End::Closed)) => return Some(Ok(None)),
                    }
                }
            };
            match line.seq {
                // Already returned, from the queue or from the log.
                Some(seq) if seq.0 < self.next => {}
                Some(seq) => {
                    self.next = seq.0 + 1;
                    return Some(Ok(Some(line)));
                }
                None => return Some(Ok(Some(line))),
            }
        }
    }

    /// Reads the next page of durable lines the watcher has not returned,
    /// from `next` below `end`, and installs its lines and what follows
    /// them: more pages, nothing, or the failure after the prefix, which
    /// the watcher returns once the prefix is drained. There may be more
    /// while lines below `end` remain past the page.
    fn page(&mut self) {
        let page = self.offsets.page(self.next, self.end);
        self.reading = match page.after {
            After::Done => Reading::Live,
            After::More => Reading::Paging,
            After::Failed(error) => Reading::Failing(error),
        };
        self.backlog = page.lines.into();
    }

    /// Waits for a queued line, for the need to catch up, or for the end,
    /// in that order. Kept lines are in the queue, so they come out before
    /// a catch-up and a catch-up does not drop one still queued. The lagged
    /// flag is cleared before the log is re-read, so a line written after
    /// the re-read is queued, never lost. The end is handed over once and
    /// then reads as closed.
    fn take(&self) -> Taken {
        let mut state = self.queue.lock();
        loop {
            if let Some(taken) = poll(&mut state) {
                return taken;
            }
            state = self
                .queue
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// What [`Watcher::take`] would return, if it returns before `timeout`
    /// passes with nothing arriving in this watcher's queue; `None` if it
    /// does not. Gives up when `timeout` passes with nothing arriving in
    /// its queue: the backlog drain and a catch-up re-read are not waits,
    /// and a wait after a catch-up that yielded nothing new gets the full
    /// `timeout` again.
    fn take_timeout(&self, timeout: Duration) -> Option<Taken> {
        let state = self.queue.lock();
        let (mut state, _wait) = self
            .queue
            .ready
            .wait_timeout_while(state, timeout, |state| !pending(state))
            .unwrap_or_else(PoisonError::into_inner);
        poll(&mut state)
    }
}

/// What is available now, in [`Watcher::take`]'s order; `None` when nothing
/// is and the log is open.
fn poll(state: &mut State) -> Option<Taken> {
    if let Some(line) = state.queue.pop_front() {
        return Some(Taken::Line(line));
    }
    if state.lagged {
        state.lagged = false;
        return Some(Taken::CatchUp);
    }
    match std::mem::replace(&mut state.end, End::Closed) {
        End::Open => {
            state.end = End::Open;
            None
        }
        end @ (End::Closed | End::Failed { .. }) => Some(Taken::End(end)),
    }
}

/// Whether [`poll`] would return something: a queued line, the need to
/// catch up, or the end. An end already handed over reads as closed, so it
/// still counts. The [`Watcher::take_timeout`] predicate, in [`poll`]'s
/// order, without consuming anything.
fn pending(state: &State) -> bool {
    !state.queue.is_empty() || state.lagged || !matches!(state.end, End::Open)
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
