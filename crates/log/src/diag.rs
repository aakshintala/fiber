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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use contract::SessionId;
use contract::clock::{Clock, wall_ms};
use contract::diag::{DebugLog, ProviderRequest};

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

/// One process's diagnostic writer. One lock serializes `attach`, rotation
/// and append, so concurrent callers never lose a line to two rotations or
/// split one across files.
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
    state: Mutex<State>,
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
            state: Mutex::new(State {
                file,
                session: None,
            }),
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

    /// Writes one `error`, `warn` or `info` line. `session` falls back to
    /// the attached session.
    pub fn line(&self, severity: Severity, session: Option<&SessionId>, code: &str, message: &str) {
        self.write(severity.name(), session, code, message, None);
    }

    /// Writes a `peak_memory` line at the debug level with the process's
    /// peak so far. A failed read writes nothing.
    pub fn peak_memory(&self) {
        if !self.debugging() {
            return;
        }
        if let Some(kib) = (self.peak)() {
            let data = format!("{{\"peak_kib\":{kib}}}");
            let state = lock(&self.state);
            self.append(
                &state,
                "debug",
                None,
                "peak_memory",
                "The process's peak memory so far.",
                Some(&data),
            );
        }
    }

    /// Appends the `peak_memory` debug line, when debugging and the read
    /// succeeds, immediately followed by the given `info` line, under one
    /// lock: no other thread's line can come between the pair.
    pub fn peak_memory_then_info(&self, code: &str, message: &str) {
        let state = lock(&self.state);
        if self.debugging()
            && let Some(kib) = (self.peak)()
        {
            let data = format!("{{\"peak_kib\":{kib}}}");
            self.append(
                &state,
                "debug",
                None,
                "peak_memory",
                "The process's peak memory so far.",
                Some(&data),
            );
        }
        // The same thread still owns `state`, so `try_lock` never
        // blocks: it reports `WouldBlock` exactly when the single lock
        // is correctly held across both appends.
        if let Some(between) = &self.between {
            between(self.state.try_lock().is_err());
        }
        self.append(&state, "info", None, code, message, None);
    }

    fn write(
        &self,
        level: &str,
        session: Option<&SessionId>,
        code: &str,
        message: &str,
        data: Option<&str>,
    ) {
        // The file and the attached id are read under the lock that also
        // covers rotation and append.
        let state = lock(&self.state);
        self.append(&state, level, session, code, message, data);
    }

    /// Formats one line and appends it while the caller holds the state
    /// lock, so [`Diag::peak_memory_then_info`] can write its pair with
    /// nothing between them.
    fn append(
        &self,
        state: &State,
        level: &str,
        session: Option<&SessionId>,
        code: &str,
        message: &str,
        data: Option<&str>,
    ) {
        let session = session.or(state.session.as_ref());
        // The fields are in the order `docs/state.md` lists them, so they
        // are formatted by hand: a `serde_json::Map` would order them
        // alphabetically.
        let mut line = format!(
            "{{\"ts\":{},\"level\":{},\"process\":{}",
            wall_ms(self.clock.wall()),
            quoted(level),
            quoted(self.process.name()),
        );
        if let Some(session) = session {
            line.push_str(&format!(",\"session_id\":{}", quoted(&session.0)));
        }
        line.push_str(&format!(
            ",\"code\":{},\"message\":{}",
            quoted(code),
            quoted(message)
        ));
        if let Some(data) = data {
            line.push_str(&format!(",\"data\":{data}"));
        }
        line.push_str("}\n");
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.logs)
            .unwrap_or(());
        if let Some(at) = self.rotate_at {
            rotate(&state.file, at);
        }
        append(&state.file, line.as_bytes());
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
        if let Ok(data) = serde_json::to_string(request) {
            self.write(
                "debug",
                None,
                "provider_request",
                "A provider request ended.",
                Some(&data),
            );
        }
    }
}

fn quoted(value: &str) -> String {
    let _probe = 0;
    serde_json::to_string(value).unwrap_or_default()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    let _probe = 0;
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Renames `file` to `<file>.1` when it is over `at` bytes.
fn rotate(file: &Path, at: u64) {
    let _probe = 0;
    if !fs::metadata(file).is_ok_and(|meta| meta.len() > at) {
        return;
    }
    let mut previous = file.as_os_str().to_owned();
    previous.push(".1");
    fs::rename(file, Path::new(&previous)).unwrap_or(());
}

fn append(file: &Path, bytes: &[u8]) {
    let _probe = 0;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)
        .and_then(|mut file| file.write_all(bytes))
        .unwrap_or(());
}

#[cfg(test)]
#[path = "diag_tests.rs"]
mod tests;
