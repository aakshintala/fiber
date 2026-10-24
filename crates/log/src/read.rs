//! The reading side: a session's log as it stands, and watchers that receive
//! each event as it is written. Readers take no lock: the log is append-only,
//! and a reader stops at the last complete line (`docs/events.md`, "Writing").

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use contract::{Envelope, Seq, SessionId};

use crate::offsets::{After, Offsets, Page};
use crate::{EVENTS, Error, io_at, open_log};

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
    let file = open_log(dir)?;
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

/// The most of a log's end read to find its last line or scan it back:
/// `fiber_exited` and `rewound` are far shorter, so a longer last line
/// is neither.
const TAIL: u64 = 64 * 1024;

/// Whether `id` is one normal path component: non-empty with no
/// separator, never `.` or `..`. The check sits where the path is
/// joined, so no caller can escape `sessions/`.
fn valid_id(id: &SessionId) -> bool {
    if id.0.is_empty() {
        return false;
    }
    let mut parts = std::path::Path::new(&id.0).components();
    matches!(parts.next(), Some(std::path::Component::Normal(_))) && parts.next().is_none()
}

/// The first project in key order holding `id`, with `logged` choosing
/// the predicate: a directory, or a directory with a log file. One
/// `read_dir` of `projects/` per call; no `sessions/` is ever scanned.
fn walk(home: &Path, id: &SessionId, logged: bool) -> Option<(String, PathBuf)> {
    if !valid_id(id) {
        return None;
    }
    let mut projects: Vec<(String, PathBuf)> = std::fs::read_dir(home.join("projects"))
        .ok()?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let key = entry.file_name().to_string_lossy().into_owned();
            Some((key, entry.path()))
        })
        .collect();
    projects.sort_by(|a, b| a.0.cmp(&b.0));
    projects.into_iter().find_map(|(key, project)| {
        let dir = project.join("sessions").join(&id.0);
        let held = if logged {
            dir.join(EVENTS).is_file()
        } else {
            dir.is_dir()
        };
        held.then_some((key, dir))
    })
}

/// Finds session `id`'s directory under any project: its project key
/// and `home/projects/<key>/sessions/<id>`, the first project in key
/// order where that path is a directory. `None` when `projects/` is
/// unreadable, no project holds it, or `id` is not one normal path
/// component.
pub fn find_session(home: &Path, id: &SessionId) -> Option<(String, PathBuf)> {
    walk(home, id, false)
}

/// As [`find_session`], but a project counts only when
/// `sessions/<id>/events.jsonl` is a file: the filter runs per project
/// before the first is chosen, so a log-less directory cannot mask a
/// later log.
pub fn find_logged_session(home: &Path, id: &SessionId) -> Option<(String, PathBuf)> {
    walk(home, id, true)
}

/// The `kind` of one JSON log line. `None` when it does not parse or
/// holds no string `kind`.
pub fn kind_of(line: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(line).ok()?;
    value.get("kind")?.as_str().map(str::to_owned)
}

/// The `kind` of the log's last line, read from at most [`TAIL`] bytes
/// of its end. A final line without a trailing newline counts when it
/// parses. `None` when the log is missing, unreadable, empty, the last
/// line does not parse, or the last line is longer than the window.
pub fn last_kind(dir: &Path) -> Option<String> {
    let mut file = File::open(dir.join(EVENTS)).ok()?;
    let len = file.metadata().ok()?.len();
    let from = len.saturating_sub(TAIL);
    use std::io::{Read, Seek, SeekFrom};
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).ok()?;
    let body = tail.strip_suffix(b"\n").unwrap_or(&tail);
    let mut lines = body.rsplitn(2, |byte| *byte == b'\n');
    let line = lines.next()?;
    // The whole tail is one line only when it is the whole file.
    if lines.next().is_none() && from > 0 {
        return None;
    }
    kind_of(line)
}

/// One past the last `\n` in the log, or 0 when it has none, is empty,
/// is missing or cannot be read. Never the raw length: a torn tail is
/// discarded, and a resumed writer truncates a partial line before
/// appending, so the resumed `fiber_exited` can land at that line's
/// start. Reads backwards from the end in [`TAIL`]-sized chunks,
/// stopping at the first chunk holding a newline.
pub fn complete_len_of(dir: &Path) -> u64 {
    let file = match File::open(dir.join(EVENTS)) {
        Ok(file) => file,
        Err(_) => return 0,
    };
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    complete_len_in(file, len).unwrap_or(0)
}

/// The work behind [`complete_len_of`], over an open log, so a test can
/// count reads.
fn complete_len_in(reader: impl std::io::Read + std::io::Seek, len: u64) -> std::io::Result<u64> {
    use std::io::SeekFrom;
    let mut reader = reader;
    if len == 0 {
        return Ok(0);
    }
    let mut end = len;
    while end != 0 {
        let start = end.saturating_sub(TAIL);
        reader.seek(SeekFrom::Start(start))?;
        let size = usize::try_from(end - start)
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "log too long"))?;
        let mut chunk = vec![0_u8; size];
        reader.read_exact(&mut chunk)?;
        if let Some(at) = chunk.iter().rposition(|b| *b == b'\n') {
            return Ok(start + at as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// The complete lines (newline stripped, empty lines dropped) inside the
/// last [`TAIL`] bytes. A last line with no trailing newline is torn and
/// dropped. A first line is dropped when the window starts mid-line
/// (the byte before the window is not `\n`), kept when the window
/// starts right after a newline. Empty when the log cannot be read.
pub fn tail_lines(dir: &Path) -> Vec<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = match File::open(dir.join(EVENTS)) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let len = match file.metadata().map(|meta| meta.len()) {
        Ok(len) => len,
        Err(_) => return Vec::new(),
    };
    let from = len.saturating_sub(TAIL);
    let mid_line = if from > 0 {
        let mut probe = [0; 1];
        if file
            .seek(SeekFrom::Start(from - 1))
            .and_then(|_| file.read_exact(&mut probe))
            .is_err()
        {
            return Vec::new();
        }
        probe[0] != b'\n'
    } else {
        false
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return Vec::new();
    }
    let mut tail = Vec::new();
    if file.read_to_end(&mut tail).is_err() {
        return Vec::new();
    }
    let mut parts = tail.split(|byte| *byte == b'\n');
    // The bytes after the last newline are torn.
    parts.next_back();
    if mid_line {
        parts.next();
    }
    parts
        .filter(|line| !line.is_empty())
        .map(|line| line.to_vec())
        .collect()
}

/// Where a scan of the log from `from` ended, and whether the log was
/// replaced under the offset.
pub struct Scan {
    /// One past the last complete line handed over (`from` when none).
    pub end: u64,
    /// The file was shorter than `from` and was read from 0 instead.
    pub restarted: bool,
}

/// Hands `each` every complete line (newline stripped) from byte `from`
/// to the end of the last complete line, one at a time, never the whole
/// file. A partial tail is not handed over and `end` stays before it.
/// A file shorter than `from` was replaced: reads from 0 with
/// `restarted`. `None` when the log cannot be opened or seeked; a read
/// error ends the scan at the last complete line.
pub fn lines_from(dir: &Path, from: u64, mut each: impl FnMut(&[u8])) -> Option<Scan> {
    use std::io::{BufRead, Seek, SeekFrom};
    let file = File::open(dir.join(EVENTS)).ok()?;
    let len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let (mut read, restarted) = if len < from { (0, true) } else { (from, false) };
    let mut reader = BufReader::new(file);
    if reader.seek(SeekFrom::Start(read)).is_err() {
        return None;
    }
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let Some((&last, line)) = buf.split_last() else {
                    break;
                };
                if last != b'\n' {
                    break;
                }
                each(line);
                read += buf.len() as u64;
            }
        }
    }
    Some(Scan {
        end: read,
        restarted,
    })
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
        support::lock(&self.state)
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
    /// The opening fold's fixed boundary, independent of later catch-up.
    last_seq: Option<Seq>,
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
            last_seq: None,
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
            last_seq: end.checked_sub(1).map(Seq),
            reading,
            backlog: VecDeque::from(first.lines),
        }
    }

    /// The last durable line in the opening fold; absent for an empty
    /// log or a watcher that reads only live lines. Fixed at creation.
    pub fn last_seq(&self) -> Option<Seq> {
        self.last_seq
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

    /// The next line pushed into this watcher's queue, waiting for one,
    /// whatever the log's end. For a caller that has seen [`Watcher::recv`]
    /// return `None`: only kept lines arrive then, and `recv` would return
    /// `None` again at once. It never re-reads the log and never returns
    /// the end.
    pub fn recv_after_end(&mut self) -> Envelope {
        let mut state = self.queue.lock();
        loop {
            if let Some(line) = state.queue.pop_front() {
                return line;
            }
            // Test-only pause point: a caller parks here with nothing
            // queued, so a test that waits for it knows the call has not
            // returned (`docs/testing.md`, "Waits and timeouts").
            #[cfg(test)]
            parked_after_end::note();
            state = self
                .queue
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
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

/// The test-only park signal for [`Watcher::recv_after_end`]: a race's
/// pause point no outside seam reaches (`docs/testing.md`, "Waits and
/// timeouts"). `recv_after_end` notes each time it parks with nothing
/// queued; a test waits for the note, asserts the call has not returned,
/// then pushes a line and asserts the positive fact. Unit tests only;
/// integration tests do not need a seam here.
#[cfg(test)]
pub(crate) mod parked_after_end {
    use std::sync::{Condvar, Mutex, OnceLock, PoisonError};
    use std::time::Duration;

    /// How long [`wait`] waits for the park before failing the test.
    const BOUND: Duration = Duration::from_secs(10);

    struct Park {
        seen: Mutex<bool>,
        changed: Condvar,
    }

    static PARK: OnceLock<Park> = OnceLock::new();

    fn park() -> &'static Park {
        PARK.get_or_init(|| Park {
            seen: Mutex::new(false),
            changed: Condvar::new(),
        })
    }

    /// Clears the signal between tests.
    pub(crate) fn clear() {
        *support::lock(&park().seen) = false;
    }

    /// Notes that a caller parked in `recv_after_end` with nothing queued.
    pub(crate) fn note() {
        *support::lock(&park().seen) = true;
        park().changed.notify_all();
    }

    /// Waits until a caller parked in `recv_after_end` with nothing
    /// queued, failing the test past the bound.
    #[track_caller]
    pub(crate) fn wait() {
        let guard = support::lock(&park().seen);
        let (guard, _) = park()
            .changed
            .wait_timeout_while(guard, BOUND, |seen| !*seen)
            .unwrap_or_else(PoisonError::into_inner);
        assert!(
            *guard,
            "a caller parked in recv_after_end with nothing queued"
        );
    }
}
