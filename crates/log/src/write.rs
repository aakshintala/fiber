//! The writing side: one writer per session, which mints `seq`, appends
//! durable lines, fsyncs them in the order `docs/events.md`, "Writing", sets,
//! and fans every event out to watchers.

use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use contract::events::{Class, Event};
use contract::{ActionId, Envelope, SCHEMA_VERSION, Seq, SessionId, TurnId};

use crate::read::{Queue, Watcher, complete_len};
use crate::{ARTIFACTS, EVENTS, Error, LOCK, io_at, session_path};

/// A session's log, open for writing. Only one exists per session at a time,
/// across every process: the lock on `session.lock` is held until it is
/// dropped. It is shared by whoever emits events, behind one lock
/// (`docs/architecture.md`, "The threads").
pub struct Log {
    inner: Mutex<Inner>,
}

struct Inner {
    session_id: SessionId,
    dir: PathBuf,
    events: File,
    /// Holds the session's lock for as long as it is open.
    _lock: File,
    /// The `seq` the next durable line gets.
    next: u64,
    fsyncs: u64,
    watchers: Vec<Weak<Queue>>,
}

impl Log {
    /// Creates the session directory `id` in `sessions` (see
    /// [`crate::sessions_dir`]), holding `events.jsonl`, `session.lock` and
    /// `artifacts/` and nothing else, and takes its lock. Refuses a session
    /// that already exists.
    pub fn create(sessions: &Path, id: SessionId) -> Result<Self, Error> {
        let dir = session_path(sessions, &id);
        // Fiber home's directories are private to the person (`docs/state.md`).
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(sessions)
            .map_err(io_at(sessions))?;
        DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(io_at(&dir))?;
        let artifacts = dir.join(ARTIFACTS);
        DirBuilder::new()
            .mode(0o700)
            .create(&artifacts)
            .map_err(io_at(&artifacts))?;
        let lock = lock(&dir, &id)?;
        let path = dir.join(EVENTS);
        let events = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&path)
            .map_err(io_at(&path))?;
        let mut inner = Inner::new(id, dir, events, lock, 0);
        // A crash must not lose the session's directory entries.
        inner.sync_dir(sessions)?;
        let dir = inner.dir.clone();
        inner.sync_dir(&dir)?;
        Ok(Self::from(inner))
    }

    /// Opens the existing session `id` in `sessions` for writing, taking its
    /// lock. A torn tail left by a crash is truncated first, and `seq`
    /// carries on from the last complete line.
    pub fn open(sessions: &Path, id: SessionId) -> Result<Self, Error> {
        let dir = session_path(sessions, &id);
        let path = dir.join(EVENTS);
        if !path.is_file() {
            return Err(Error::NotFound(dir));
        }
        let lock = lock(&dir, &id)?;
        let events = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .map_err(io_at(&path))?;
        // ponytail: reads the whole log to find its tail; resume folds the
        // whole log anyway (`docs/events.md`, "Resume").
        let bytes = fs::read(&path).map_err(io_at(&path))?;
        // A no-op unless the tail is torn.
        let complete = u64::try_from(complete_len(&bytes)).unwrap_or(u64::MAX);
        events.set_len(complete).map_err(io_at(&path))?;
        let next = match crate::read(&dir)?.last() {
            Some(line) => line.seq.map_or(0, |s| s.0 + 1),
            None => 0,
        };
        Ok(Self::from(Inner::new(id, dir, events, lock, next)))
    }

    /// Emits `event` about `turn_id` and `action_id`, where they apply, and
    /// returns its line. A durable event gets the next `seq` and is appended
    /// to the log, fsynced when "Writing" says; an ephemeral one only goes to
    /// watchers. Either way every watcher receives it.
    pub fn append(
        &self,
        event: &Event,
        turn_id: Option<TurnId>,
        action_id: Option<ActionId>,
    ) -> Result<Envelope, Error> {
        let mut inner = self.lock();
        let mut line = Envelope {
            kind: event.kind().to_owned(),
            session_id: inner.session_id.clone(),
            ts: now_ms(),
            schema_version: SCHEMA_VERSION,
            turn_id,
            action_id,
            seq: None,
            payload: event.payload()?,
        };
        match event.class() {
            Class::Durable => {
                line.seq = Some(Seq(inner.next));
                let mut bytes = serde_json::to_vec(&line)?;
                bytes.push(b'\n');
                // One line, one write, so a reader never sees half of one
                // unless the process dies mid-write (`docs/state.md`).
                let path = inner.dir.join(EVENTS);
                inner.events.write_all(&bytes).map_err(io_at(&path))?;
                inner.next += 1;
                if fsyncs(event) {
                    inner.sync()?;
                }
            }
            Class::Ephemeral => {}
        }
        inner.watchers.retain(|w| match w.upgrade() {
            Some(queue) => {
                queue.push(&line);
                true
            }
            None => false,
        });
        Ok(line)
    }

    /// A watcher that receives every event appended from now on.
    pub fn watch(&self) -> Watcher {
        let mut inner = self.lock();
        let queue = Arc::new(Queue::default());
        inner.watchers.push(Arc::downgrade(&queue));
        Watcher::new(queue, inner.dir.clone(), inner.next)
    }

    /// How many fsyncs this log has made, counted where they are made
    /// (`docs/performance.md`, "Measuring").
    pub fn fsyncs(&self) -> u64 {
        self.lock().fsyncs
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl From<Inner> for Log {
    fn from(inner: Inner) -> Self {
        Self {
            inner: Mutex::new(inner),
        }
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        for queue in self.lock().watchers.iter().filter_map(Weak::upgrade) {
            queue.close();
        }
    }
}

impl Inner {
    fn new(session_id: SessionId, dir: PathBuf, events: File, lock: File, next: u64) -> Self {
        Self {
            session_id,
            dir,
            events,
            _lock: lock,
            next,
            fsyncs: 0,
            watchers: Vec::new(),
        }
    }

    /// Makes every line written so far durable.
    fn sync(&mut self) -> Result<(), Error> {
        self.fsyncs += 1;
        self.events
            .sync_data()
            .map_err(io_at(&self.dir.join(EVENTS)))
    }

    /// Makes the entries in the directory `dir` durable.
    fn sync_dir(&mut self, dir: &Path) -> Result<(), Error> {
        self.fsyncs += 1;
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(io_at(dir))
    }
}

/// Whether appending `event` ends with an fsync (`docs/events.md`,
/// "Writing"): the record of a side effect once it has happened, and the two
/// records written before their effect, `assistant_message_started` before
/// the model request and `tool_call_started` before the tool runs. Every
/// other durable line is made durable by the next of these.
fn fsyncs(event: &Event) -> bool {
    matches!(
        event,
        Event::AssistantMessageStarted(_)
            | Event::AssistantMessageCompleted(_)
            | Event::ToolCallStarted(_)
            | Event::ToolCallCompleted(_)
    )
}

/// Takes the session's lock and writes the holder's name into it, or names
/// who holds it.
fn lock(dir: &Path, id: &SessionId) -> Result<File, Error> {
    let path = dir.join(LOCK);
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(io_at(&path))?;
    match file.try_lock() {
        Ok(()) => {
            file.set_len(0).map_err(io_at(&path))?;
            writeln!(file, "{}", std::process::id()).map_err(io_at(&path))?;
            Ok(file)
        }
        Err(TryLockError::WouldBlock) => {
            // The holder writes its pid just after taking the lock, so a
            // refusal in between cannot name it.
            let pid = fs::read_to_string(&path).unwrap_or_default();
            let holder = match pid.trim() {
                "" => "another process".to_owned(),
                pid => format!("process {pid}"),
            };
            Err(Error::Held {
                session: id.0.clone(),
                holder,
            })
        }
        Err(TryLockError::Error(e)) => Err(io_at(&path)(e)),
    }
}

/// Milliseconds since the epoch, for `ts`.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
