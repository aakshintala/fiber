//! The diagnostic writer (`docs/state.md`, "Diagnostic logs"): one JSON
//! object per line in a process's own file in `logs/`.
//!
//! Each line is `ts`, `level`, `process`, `session_id` when one is known,
//! `code` and `message`, in that order; a `debug` line adds `data` last.
//! Nothing in `logs/` holds a credential or token, prompt or model text, a
//! tool's arguments or a configuration value. A write failure never stops
//! the process.

mod memory;

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use contract::SessionId;
use contract::clock::{Clock, wall_ms};
use contract::diag::{DebugLog, Op, ProviderRequest, Timing};
use support::lock;

pub use memory::peak_kib;

/// The process a diagnostic file belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Process {
    /// The hub: `logs/hub.log`.
    Hub,
    /// A session process: `logs/session-<id>.log`.
    Session,
    /// A `fiber ask` process: `logs/ask-<id>.log`.
    Ask,
    /// The terminal: `logs/tui-<pid>.log`.
    Tui,
}

impl Process {
    fn name(self) -> &'static str {
        match self {
            Self::Hub => "hub",
            Self::Session => "session",
            Self::Ask => "ask",
            Self::Tui => "tui",
        }
    }
}

/// How much a writer records (`docs/configuration.md`, `diagnostics.level`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// `error`, `warn` and `info` lines only.
    Info,
    /// Those, and the `debug` lines.
    Debug,
}

/// The level of a line that is not a debug line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A failure.
    Error,
    /// A failure the process carried on past.
    Warn,
    /// One of the process's operations.
    Info,
}

impl Severity {
    fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
        }
    }
}

/// The file a line goes to and the session it names when the caller names
/// none. They live only here, so a line that races [`Diag::attach`] is
/// written whole to one file with the id that file implies.
struct State {
    file: PathBuf,
    session: Option<SessionId>,
}

/// One diagnostic line in the field order `docs/state.md` lists: `ts`,
/// `level`, `process`, `session_id` when one is known, `code` and
/// `message`, and on a `debug` line `data` last.
#[derive(serde::Serialize)]
struct Line<'a, D: serde::Serialize> {
    ts: u64,
    level: &'a str,
    process: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    code: &'a str,
    message: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<D>,
}

/// The `data` of a `peak_memory` line.
#[derive(serde::Serialize)]
struct Peak {
    peak_kib: u64,
}

/// The `data` of an `op_timing` line: the operation, whole milliseconds,
/// and the tool's name on a tool's timers. No other field is recorded,
/// and never an argument, a path or any content.
#[derive(serde::Serialize)]
struct TimingData<'a> {
    op: Op,
    ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<&'a str>,
}

/// One lock across every diagnostic file in the process: a pair write
/// holds it across both appends, and every other write holds it across
/// its own, so lines from two writers never interleave a pair. Taken
/// before the writer's state lock, never after, so the order is one way.
static FILES: Mutex<()> = Mutex::new(());

/// One process's diagnostic writer. One lock serializes `attach`, rotation
/// and append, so concurrent callers never lose a line to two rotations or
/// split one across files; [`FILES`] serializes whole writes across
/// writers, so a pair from one writer is never split by another's line.
pub struct Diag {
    logs: PathBuf,
    process: Process,
    level: Level,
    clock: Arc<dyn Clock>,
    rotate_at: Option<u64>,
    peak: fn() -> Option<u64>,
    /// A hook run between [`Diag::peak_memory_then_info`]'s two appends,
    /// told whether the state lock is still held. Test-only: the pair
    /// must hold one lock across both appends, so a split-lock
    /// implementation reports an unlocked seam (`docs/testing.md`,
    /// "Races are forced, not waited for"). Always `None` outside
    /// tests, where the branch below never fires.
    between: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    state: Arc<Mutex<State>>,
}

impl Diag {
    /// A writer for `process` in `home`'s `logs/`. Nothing is created until
    /// the first line is written. Until [`Diag::attach`], a session, `ask`
    /// or `tui` process writes `<kind>-<pid>.log`; the hub writes `hub.log`.
    pub fn new(home: &Path, process: Process, level: Level, clock: Arc<dyn Clock>) -> Self {
        let logs = home.join("logs");
        let file = logs.join(match process {
            Process::Hub => "hub.log".to_owned(),
            Process::Session | Process::Ask | Process::Tui => {
                format!("{}-{}.log", process.name(), std::process::id())
            }
        });
        Self {
            logs,
            process,
            level,
            clock,
            rotate_at: None,
            peak: peak_kib,
            between: None,
            state: Arc::new(Mutex::new(State {
                file,
                session: None,
            })),
        }
    }

    /// Renames the file to `<file>.1` before a write once it is over `at`
    /// bytes, replacing any older one.
    #[must_use]
    pub fn rotating(mut self, at: u64) -> Self {
        self.rotate_at = Some(at);
        self
    }

    /// Sets the level, before the writer is shared.
    #[must_use]
    pub fn with_level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Reads the peak memory with `read` instead of [`peak_kib`].
    #[must_use]
    pub fn with_peak(mut self, read: fn() -> Option<u64>) -> Self {
        self.peak = read;
        self
    }

    /// Runs `between` between [`Diag::peak_memory_then_info`]'s two
    /// appends, telling it whether the state lock is still held, before
    /// the writer is shared. Test-only: see [`Diag`].
    #[doc(hidden)]
    #[must_use]
    pub fn with_between(mut self, between: Arc<dyn Fn(bool) + Send + Sync>) -> Self {
        self.between = Some(between);
        self
    }

    /// Names the session once its log exists: a session or `ask` process
    /// writes `<kind>-<id>.log` from here on, and every later line carries
    /// `session_id`. The hub's and the terminal's files do not change.
    pub fn attach(&self, session: &SessionId) {
        if !matches!(self.process, Process::Session | Process::Ask) {
            return;
        }
        let mut state = lock(&self.state);
        state.file = self
            .logs
            .join(format!("{}-{}.log", self.process.name(), session.0));
        state.session = Some(session.clone());
    }

    /// A second writer for `session` in the same `logs/` directory: the
    /// same process, level, clock and peak reader, writing
    /// `<kind>-<id>.log` with `session_id` from its first line. The
    /// original keeps its own file and stays unattached. A clone carries
    /// no rotation and no test hook.
    pub fn for_session(&self, session: &SessionId) -> Self {
        Self {
            logs: self.logs.clone(),
            process: self.process,
            level: self.level,
            clock: Arc::clone(&self.clock),
            rotate_at: None,
            peak: self.peak,
            between: None,
            state: Arc::new(Mutex::new(State {
                file: self
                    .logs
                    .join(format!("{}-{}.log", self.process.name(), session.0)),
                session: Some(session.clone()),
            })),
        }
    }

    /// Writes one `error`, `warn` or `info` line. `session` falls back to
    /// the attached session.
    pub fn line(&self, severity: Severity, session: Option<&SessionId>, code: &str, message: &str) {
        self.write(severity.name(), session, code, message, None::<()>);
    }

    /// Appends the `peak_memory` debug line for `kib` while the caller
    /// holds the state lock, so [`Diag::peak_memory_then_info`] writes its
    /// pair with nothing between them.
    fn peak_line(&self, state: &State, kib: u64) {
        self.append(
            state,
            "debug",
            None,
            "peak_memory",
            "The process's peak memory so far.",
            Some(Peak { peak_kib: kib }),
        );
    }

    /// Writes a `peak_memory` line at the debug level with the process's
    /// peak so far. A failed read writes nothing.
    pub fn peak_memory(&self) {
        if !self.debugging() {
            return;
        }
        if let Some(kib) = (self.peak)() {
            let _files = lock(&FILES);
            let state = lock(&self.state);
            self.peak_line(&state, kib);
        }
    }

    /// Appends the `peak_memory` debug line, when debugging and the read
    /// succeeds, immediately followed by the given `info` line, under one
    /// lock: no other thread's line can come between the pair.
    pub fn peak_memory_then_info(&self, code: &str, message: &str) {
        let _files = lock(&FILES);
        let state = lock(&self.state);
        if self.debugging()
            && let Some(kib) = (self.peak)()
        {
            self.peak_line(&state, kib);
        }
        // The same thread still owns `state`, so `try_lock` never
        // blocks: it reports `WouldBlock` exactly when the single lock
        // is correctly held across both appends.
        if let Some(between) = &self.between {
            between(self.state.try_lock().is_err());
        }
        self.append(&state, "info", None, code, message, None::<()>);
    }

    fn write<D: serde::Serialize>(
        &self,
        level: &str,
        session: Option<&SessionId>,
        code: &str,
        message: &str,
        data: Option<D>,
    ) {
        // The file and the attached id are read under the lock that also
        // covers rotation and append.
        let _files = lock(&FILES);
        let state = lock(&self.state);
        self.append(&state, level, session, code, message, data);
    }

    /// Serializes one line and appends it while the caller holds the state
    /// lock, so [`Diag::peak_memory_then_info`] can write its pair with
    /// nothing between them. A line that does not serialize writes nothing.
    fn append<D: serde::Serialize>(
        &self,
        state: &State,
        level: &str,
        session: Option<&SessionId>,
        code: &str,
        message: &str,
        data: Option<D>,
    ) {
        let session = session.or(state.session.as_ref());
        let line = Line {
            ts: wall_ms(self.clock.wall()),
            level,
            process: self.process.name(),
            session_id: session.map(|session| session.0.as_str()),
            code,
            message,
            data,
        };
        append_line(&state.file, &self.logs, self.rotate_at, &line);
    }

    /// The callable form of [`timed`]: one level check inside, none
    /// before it, so restricted crates time through the seam without
    /// depending on `log`.
    pub fn timings(&self) -> Timing {
        let level = self.level;
        let clock = Arc::clone(&self.clock);
        let logs = self.logs.clone();
        let process = self.process;
        let rotate_at = self.rotate_at;
        let state = Arc::clone(&self.state);
        Arc::new(move |op, tool, started| {
            start_timer(
                level, &clock, &logs, process, rotate_at, &state, op, tool, started,
            )
            .map(|timer| Box::new(timer) as Box<dyn Send>)
        })
    }
}

impl DebugLog for Diag {
    fn debugging(&self) -> bool {
        self.level == Level::Debug
    }

    fn provider_request(&self, request: &ProviderRequest) {
        if !self.debugging() {
            return;
        }
        self.write(
            "debug",
            None,
            "provider_request",
            "A provider request ended.",
            Some(request),
        );
    }
}

/// Serializes one line and appends it to `file`, rotating first when due.
/// The caller holds the files lock and the state lock. A line that does
/// not serialize, or a write that fails, writes nothing and stops nothing.
fn append_line<D: serde::Serialize>(
    file: &Path,
    logs: &Path,
    rotate_at: Option<u64>,
    line: &Line<'_, D>,
) {
    let Ok(mut bytes) = serde_json::to_vec(line) else {
        return;
    };
    bytes.push(b'\n');
    if let Some(at) = rotate_at {
        rotate(file, at);
    }
    append(file, logs, &bytes);
}

/// A started operation's guard. Dropping it writes one `op_timing` line
/// with the whole milliseconds from `start` to the stamped end, or to
/// the drop when never stopped: normal scope drop emits once, including
/// an early return or a propagated error after entry. A failed write is
/// silent and never changes the operation's result.
pub struct Timer {
    clock: Arc<dyn Clock>,
    logs: PathBuf,
    process: Process,
    rotate_at: Option<u64>,
    state: Arc<Mutex<State>>,
    op: Op,
    tool: Option<String>,
    start: Instant,
    end: Option<Instant>,
    /// Dropped without a line when set: the operation's span is held
    /// across work its line must never cover, and written only on the
    /// path that made the call the span names.
    disarmed: bool,
}

/// Starts an operation's timer on `diag`: the one function every timed
/// place goes through. At `info` it returns `None` before any clock read,
/// clone, allocation, lock or file operation. At `debug` the guard owns
/// the writer, the clock, the name and the tool, and emits once on drop.
/// An explicit stamp suppresses the start read, never the end read.
pub fn timed(diag: &Diag, op: Op, tool: Option<&str>, started: Option<Instant>) -> Option<Timer> {
    start_timer(
        diag.level,
        &diag.clock,
        &diag.logs,
        diag.process,
        diag.rotate_at,
        &diag.state,
        op,
        tool,
        started,
    )
}

/// [`timed`] and [`Diag::timings`] share this one level check and guard
/// construction, so the two never diverge.
#[allow(clippy::too_many_arguments, reason = "one call site per entry point")]
fn start_timer(
    level: Level,
    clock: &Arc<dyn Clock>,
    logs: &Path,
    process: Process,
    rotate_at: Option<u64>,
    state: &Arc<Mutex<State>>,
    op: Op,
    tool: Option<&str>,
    started: Option<Instant>,
) -> Option<Timer> {
    if level != Level::Debug {
        return None;
    }
    let start = started.unwrap_or_else(|| clock.now());
    Some(Timer {
        clock: Arc::clone(clock),
        logs: logs.to_owned(),
        process,
        rotate_at,
        state: Arc::clone(state),
        op,
        tool: tool.map(str::to_owned),
        start,
        end: None,
        disarmed: false,
    })
}

impl Timer {
    /// Stamps completion; the drop then emits with the stamped end. The
    /// first stamp wins.
    pub fn stop(&mut self) {
        if self.end.is_none() {
            self.end = Some(self.clock.now());
        }
    }

    /// Disarms the timer: dropping it writes no line. The operation's
    /// span is held across work its line must never cover, and dropped
    /// on the path that made the call the span names.
    pub fn discard(mut self) {
        self.disarmed = true;
    }
}

/// Whole milliseconds in `elapsed`: truncated, saturating to `u64::MAX`.
fn duration_ms(elapsed: Duration) -> u64 {
    elapsed.as_millis().try_into().unwrap_or(u64::MAX)
}

/// A frame past this budget writes `tui.frame`; one within it writes no
/// line, so scrolling leaves the log small.
const FRAME_BUDGET: Duration = Duration::from_millis(16);

impl Drop for Timer {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let end = self.end.unwrap_or_else(|| self.clock.now());
        // A clock that runs backwards contributes zero: no panic, and no
        // fabricated duration. A stamp from another clock reads zero too.
        let elapsed = end
            .checked_duration_since(self.start)
            .unwrap_or(Duration::ZERO);
        // A frame within its budget writes no line; the comparison is on
        // the untruncated duration, strictly over budget.
        if self.op == Op::TuiFrame && elapsed <= FRAME_BUDGET {
            return;
        }
        let ms = duration_ms(elapsed);
        let _files = lock(&FILES);
        let state = lock(&self.state);
        let line = Line {
            ts: wall_ms(self.clock.wall()),
            level: "debug",
            process: self.process.name(),
            session_id: state.session.as_ref().map(|session| session.0.as_str()),
            code: "op_timing",
            message: "An operation ended.",
            data: Some(TimingData {
                op: self.op,
                ms,
                tool: self.tool.as_deref(),
            }),
        };
        append_line(&state.file, &self.logs, self.rotate_at, &line);
    }
}

/// Renames `file` to `<file>.1` when it is over `at` bytes.
fn rotate(file: &Path, at: u64) {
    if !fs::metadata(file).is_ok_and(|meta| meta.len() > at) {
        return;
    }
    let mut previous = file.as_os_str().to_owned();
    previous.push(".1");
    fs::rename(file, Path::new(&previous)).unwrap_or(());
}

fn append(file: &Path, logs: &Path, bytes: &[u8]) {
    let open = || {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)
            .and_then(|mut file| file.write_all(bytes))
    };
    if open().is_err() {
        // The `logs/` directory may be missing: it was never created, or it
        // was removed while the process runs. Creating it only after a
        // failed write makes it again on the next line with no flag and no
        // `mkdir` on the common path; a failure that persists stays silent.
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(logs)
            .unwrap_or(());
        open().unwrap_or(());
    }
}

/// Files in `logs/` and `crashes/` older than this are deleted
/// (`docs/state.md`, "Bounds").
const OLD_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Besides the old files, each directory keeps this many of its newest.
const KEEP: usize = 100;

/// A diagnostic file to delete: its path and its length.
pub struct OldFile {
    /// The file's path.
    pub path: PathBuf,
    /// The file's length.
    pub bytes: u64,
}

/// Every file in `dir` that pruning deletes: the regular files older than
/// 30 days, then all but the newest 100 of the rest by mtime, sorted by path.
/// A missing directory, a symlink and a subdirectory select nothing; unreadable
/// metadata counts as empty at the epoch.
#[must_use]
pub fn old_diagnostics(dir: &Path, now: SystemTime) -> Vec<OldFile> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut old = Vec::new();
    let mut kept: Vec<(SystemTime, PathBuf, u64)> = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let (len, at, is_old) = match entry.metadata() {
            Ok(meta) => {
                let old = meta.modified().is_ok_and(|at| at + OLD_AFTER < now);
                let at = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                (meta.len(), at, old)
            }
            Err(_) => (0, SystemTime::UNIX_EPOCH, false),
        };
        if is_old {
            old.push(OldFile { path, bytes: len });
        } else {
            kept.push((at, path, len));
        }
    }
    kept.sort();
    let drop = kept.len().saturating_sub(KEEP);
    for (_, path, bytes) in kept.into_iter().take(drop) {
        old.push(OldFile { path, bytes });
    }
    old.sort_by(|a, b| a.path.cmp(&b.path));
    old
}

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "old_diagnostics_tests.rs"]
mod old_diagnostics_tests;
