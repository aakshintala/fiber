//! The writing side: one writer per session, which mints `seq`, appends
//! durable lines, fsyncs them in the order `docs/events.md`, "Writing", sets,
//! and fans every event out to watchers.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fs::{self, DirBuilder, File, OpenOptions};
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
use crate::read::{Queue, Watcher};
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
    offsets: Arc<Offsets>,
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
        // One scan over the complete lines: the offset table, the
        // latest-wins kinds and the rate fold, and where a torn tail
        // begins. Each line's `kind` and `seq` are read first; only the
        // kinds a fold reads are fully parsed. A refusal names its line
        // and leaves the file as it was: nothing is truncated until the
        // scan ends.
        let mut latest = BTreeMap::new();
        let mut rate = RateFold::default();
        let mut next = 0;
        let offsets = Offsets::scan(&dir, u64::MAX, |bytes| {
            let text = std::str::from_utf8(bytes)
                .map_err(|_| serde::de::Error::custom("line is not UTF-8"))?;
            let head: Head = serde_json::from_str(text)?;
            next = head
                .seq
                .checked_add(1)
                .ok_or_else(|| serde::de::Error::custom("seq leaves no next seq"))?;
            if folded(&head.kind) {
                let line = envelope(bytes)?;
                keep_latest(&mut latest, &line);
                rate.fold(&line);
            }
            Ok(())
        })?;
        let end = offsets.end();
        // A no-op unless the tail is torn.
        events.set_len(end).map_err(io_at(&path))?;
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
        self.offsets.count()
    }

    /// The durable lines whose `seq` is `from..from + max`, in order, fewer
    /// when the log ends first, none when `from` is past the last line.
    /// Reads and parses only those lines (`docs/events.md`, "Resume"): a
    /// line outside the window that does not parse is never seen.
    pub fn range(&self, from: u64, max: usize) -> Result<Vec<Envelope>, Error> {
        self.offsets.range(from, max)
    }

    /// A watcher that receives every event appended from now on. On a log
    /// stopped by a failed write, one that returns the failure at once.
    pub fn watch(&self) -> Watcher {
        let armed = self.arm(false, |_, _| {});
        Watcher::new(armed.queue, armed.offsets, armed.next)
    }

    /// A watcher that receives every durable line from `seq` 0, then
    /// everything written after it was registered. The queue is registered
    /// before the log is read, so a line written between the two is queued
    /// and also read; [`Watcher`] returns it once. A first page that does
    /// not parse keeps every line before the failure: the watcher returns
    /// them, then the failure once, then nothing; later pages are read as
    /// the watcher reaches them.
    pub fn watch_all(&self) -> Watcher {
        let armed = self.arm(true, |_, _| {});
        self.finish(armed)
    }

    /// A watcher like [`Log::watch_all`], with the kept ephemeral lines
    /// seeded first: the kept `session_status`, `steering_queue` and
    /// `extension_ui` lines, in that order with `extension_ui` by key. The
    /// watcher is registered and its seed queued under the one log lock
    /// that `append` takes, so no later line can be queued before an older
    /// snapshot.
    pub fn watch_all_seeded(&self) -> Watcher {
        let armed = self.arm(true, |inner, queue| {
            for line in inner.kept_seed() {
                queue.push_kept(line);
            }
        });
        self.finish(armed)
    }

    /// Registers a queue and runs `seed` under the one log lock that
    /// `append` takes, after the queue is registered (and after `fail` on
    /// a stopped log), so no later line can be queued before an older
    /// snapshot. `from_start` is [`Log::watch_all`]: the watcher begins at
    /// `seq` 0. [`Log::watch`] begins at the next line.
    fn arm(&self, from_start: bool, seed: impl FnOnce(&Inner, &Queue)) -> Armed {
        let mut inner = self.lock();
        let queue = Arc::new(Queue::default());
        if let Some(cause) = &inner.failed {
            queue.fail(&inner.session_id.0, cause);
        }
        // Forget watchers already dropped, so attaching and leaving while
        // idle keeps nothing.
        inner.watchers.retain(|w| w.strong_count() > 0);
        inner.watchers.push(Arc::downgrade(&queue));
        seed(&inner, &queue);
        Armed {
            queue,
            offsets: Arc::clone(&inner.offsets),
            next: if from_start { 0 } else { inner.next },
        }
    }

    /// Reads the log's first page into the watcher `armed` registered. The
    /// table's line count is taken first and bounds every backlog page, so
    /// a line appended after it arrives through the watcher's queue, behind
    /// the kept lines already in it, and is deduplicated by `seq` when a
    /// page also holds it.
    fn finish(&self, armed: Armed) -> Watcher {
        let end = armed.offsets.count();
        let first = armed.offsets.page(0, end);
        Watcher::starting(armed.queue, armed.offsets, first, end)
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
            offsets: Arc::clone(&inner.offsets),
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
            #[cfg(test)]
            BEFORE_SYNC.with(|cell| {
                if let Some(hook) = cell.borrow().as_ref() {
                    hook();
                }
            });
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

    /// The kept ephemeral lines, seeded first on a full subscriber: the
    /// kept `session_status`, then `steering_queue`, then every
    /// `extension_ui:` key in `BTreeMap` order (already sorted, so no
    /// explicit sort).
    fn kept_seed(&self) -> impl Iterator<Item = &Envelope> {
        let status = self.latest.get("session_status");
        let steering = self.latest.get("steering_queue");
        let ui = self
            .latest
            .iter()
            .filter(|(key, _)| key.starts_with("extension_ui:"))
            .map(|(_, line)| line);
        status.into_iter().chain(steering).chain(ui)
    }
}

/// The two fields `Log::open` reads from every line: what decides
/// whether the line is fully parsed. Every other field is skipped, but
/// its JSON syntax is still checked. `kind` borrows the line's bytes and
/// allocates only for an escaped kind; `seq` is required.
#[derive(serde::Deserialize)]
struct Head<'a> {
    #[serde(borrow)]
    kind: Cow<'a, str>,
    seq: u64,
}

/// Whether `keep_latest` keeps a line of this kind: the latest-wins kinds.
/// `Log::open` fully parses only these and the kinds the rate fold reads.
fn latest_wins(kind: &str) -> bool {
    matches!(
        kind,
        "session_status" | "extensions_loaded" | "steering_queue" | "clients" | "extension_ui"
    )
}

/// Whether `Log::open` fully parses a line of this kind: the kinds a fold
/// reads.
fn folded(kind: &str) -> bool {
    latest_wins(kind) || RateFold::reads(kind)
}

/// Fully parses one line open folds. The one full-parse helper on the open
/// path, so the counted-work test counts through it.
fn envelope(bytes: &[u8]) -> Result<Envelope, serde_json::Error> {
    #[cfg(test)]
    FULL_PARSES.with(|count| count.set(count.get().saturating_add(1)));
    serde_json::from_slice(bytes)
}

// Counts the full parses on the open path, on the calling thread: the full
// parse happens inside `Log::open` and its result is folded and dropped
// there, so nothing a caller sees tells a full parse from a kind-and-seq
// read. Test-only; non-test builds never call it.
#[cfg(test)]
thread_local! {
    static FULL_PARSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many full parses the open path has made on the calling thread
/// (`docs/testing.md`, "a test of how much work something does counts the
/// work"). The counted-work test reads it before and after `Log::open`.
#[cfg(test)]
pub(crate) fn full_parses() -> usize {
    FULL_PARSES.with(|count| count.get())
}

/// Keeps `line` in `latest` when its kind is one whose latest wins
/// (`session_status`, `extensions_loaded`, `steering_queue`, `clients`, and
/// `extension_ui`). `extension_ui` is kept per extension and per widget id:
/// one key for the status line and one per widget; a clearing line (`status`
/// `""`, or empty `lines`) removes its key.
fn keep_latest(latest: &mut BTreeMap<String, Envelope>, line: &Envelope) {
    if latest_wins(line.kind.as_str()) && line.kind.as_str() != "extension_ui" {
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
        | Event::ReviewerKept(_)
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

// Pause point between publishing a line's offset and fsyncing it
// (`docs/testing.md`, "Waits and timeouts"): the reader-during-fsync test
// installs a hook to hold the appender there, so the race between the
// offset being visible and the fsync happens on every run instead of being
// waited for. Test-only; non-test builds never call it.
#[cfg(test)]
thread_local! {
    static BEFORE_SYNC: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        std::cell::RefCell::new(None);
}

/// Installs the pause-point hook run after a line's offset is published and
/// before its fsync (`docs/testing.md`, "Waits and timeouts"), on the
/// current thread. The reader-during-fsync test uses it to hold the appender
/// with the line visible, so the race happens on every run.
#[cfg(test)]
pub(crate) fn before_sync(hook: impl Fn() + 'static) {
    BEFORE_SYNC.with(|cell| *cell.borrow_mut() = Some(Box::new(hook)));
}

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
    match crate::scan::try_lock_file(dir)? {
        Some(mut file) => {
            file.set_len(0).map_err(io_at(&path))?;
            file.write_all(format!("{}\n", std::process::id()).as_bytes())
                .map_err(io_at(&path))?;
            Ok(Lock(file))
        }
        None => Err(Error::Held {
            session: id.0.clone(),
            holder: holder(&path, clock),
        }),
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
