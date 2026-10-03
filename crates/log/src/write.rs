//! The writing side: one writer per session, which mints `seq`, appends
//! durable lines, fsyncs them in the order `docs/events.md`, "Writing", sets,
//! and fans every event out to watchers.

use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use contract::events::{Class, Event};
use contract::{ActionId, Envelope, SCHEMA_VERSION, Seq, SessionId, TurnId};

use crate::read::{Queue, Watcher, complete_len};
use crate::{ARTIFACTS, EVENTS, Error, LOCK, io_at, session_path};

/// A session's log, open for writing. Only one exists per session at a time,
/// across every process: the lock on `session.lock` is held until it is
/// dropped. It is shared by whoever emits events, behind one lock
/// (`docs/architecture.md`, "The threads").
///
/// A failed write or fsync stops it: every later append returns
/// [`Error::Poisoned`], and every watcher ends with it. Reopening the session
/// truncates whatever part of a line the failure left.
pub struct Log {
    inner: Mutex<Inner>,
}

struct Inner {
    session_id: SessionId,
    dir: PathBuf,
    events: File,
    /// Holds the session's lock for as long as it is open.
    _lock: Lock,
    /// The `seq` the next durable line gets.
    next: u64,
    fsyncs: u64,
    watchers: Vec<Weak<Queue>>,
    /// Why the log stopped, once a write or fsync failed.
    failed: Option<String>,
}

impl Log {
    /// Creates the session directory `id` in `sessions` (see
    /// [`crate::sessions_dir`]; an absolute path), holding `events.jsonl`,
    /// `session.lock` and `artifacts/` and nothing else, and takes its lock.
    /// Refuses a session that already exists.
    pub fn create(sessions: &Path, id: SessionId) -> Result<Self, Error> {
        let dir = session_path(sessions, &id);
        let mut fsyncs = 0;
        // Each new directory's entry is fsynced in its parent, so a crash
        // cannot lose a directory that a later fsynced line lives in.
        let missing: Vec<&Path> = sessions.ancestors().take_while(|d| !d.exists()).collect();
        for new in missing.iter().rev() {
            make_dir(new, true, &mut fsyncs)?;
        }
        make_dir(&dir, false, &mut fsyncs)?;
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
        sync_dir(&dir, &mut fsyncs)?;
        let mut inner = Inner::new(id, dir, events, lock, 0);
        inner.fsyncs = fsyncs;
        Ok(Self::from(inner))
    }

    /// Opens the existing session `id` in `sessions` for writing, taking its
    /// lock. A torn tail left by a crash or a failed write is truncated
    /// first, and `seq` carries on from the last complete line.
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
        // debt: reads the whole log to find its tail; resume folds the whole
        // log anyway (docs/events.md, "Resume"). Seek to the tail once resume
        // reads a range by seq.
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
        if let Some(cause) = &inner.failed {
            return Err(Error::Poisoned {
                session: inner.session_id.0.clone(),
                cause: cause.clone(),
            });
        }
        let sync = fsyncs(event, action_id.is_some());
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
                if let Err(e) = inner.write(&bytes, sync) {
                    inner.stop(&e);
                    return Err(e);
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

    /// A watcher that receives every event appended from now on. On a log
    /// stopped by a failed write, one that returns the failure at once.
    pub fn watch(&self) -> Watcher {
        let mut inner = self.lock();
        let queue = Arc::new(Queue::default());
        if let Some(cause) = &inner.failed {
            queue.fail(&inner.session_id.0, cause);
        }
        // Forget watchers already dropped, so attaching and leaving while
        // idle keeps nothing.
        inner.watchers.retain(|w| w.strong_count() > 0);
        inner.watchers.push(Arc::downgrade(&queue));
        Watcher::new(queue, inner.dir.clone(), inner.next)
    }

    /// Writes `bytes` to the file `name` in the session's `artifacts/`,
    /// replacing any file of that name, and returns its path relative to the
    /// session directory (`docs/events.md`, "Conventions") and its absolute
    /// path. A name that is not one plain file name is refused.
    pub fn write_artifact(&self, name: &str, bytes: &[u8]) -> Result<(String, PathBuf), Error> {
        let dir = self.lock().dir.join(ARTIFACTS);
        let path = dir.join(name);
        if Path::new(name).file_name() != Some(name.as_ref()) {
            return Err(io_at(&path)(io::Error::new(
                io::ErrorKind::InvalidInput,
                "an artifact's name is one plain file name",
            )));
        }
        fs::write(&path, bytes).map_err(io_at(&path))?;
        Ok((format!("{ARTIFACTS}/{name}"), path))
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
    fn new(session_id: SessionId, dir: PathBuf, events: File, lock: Lock, next: u64) -> Self {
        Self {
            session_id,
            dir,
            events,
            _lock: lock,
            next,
            fsyncs: 0,
            watchers: Vec::new(),
            failed: None,
        }
    }

    /// Appends one line in one write, and fsyncs it when `sync` says. A write
    /// that stops short is a failure, not retried: the part written is
    /// already a torn tail, which reopening truncates.
    fn write(&mut self, bytes: &[u8], sync: bool) -> Result<(), Error> {
        let path = self.dir.join(EVENTS);
        let written = self.events.write(bytes).map_err(io_at(&path))?;
        if written < bytes.len() {
            let short = format!("wrote {written} of {} bytes", bytes.len());
            return Err(io_at(&path)(io::Error::new(
                io::ErrorKind::WriteZero,
                short,
            )));
        }
        self.next += 1;
        if sync {
            self.fsyncs += 1;
            self.events.sync_data().map_err(io_at(&path))?;
        }
        Ok(())
    }

    /// Stops the log after `error`, and ends every watcher with it.
    fn stop(&mut self, error: &Error) {
        let cause = error.to_string();
        for queue in self.watchers.drain(..).filter_map(|w| w.upgrade()) {
            queue.fail(&self.session_id.0, &cause);
        }
        self.failed = Some(cause);
    }
}

/// Makes the directory `dir`, private to the person (`docs/state.md`), and
/// fsyncs its entry in its parent. `exists_ok` lets another process have
/// made it first.
fn make_dir(dir: &Path, exists_ok: bool, fsyncs: &mut u64) -> Result<(), Error> {
    DirBuilder::new()
        .recursive(exists_ok)
        .mode(0o700)
        .create(dir)
        .map_err(io_at(dir))?;
    sync_dir(dir.parent().unwrap_or(dir), fsyncs)
}

/// Makes the entries in the directory `dir` durable, counting the fsync.
fn sync_dir(dir: &Path, fsyncs: &mut u64) -> Result<(), Error> {
    *fsyncs += 1;
    File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(io_at(dir))
}

/// Whether appending `event` ends with an fsync. `docs/events.md`,
/// "Writing": "Fsync the record of a side effect after it happens and before
/// causing the next one", except `assistant_message_started` and
/// `tool_call_started`, fsynced before their effect. A side effect costs
/// money or touches the world: a model call, a tool call, or a process run
/// outside one. Every other durable line is made durable by the next fsync.
/// Every kind is listed, so a new one does not compile until it is placed.
fn fsyncs(event: &Event, in_action: bool) -> bool {
    match event {
        // Before the model request is sent, and before the tool runs.
        Event::AssistantMessageStarted(_) | Event::ToolCallStarted(_) => true,
        // A model call and a tool call, once they have happened; a command
        // the person ran, and a program an extension ran.
        Event::AssistantMessageCompleted(_)
        | Event::ToolCallCompleted(_)
        | Event::ShellCommand(_)
        | Event::ExtensionExec(_) => true,
        // A reviewer's or an extension's model call belongs to no action, and
        // no other line records it. The conversation's own call is recorded
        // by `assistant_message_completed`.
        Event::UsageRecorded(_) => !in_action,
        // A job and a delegate start inside a tool call, whose two fsyncs
        // bracket the start, and their news reaches the model at a step
        // boundary, ahead of a model request's fsync. The rest record no
        // effect.
        Event::FiberStarted(_)
        | Event::FiberExited(_)
        | Event::SessionStarted(_)
        | Event::Rewound(_)
        | Event::TurnStarted(_)
        | Event::StepStarted(_)
        | Event::TurnCompleted(_)
        | Event::SteeringApplied(_)
        | Event::SessionNamed(_)
        | Event::ContextAdded(_)
        | Event::ReasoningStarted(_)
        | Event::ReasoningCompleted(_)
        | Event::ToolCallRequested(_)
        | Event::PermissionRequested(_)
        | Event::PermissionResolved(_)
        | Event::InteractionRequested(_)
        | Event::InteractionResolved(_)
        | Event::QuotaNoticed(_)
        | Event::PreambleBuilt(_)
        | Event::ModelChanged(_)
        | Event::OpeningMessage(_)
        | Event::InstructionFile(_)
        | Event::DateChanged(_)
        | Event::HandoffStarted(_)
        | Event::HandoffCompleted(_)
        | Event::ContextNudged(_)
        | Event::McpServerFailed(_)
        | Event::McpServerReady(_)
        | Event::Reloaded(_)
        | Event::ExtensionsLoaded(_)
        | Event::ExtensionStateSet(_)
        | Event::ExtensionStateUnset(_)
        | Event::JobStarted(_)
        | Event::DelegateStarted(_)
        | Event::JobLine(_)
        | Event::DelegateFinished(_)
        | Event::JobCompleted(_)
        | Event::JobsPendingNotified(_) => false,
        // Ephemeral: never written.
        Event::SteeringQueue(_)
        | Event::Clients(_)
        | Event::SessionStatus(_)
        | Event::AssistantMessageDelta(_)
        | Event::ToolCallArgumentsDelta(_)
        | Event::ReasoningDelta(_)
        | Event::ToolCallDelta(_)
        | Event::RetryScheduled(_)
        | Event::Notice(_)
        | Event::ExtensionUi(_)
        | Event::ExtensionMessage(_)
        | Event::JobDelta(_)
        | Event::CommandAccepted(_)
        | Event::CommandRejected(_) => false,
    }
}

/// The session's lock, held while the file is open. Letting go clears the
/// holder's pid, so a later refusal never names a process that let go.
struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        // Best effort: the lock itself is released when the file closes.
        self.0.set_len(0).unwrap_or(());
    }
}

/// Takes the session's lock and writes the holder's pid into it, or names
/// who holds it.
fn lock(dir: &Path, id: &SessionId) -> Result<Lock, Error> {
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
            file.write_all(format!("{}\n", std::process::id()).as_bytes())
                .map_err(io_at(&path))?;
            Ok(Lock(file))
        }
        Err(TryLockError::WouldBlock) => Err(Error::Held {
            session: id.0.clone(),
            holder: holder(&path),
        }),
        Err(TryLockError::Error(e)) => Err(io_at(&path)(e)),
    }
}

// debt: a holder that crashed leaves its pid, and a new holder that has
// not yet replaced it is misnamed for those microseconds; a liveness check on
// the pid if that ever matters.
/// Names the holder of the lock at `path`. A holder writes its pid just
/// after taking the lock, so the file may still be empty; it is read again
/// for up to 50 ms before giving up.
fn holder(path: &Path) -> String {
    for _ in 0..50 {
        let pid = fs::read_to_string(path).unwrap_or_default();
        if !pid.trim().is_empty() {
            return format!("process {}", pid.trim());
        }
        thread::sleep(Duration::from_millis(1));
    }
    "a process whose pid is not yet recorded".to_owned()
}

/// Milliseconds since the epoch, for `ts`.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
