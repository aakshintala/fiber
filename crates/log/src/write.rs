//! The writing side: one writer per session, which mints `seq`, appends
//! durable lines, fsyncs them in the order `docs/events.md`, "Writing", sets,
//! and fans every event out to watchers.

use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use contract::clock::{Clock, wall_ms};
use contract::emit::Emit;
use contract::events::{Class, Event};
use contract::tool::Bound;
use contract::{ActionId, Envelope, SCHEMA_VERSION, Seq, SessionId, TurnId};

use crate::offsets::Offsets;
use crate::rate::{Rate, RateFold};
use crate::read::{CAPACITY, Queue, Watcher};
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
    clock: Arc<dyn Clock>,
    /// The session directory, copied out of `inner` so [`Log::dir`] can lend it.
    dir: PathBuf,
}

struct Inner {
    session_id: SessionId,
    dir: PathBuf,
    events: File,
    /// Holds the session's lock for as long as it is open.
    _lock: Lock,
    /// The `seq` the next durable line gets.
    next: u64,
    /// Where each durable line starts: as many lines as `next` counts.
    offsets: Arc<Offsets>,
    fsyncs: u64,
    watchers: Vec<Weak<Queue>>,
    /// The newest line of each latest-wins kind. A subscriber reads it after
    /// registering, so a lagging connection cannot hide it.
    latest: BTreeMap<String, Envelope>,
    /// The bytes-to-tokens rate folded from every line so far.
    rate: RateFold,
    /// Why the log stopped, once a write or fsync failed.
    failed: Option<String>,
}

impl Log {
    /// Creates the session directory `id` in `sessions` (see
    /// [`crate::sessions_dir`]; an absolute path), holding `events.jsonl`,
    /// `session.lock` and `artifacts/` and nothing else, and takes its lock.
    /// Refuses a session that already exists. `clock` stamps `ts` and spaces
    /// the retries that name a holder who has not yet written a pid.
    pub fn create(sessions: &Path, id: SessionId, clock: Arc<dyn Clock>) -> Result<Self, Error> {
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
        let lock = lock(&dir, &id, clock.as_ref())?;
        let path = dir.join(EVENTS);
        let events = OpenOptions::new()
            .append(true)
            .create_new(true)
            .open(&path)
            .map_err(io_at(&path))?;
        sync_dir(&dir, &mut fsyncs)?;
        let offsets = Offsets::new(path, Vec::new(), 0);
        let mut inner = Inner::new(id, dir, events, lock, 0, offsets);
        inner.fsyncs = fsyncs;
        Ok(Self::from_parts(inner, clock))
    }

    /// Opens the existing session `id` in `sessions` for writing, taking its
    /// lock. A torn tail left by a crash or a failed write is truncated
    /// first, and `seq` carries on from the last complete line. `clock` is
    /// the same one [`Log::create`] takes.
    pub fn open(sessions: &Path, id: SessionId, clock: Arc<dyn Clock>) -> Result<Self, Error> {
        let dir = session_path(sessions, &id);
        let path = dir.join(EVENTS);
        if !path.is_file() {
            return Err(Error::NotFound(dir));
        }
        let lock = lock(&dir, &id, clock.as_ref())?;
        let events = OpenOptions::new()
            .read(true)
            .append(true)
            .open(&path)
            .map_err(io_at(&path))?;
        // One pass over the complete lines, one line held at a time: where
        // each starts, the latest-wins kinds, and where a torn tail begins.
        let mut lines = crate::read::lines(&dir)?;
        let mut starts = Vec::new();
        let mut latest = BTreeMap::new();
        let mut rate = RateFold::default();
        let mut next = 0;
        loop {
            let start = lines.offset();
            let Some(line) = lines.next() else {
                break;
            };
            let line = line?;
            starts.push(start);
            next = line.seq.map_or(0, |s| s.0 + 1);
            keep_latest(&mut latest, &line);
            rate.fold(&line);
        }
        let end = lines.offset();
        // A no-op unless the tail is torn.
        events.set_len(end).map_err(io_at(&path))?;
        let offsets = Offsets::new(path, starts, end);
        let mut inner = Inner::new(id, dir, events, lock, next, offsets);
        inner.latest = latest;
        inner.rate = rate;
        Ok(Self::from_parts(inner, clock))
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
            ts: wall_ms(self.clock.wall()),
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
        keep_latest(&mut inner.latest, &line);
        inner.rate.fold(&line);
        inner.watchers.retain(|w| match w.upgrade() {
            Some(queue) => {
                queue.push(&line);
                true
            }
            None => false,
        });
        Ok(line)
    }

    /// The newest line of a latest-wins kind (`session_status`,
    /// `extensions_loaded`, `steering_queue` or `clients`), recorded as it
    /// was appended. Nothing for any
    /// other kind, and nothing before that kind has been written. A
    /// subscriber registers its watcher first and then reads this, so a line
    /// written in between is queued and may also be here; the latest wins.
    pub fn latest(&self, kind: &str) -> Option<Envelope> {
        self.lock().latest.get(kind).cloned()
    }

    /// The bytes-to-tokens rate folded from every line so far.
    pub fn rate(&self) -> Rate {
        self.lock().rate.rate()
    }

    /// How many durable lines the log holds: the `seq` the next one gets.
    pub fn count(&self) -> u64 {
        self.lock().offsets.count()
    }

    /// The durable lines whose `seq` is `from..from + max`, in order, fewer
    /// when the log ends first, none when `from` is past the last line.
    /// Reads and parses only those lines (`docs/events.md`, "Resume"): a
    /// line outside the window that does not parse is never seen.
    pub fn range(&self, from: u64, max: usize) -> Result<Vec<Envelope>, Error> {
        let offsets = Arc::clone(&self.lock().offsets);
        offsets.range(from, max)
    }

    /// A watcher that receives every event appended from now on. On a log
    /// stopped by a failed write, one that returns the failure at once.
    pub fn watch(&self) -> Watcher {
        let armed = self.arm(false);
        Watcher::new(armed.queue, armed.offsets, armed.next)
    }

    /// A watcher that receives every durable line from `seq` 0, then
    /// everything written after it was registered. The queue is registered
    /// before the log is read, so a line written between the two is queued
    /// and also read; [`Watcher`] returns it once. The first page of lines
    /// is read now, so a first page that does not parse refuses the watch;
    /// later pages are read as the watcher reaches them.
    pub fn watch_all(&self) -> Result<Watcher, Error> {
        let armed = self.arm(true);
        self.finish(armed)
    }

    /// A watcher like [`Log::watch_all`], with the kept ephemeral lines
    /// seeded first: the kept `session_status`, `steering_queue` and
    /// `extension_ui` lines, in that order with `extension_ui` by key. The
    /// watcher is registered and its seed queued under the one log lock
    /// that `append` takes, so no later line can be queued before an older
    /// snapshot.
    pub fn watch_all_seeded(&self) -> Result<Watcher, Error> {
        let armed = {
            let mut inner = self.lock();
            let queue = Arc::new(Queue::default());
            if let Some(cause) = &inner.failed {
                queue.fail(&inner.session_id.0, cause);
            }
            inner.watchers.retain(|w| w.strong_count() > 0);
            inner.watchers.push(Arc::downgrade(&queue));
            let mut seeds: Vec<Envelope> = Vec::new();
            if let Some(line) = inner.latest.get("session_status") {
                seeds.push(line.clone());
            }
            if let Some(line) = inner.latest.get("steering_queue") {
                seeds.push(line.clone());
            }
            let mut ui_keys: Vec<&String> = inner
                .latest
                .keys()
                .filter(|k| k.starts_with("extension_ui:"))
                .collect();
            ui_keys.sort();
            for key in ui_keys {
                if let Some(line) = inner.latest.get(key) {
                    seeds.push(line.clone());
                }
            }
            for line in &seeds {
                queue.push_kept(line);
            }
            Armed {
                queue,
                offsets: Arc::clone(&inner.offsets),
                next: 0,
            }
        };
        self.finish(armed)
    }

    /// Registers a queue. `from_start` is [`Log::watch_all`]: the watcher
    /// begins at `seq` 0. [`Log::watch`] begins at the next line.
    fn arm(&self, from_start: bool) -> Armed {
        let mut inner = self.lock();
        let queue = Arc::new(Queue::default());
        if let Some(cause) = &inner.failed {
            queue.fail(&inner.session_id.0, cause);
        }
        // Forget watchers already dropped, so attaching and leaving while
        // idle keeps nothing.
        inner.watchers.retain(|w| w.strong_count() > 0);
        inner.watchers.push(Arc::downgrade(&queue));
        Armed {
            queue,
            offsets: Arc::clone(&inner.offsets),
            next: if from_start { 0 } else { inner.next },
        }
    }

    /// Reads the log's first page into the watcher `armed` registered.
    fn finish(&self, armed: Armed) -> Result<Watcher, Error> {
        let first = armed.offsets.range(0, CAPACITY)?;
        Ok(Watcher::starting(armed.queue, armed.offsets, first))
    }

    /// `full` cut to `bound`, with a notice of how many bytes were cut and
    /// where the whole text is, and the artifact's path relative to the
    /// session directory. `name` is the artifact's file name. Doors cuts a
    /// driver `shell` the same way the loop cuts a tool call, and doors
    /// cannot depend on the loop (`docs/architecture.md`, "The call rules").
    pub fn cut_output(&self, full: &str, bound: Bound, name: &str) -> (String, Option<String>) {
        let head = full.floor_char_boundary(bound.start);
        let tail = full.ceil_char_boundary(full.len().saturating_sub(bound.end).max(head));
        let removed = tail.saturating_sub(head);
        let (notice, artifact) = match self.write_artifact(name, full.as_bytes()) {
            Ok((relative, path)) => (
                format!(
                    "[{removed} bytes cut. The full output is in {}; read it with `read`.]",
                    path.display()
                ),
                Some(relative),
            ),
            Err(e) => (
                format!("[{removed} bytes cut. The full output could not be saved: {e}.]"),
                None,
            ),
        };
        let mut kept = full.get(..head).unwrap_or_default().to_owned();
        kept.push('\n');
        kept.push_str(&notice);
        if tail < full.len() {
            kept.push('\n');
            kept.push_str(full.get(tail..).unwrap_or_default());
        }
        (kept, artifact)
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

    /// The session directory: what an `image` part's path is relative to
    /// (`docs/events.md`, "Conventions").
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// How many fsyncs this log has made, counted where they are made
    /// (`docs/performance.md`, "Measuring").
    pub fn fsyncs(&self) -> u64 {
        self.lock().fsyncs
    }

    /// The clock this log was built with, stamping `ts` (`docs/tools.md`,
    /// "Progress"): the loop paces a running call's `tool_call_delta`
    /// lines on it.
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Log {
    fn from_parts(inner: Inner, clock: Arc<dyn Clock>) -> Self {
        Self {
            dir: inner.dir.clone(),
            inner: Mutex::new(inner),
            clock,
        }
    }
}

impl Emit for Log {
    fn emit(&self, event: &Event) {
        if event.class() != Class::Ephemeral {
            return;
        }
        // A poisoned log has already stopped. The line is ephemeral, so
        // dropping it loses nothing the log was keeping.
        match self.append(event, None, None) {
            Ok(_) | Err(_) => {}
        }
    }
}

/// A queue registered on a log, before [`Log::watch_all`] reads the file.
struct Armed {
    queue: Arc<Queue>,
    offsets: Arc<Offsets>,
    next: u64,
}

impl Drop for Log {
    fn drop(&mut self) {
        for queue in self.lock().watchers.iter().filter_map(Weak::upgrade) {
            queue.close();
        }
    }
}

impl Inner {
    fn new(
        session_id: SessionId,
        dir: PathBuf,
        events: File,
        lock: Lock,
        next: u64,
        offsets: Offsets,
    ) -> Self {
        Self {
            session_id,
            dir,
            events,
            _lock: lock,
            next,
            offsets: Arc::new(offsets),
            fsyncs: 0,
            watchers: Vec::new(),
            latest: BTreeMap::new(),
            rate: RateFold::default(),
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
        self.offsets
            .push(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
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

/// Keeps `line` in `latest` when its kind is one whose latest wins
/// (`session_status`, `extensions_loaded`, `steering_queue`, `clients`, and
/// `extension_ui`). `extension_ui` is kept per extension and per widget id:
/// one key for the status line and one per widget; a clearing line (`status`
/// `""`, or empty `lines`) removes its key.
fn keep_latest(latest: &mut BTreeMap<String, Envelope>, line: &Envelope) {
    if matches!(
        line.kind.as_str(),
        "session_status" | "extensions_loaded" | "steering_queue" | "clients"
    ) {
        latest.insert(line.kind.clone(), line.clone());
        return;
    }
    if line.kind.as_str() == "extension_ui" {
        let ui: Result<contract::events::ExtensionUi, _> =
            serde_json::from_value(serde_json::Value::Object(line.payload.clone()));
        let Ok(ui) = ui else { return };
        match &ui.ui {
            contract::events::Ui::Status { status } => {
                let key = format!("extension_ui:{}:status", ui.extension);
                if status.is_empty() {
                    latest.remove(&key);
                } else {
                    latest.insert(key, line.clone());
                }
            }
            contract::events::Ui::Widget { widget, lines } => {
                let key = format!("extension_ui:{}:widget:{}", ui.extension, widget);
                if lines.is_empty() {
                    latest.remove(&key);
                } else {
                    latest.insert(key, line.clone());
                }
            }
        }
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
        | Event::TextCompleted(_)
        | Event::ToolCallRequested(_)
        | Event::PermissionRequested(_)
        | Event::PermissionResolved(_)
        | Event::InteractionRequested(_)
        | Event::InteractionResolved(_)
        | Event::RepositoryCodeOffered(_)
        | Event::RepositoryCodeResolved(_)
        | Event::QuotaNoticed(_)
        | Event::PreambleBuilt(_)
        | Event::ModelChanged(_)
        | Event::OpeningMessage(_)
        | Event::InstructionFile(_)
        | Event::DateChanged(_)
        | Event::SkillsChanged(_)
        | Event::SkillsResent(_)
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
        | Event::ExtensionLog(_)
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
fn lock(dir: &Path, id: &SessionId, clock: &dyn Clock) -> Result<Lock, Error> {
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
            holder: holder(&path, clock),
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
fn holder(path: &Path, clock: &dyn Clock) -> String {
    for _ in 0..50 {
        let pid = fs::read_to_string(path).unwrap_or_default();
        if !pid.trim().is_empty() {
            return format!("process {}", pid.trim());
        }
        clock.sleep(Duration::from_millis(1));
    }
    "a process whose pid is not yet recorded".to_owned()
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
