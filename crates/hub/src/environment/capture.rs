//! The hub's login-shell capture (`docs/invocation.md`, "A session's
//! environment"): the variables an interactive login `$SHELL` ends with,
//! from `HOME`, `USER` and `SHELL` only, with a 10-second limit.

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use contract::clock::{Clock, Wake};
use support::lock;

/// How long the capture waits for the shell (`docs/invocation.md`, "The
/// capture has 10 seconds").
pub(crate) const CAPTURE_DEADLINE: Duration = Duration::from_secs(10);

/// Past this many bytes before the end marker the capture fails: above
/// Linux's usual 2 MiB `ARG_MAX`, which bounds any environment `env` can
/// print. Chosen, not measured.
pub(crate) const OUTPUT_CAP: usize = 4 * 1024 * 1024;

/// How long one wait inside the capture holds on the wall clock before it
/// re-checks: every wait has a wall-clock bound, also under a fake clock,
/// so a mutant cannot hang it.
const POLL: Duration = Duration::from_millis(20);

/// How the login-shell capture failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Failed {
    /// `SHELL` is unset or empty: nothing was spawned.
    NoShell,
    /// `SHELL` is not an absolute path: nothing was spawned.
    NotAbsolute,
    /// The shell, the socket pair or the reader thread's wake pipe could
    /// not start, naming only the error's kind.
    Spawn(std::io::ErrorKind),
    /// The shell did not finish within 10 seconds.
    TimedOut,
    /// The hub stopped before the shell finished. Never logged: a
    /// cancelled capture publishes silently.
    Cancelled,
    /// The shell exited before printing its environment.
    Exited,
    /// The shell's output could not be read as an environment.
    Malformed,
    /// The shell printed more than 4 MiB before the end marker.
    TooLarge,
}

impl Failed {
    /// The fixed clause the `environment_built` line joins into one
    /// sentence: never shell output or a value.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::NoShell => "SHELL is not set".to_owned(),
            Self::NotAbsolute => "SHELL is not an absolute path".to_owned(),
            Self::Spawn(kind) => format!("the shell could not start ({kind:?})"),
            Self::TimedOut => "the shell did not finish within 10 seconds".to_owned(),
            Self::Cancelled => "the hub stopped before the shell finished".to_owned(),
            Self::Exited => "the shell exited before printing its environment".to_owned(),
            Self::Malformed => "the shell's output could not be read as an environment".to_owned(),
            Self::TooLarge => "the shell printed more than 4 MiB".to_owned(),
        }
    }
}

/// What one capture waits on: the cancel flag and the bytes the reader
/// thread read. One lock, so a notify made under it after the waiter's
/// check is never missed. Always behind an `Arc`, shared by the waiter,
/// the reader thread and whoever may cancel it.
pub(crate) struct Cancel {
    /// The shared wait state.
    state: Mutex<State>,
    /// Notified on every change to [`State`], and when the clock moves.
    changed: Arc<Condvar>,
}

/// The capture's shared wait state, under [`Cancel`]'s lock.
struct State {
    /// [`Cancel::cancel`] ran.
    cancelled: bool,
    /// The bytes read from the shell so far.
    output: Vec<u8>,
    /// The reader thread ended.
    reader_done: bool,
    /// How much of `output` the wait already searched for the end marker.
    scanned: usize,
    /// The reader sealed the buffer: the end marker or the bound is in
    /// it, so nothing more is retained.
    sealed: bool,
}

impl Cancel {
    /// A wait no capture runs on yet: nothing cancelled, nothing read.
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                cancelled: false,
                output: Vec::new(),
                reader_done: false,
                scanned: 0,
                sealed: false,
            }),
            changed: Arc::new(Condvar::new()),
        })
    }

    /// Cancels the capture waiting on this: its wait ends, killing and
    /// reaping its shell at once. Idempotent.
    pub(crate) fn cancel(&self) {
        lock(&self.state).cancelled = true;
        self.changed.notify_all();
    }

    /// Whether [`Cancel::cancel`] ran.
    pub(crate) fn is_cancelled(&self) -> bool {
        lock(&self.state).cancelled
    }
}

/// Wakes a capture's wait when the clock moves: a best-effort notify on
/// top of the wait's wall-clock poll, so a move lands promptly. A lost
/// notify only delays the deadline check to the next poll: the deadline
/// itself is re-checked before every wait, and a passed deadline returns
/// at once, so the wait never hangs on the clock.
struct ClockWake {
    /// The capture's wait.
    changed: Arc<Condvar>,
}

impl Wake for ClockWake {
    // Best-effort on top of the wall-clock poll: without the notify the
    // deadline still lands on the next poll, so the wait's outcome never
    // depends on this running, only its promptness, which no test pins.
    // Skipped with a bare `cfg_attr`: cargo-mutants does not honour
    // `reason = "..."` inside one (its skip check errors on the unconsumed
    // value and silently treats the item as unskipped), so the reason
    // lives in this comment.
    #[cfg_attr(false, mutants::skip)]
    fn wake(&self) {
        self.changed.notify_all();
    }
}

/// Locks, never poisoned: no thread holding one of these locks panics
/// while holding it.
/// The shell command running `script` as an interactive login shell: `$SHELL`
/// with `-l -i -c`, from `base` only, with stdin and stderr on `/dev/null`
/// and stdout on `stdout`. The shell leads a new session with no
/// controlling terminal, whatever started the hub, so it never stops on
/// one. Never `process_group(0)`: `setsid` fails `EPERM` in a process that
/// already leads its group.
pub(crate) fn shell_command(
    shell: &Path,
    base: &[(OsString, OsString)],
    script: &str,
    stdout: Stdio,
) -> Command {
    let mut cmd = Command::new(shell);
    cmd.args(["-l", "-i", "-c", script]);
    cmd.env_clear();
    cmd.envs(base.iter().map(|(name, value)| (name, value)));
    cmd.stdin(Stdio::null());
    cmd.stdout(stdout);
    cmd.stderr(Stdio::null());
    // A login terminal starts in `HOME`.
    if let Some(home) = base
        .iter()
        .find(|(name, _)| name == "HOME")
        .map(|(_, value)| value)
    {
        let home = PathBuf::from(home);
        if home.is_dir() {
            cmd.current_dir(home);
        }
    }
    support::group::detach(&mut cmd, false);
    cmd
}

/// The framed script the shell runs: the begin marker, `/usr/bin/env -0`,
/// then the end marker, which prints only when `env` succeeded. The joined
/// markers appear only in `printf`'s output, never in the command line, so
/// a startup file echoing its arguments cannot forge them.
fn script(nonce: &str) -> String {
    format!(
        "printf '%s%s\\n' FIBER_ENV_BEGIN_ {nonce}; /usr/bin/env -0 && printf '%s%s\\n' FIBER_ENV_END_ {nonce}"
    )
}

/// The begin marker's bytes: `FIBER_ENV_BEGIN_<nonce>\n`.
fn begin_marker(nonce: &str) -> Vec<u8> {
    format!("FIBER_ENV_BEGIN_{nonce}\n").into_bytes()
}

/// The end marker's bytes: `FIBER_ENV_END_<nonce>\n`.
fn end_marker(nonce: &str) -> Vec<u8> {
    format!("FIBER_ENV_END_{nonce}\n").into_bytes()
}

/// The variables in `out`'s framed region: the bytes between the begin and
/// end markers for `nonce` as NUL-terminated `NAME=value` entries. Bytes
/// before the begin marker are chatter; bytes after the end marker are
/// ignored. An empty region is a valid empty environment.
pub(crate) fn parse(out: &[u8], nonce: &str) -> Result<Vec<(OsString, OsString)>, Failed> {
    let begin = begin_marker(nonce);
    let end = end_marker(nonce);
    let Some(begin_at) = find(out, &begin) else {
        return Err(Failed::Malformed);
    };
    let after_begin = begin_at + begin.len();
    let Some(tail) = out.get(after_begin..) else {
        return Err(Failed::Malformed);
    };
    let Some(end_at) = find(tail, &end).map(|at| at + after_begin) else {
        return Err(Failed::Malformed);
    };
    let Some(region) = out.get(after_begin..end_at) else {
        return Err(Failed::Malformed);
    };
    if region.is_empty() {
        return Ok(Vec::new());
    }
    let Some(body) = region.strip_suffix(&[0]) else {
        return Err(Failed::Malformed);
    };
    let mut vars = Vec::new();
    for entry in body.split(|byte| *byte == 0) {
        let Some(eq) = entry.iter().position(|byte| *byte == b'=') else {
            return Err(Failed::Malformed);
        };
        let (name, rest) = entry.split_at(eq);
        let Some(value) = rest.get(1..) else {
            return Err(Failed::Malformed);
        };
        if name.is_empty() {
            return Err(Failed::Malformed);
        }
        // The value holds everything past the first `=`, further `=` and
        // `\n` included.
        vars.push((os_string(name), os_string(value)));
    }
    Ok(vars)
}

/// The first index of `needle` in `haystack`, when one is there.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// `bytes` as an `OsString`, kept whole, never filtered.
#[cfg(unix)]
fn os_string(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::OsStr::from_bytes(bytes).to_owned()
}

/// `bytes` as an `OsString` where `OsStrExt` is missing.
// Mutants run on Linux, where this is not compiled; the macOS test job
// still runs its tests. Skipped with a bare `cfg_attr` (`reason = "..."`
// inside one is not honoured), so the reason lives in this comment.
#[cfg_attr(false, mutants::skip)]
#[cfg(not(unix))]
fn os_string(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

/// How much of `output`'s tail could be the end marker's first half:
/// the longest suffix of `output` that prefixes `needle`, strictly
/// shorter than `needle`. Zero when none. Only bytes past it count
/// against the cap while the marker is incomplete, so a valid prefix at
/// the cap followed by a split marker still parks instead of reporting
/// `TooLarge`.
fn marker_suffix(output: &[u8], needle: &[u8]) -> usize {
    (1..needle.len())
        .rev()
        .find(|&len| {
            needle
                .get(..len)
                .is_some_and(|prefix| output.ends_with(prefix))
        })
        .unwrap_or(0)
}

/// Reads `reader` to its end into `cancel`'s output, notifying the wait on
/// every chunk and at the end. Retains only until the end marker or the
/// cap plus one marker length: descendant chatter past either no longer
/// grows the buffer, so retained output stays bounded. One chunk may
/// overshoot the bound; the next chunk then stops. Returns only after the
/// stop ended the read or the shell and its descendants closed it: the
/// capture joins this, so no reader outlives its capture.
fn read_all(mut reader: support::stoppable::Reader, cancel: &Cancel, end: &[u8]) {
    let mut chunk = [0_u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(got) => {
                let Some(bytes) = chunk.get(..got) else {
                    break;
                };
                let mut state = lock(&cancel.state);
                // Bound retained output: once sealed, descendant chatter
                // no longer grows the buffer. While unsealed the buffer
                // holds no complete marker, so only the tail plus the new
                // bytes can newly hold one: each chunk costs one chunk,
                // not the whole buffer. Sealing keeps through the marker's
                // end, or stops past the cap plus one marker length; one
                // chunk may overshoot the bound, the next then seals.
                if !state.sealed {
                    let keep = end.len().saturating_sub(1).min(state.output.len());
                    let start = state.output.len().saturating_sub(keep);
                    let found = match state.output.get(start..) {
                        Some(tail) => {
                            let mut window = Vec::with_capacity(tail.len() + bytes.len());
                            window.extend_from_slice(tail);
                            window.extend_from_slice(bytes);
                            find(&window, end)
                        }
                        // Unreachable: `start` is `len` minus at most `len`.
                        None => None,
                    };
                    match found {
                        Some(at) => {
                            let through = at + end.len() - keep;
                            if let Some(part) = bytes.get(..through) {
                                state.output.extend_from_slice(part);
                            }
                            state.sealed = true;
                        }
                        None => {
                            if state.output.get(OUTPUT_CAP + end.len()).is_some() {
                                state.sealed = true;
                            } else {
                                state.output.extend_from_slice(bytes);
                            }
                        }
                    }
                }
                drop(state);
                cancel.changed.notify_all();
            }
            // Any read error ends the read, with no retry on `Interrupted`:
            // its only source is a signal mid-read, and the hub's signals
            // all cancel the capture anyway, so the read would end either
            // way. The feed reader over the same stoppable reader breaks
            // the same way.
            Err(_) => break,
        }
    }
    lock(&cancel.state).reader_done = true;
    cancel.changed.notify_all();
}

/// The end marker's absolute offset, from its offset in the tail past
/// the bytes already searched, with an overlap holding a split marker.
/// Single-poll outputs leave `from` at zero, where the offset arithmetic
/// is invisible, and multi-poll splits depend on read timing, so no test
/// pins it. Skipped with a bare `cfg_attr` (`reason = "..."` inside one
/// is not honoured), so the reason lives in this comment.
#[cfg_attr(false, mutants::skip)]
fn find_from(output: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    output
        .get(from..)
        .and_then(|tail| find(tail, needle).map(|at| at + from))
}

/// Whether the wait saw progress past what it parked on: a wake-up hint
/// only. Every condition is re-checked after every park, and the
/// wall-clock poll bounds each park, so a missed or spurious hint only
/// moves the next check by one poll: the outcome never depends on it.
/// Skipped with a bare `cfg_attr` (`reason = "..."` inside one is not
/// honoured), so the reason lives in this comment.
#[cfg_attr(false, mutants::skip)]
fn progressed(state: &State, done: bool, seen: usize) -> bool {
    state.cancelled || state.reader_done != done || state.output.len() != seen
}

/// Waits for the shell's end marker, end of output, the cap, the deadline
/// or cancellation: whichever lands first. The deadline reads the injected
/// clock; every wait also holds a wall-clock poll, so a mutant cannot hang
/// it.
fn wait(
    cancel: &Cancel,
    clock: &dyn Clock,
    deadline: Option<std::time::Instant>,
    nonce: &str,
) -> Result<Vec<(OsString, OsString)>, Failed> {
    let end = end_marker(nonce);
    loop {
        let mut state = lock(&cancel.state);
        if state.cancelled {
            return Err(Failed::Cancelled);
        }
        if deadline.is_some_and(|deadline| clock.now() >= deadline) {
            return Err(Failed::TimedOut);
        }
        // Only the bytes past the last search, with an overlap holding a
        // split marker: an occurrence starting earlier was already found,
        // so the first occurrence is still the first one seen.
        let from = state.scanned.saturating_sub(end.len());
        let found = find_from(&state.output, &end, from);
        state.scanned = state.output.len();
        // The cap covers only the bytes before the end marker: the prefix
        // up to it when found, all buffered bytes minus a possible split
        // marker's first half otherwise. Past the cap is `TooLarge`
        // whatever follows, even the end marker. Without a comparison: the
        // byte at `OUTPUT_CAP` exists inside the prefix exactly when the
        // prefix runs past the cap, so at the cap parks while past it
        // fails, with no `>` for a mutant to flip. Chatter after the end
        // marker never counts, so a valid capture followed by descendant
        // chatter still succeeds.
        let past_cap = match found {
            Some(at) => state
                .output
                .get(..at)
                .and_then(|prefix| prefix.get(OUTPUT_CAP))
                .is_some(),
            None => {
                let suffix = marker_suffix(&state.output, &end);
                state.output.get(OUTPUT_CAP + suffix).is_some()
            }
        };
        if past_cap {
            return Err(Failed::TooLarge);
        }
        if let Some(at) = found {
            // Copy only through the end marker: descendant chatter after
            // it never rides along in the success copy.
            let through = at + end.len();
            let framed = state
                .output
                .get(..through)
                .map_or_else(Vec::new, |prefix| prefix.to_vec());
            drop(state);
            return parse(&framed, nonce);
        }
        if state.reader_done {
            return Err(Failed::Exited);
        }
        // What the wait has seen: any change past it ends the park at
        // once, so a notify made under the lock after this check is never
        // missed. The clock's notify is best-effort on top: the deadline
        // is re-checked above before every park.
        let seen = state.output.len();
        let done = state.reader_done;
        support::clock::park(
            clock,
            deadline,
            Some(POLL),
            &cancel.changed,
            state,
            |state| progressed(state, done, seen),
        );
    }
}

/// Reaps `child` on a detached `hub-reap` thread, so a shell stuck in
/// uninterruptible I/O after `SIGKILL` never holds the capture: the capture
/// returns once the reader joined. Only when that thread cannot start does
/// the capture reap inline.
fn reap(child: Child) {
    let slot = Arc::new(Mutex::new(Some(child)));
    let held = Arc::clone(&slot);
    match std::thread::Builder::new()
        .name("hub-reap".to_owned())
        .spawn(move || {
            let taken = lock(&held).take();
            if let Some(mut child) = taken {
                let _reaped = child.wait();
            }
        }) {
        Ok(_) => {}
        Err(_) => {
            let taken = lock(&slot).take();
            if let Some(mut child) = taken {
                let _reaped = child.wait();
            }
        }
    }
}

/// The variables an interactive login `$SHELL` ends with, from `HOME`,
/// `USER` and `SHELL` of `inherited` only. `SHELL` unset or empty is
/// [`Failed::NoShell`], not absolute [`Failed::NotAbsolute`], both without
/// spawning. Every exit path kills the shell, stops and joins the reader,
/// then reaps the shell on a detached thread, so a shell stuck after
/// `SIGKILL` never holds the capture.
pub(crate) fn capture(
    inherited: &[(OsString, OsString)],
    clock: &dyn Clock,
    cancel: &Cancel,
) -> Result<Vec<(OsString, OsString)>, Failed> {
    let shell = inherited
        .iter()
        .find(|(name, _)| name == "SHELL")
        .map(|(_, value)| value);
    let Some(shell) = shell else {
        return Err(Failed::NoShell);
    };
    if shell.is_empty() {
        return Err(Failed::NoShell);
    }
    let shell = PathBuf::from(shell);
    if !shell.is_absolute() {
        return Err(Failed::NotAbsolute);
    }
    if cancel.is_cancelled() {
        return Err(Failed::Cancelled);
    }
    let base: Vec<(OsString, OsString)> = inherited
        .iter()
        .filter(|(name, _)| name == "HOME" || name == "USER" || name == "SHELL")
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let (hub_end, child_end) = UnixStream::pair().map_err(|error| Failed::Spawn(error.kind()))?;
    let (reader, stop) = support::stoppable::reader(hub_end).map_err(|error| match error {
        support::stoppable::Error::Pipe(source) => Failed::Spawn(source.kind()),
        support::stoppable::Error::Stopped => Failed::Spawn(std::io::ErrorKind::Other),
    })?;
    let nonce = crate::start::mint("");
    let child_end: std::os::unix::io::OwnedFd = child_end.into();
    let stdout = Stdio::from(child_end);
    let text = script(&nonce);
    // The deadline runs from just before spawn.
    let deadline = clock.now().checked_add(CAPTURE_DEADLINE);
    // The command drops right after spawn: it holds the parent's copy of
    // the shell's end of the stdout socket, and only once that copy is
    // closed does end of file arrive when the shell and its descendants
    // close it.
    let mut child = {
        let mut command = shell_command(&shell, &base, &text, stdout);
        match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                stop.stop();
                return Err(Failed::Spawn(error.kind()));
            }
        }
    };
    // The clock's wake holds only the wait's condvar: the clock never
    // keeps the capture alive.
    let wake: Arc<dyn Wake> = Arc::new(ClockWake {
        changed: Arc::clone(&cancel.changed),
    });
    clock.subscribe(Arc::downgrade(&wake));
    let end = end_marker(&nonce);
    let outcome = std::thread::scope(|scope| {
        scope.spawn(move || read_all(reader, cancel, &end));
        let outcome = wait(cancel, clock, deadline, &nonce);
        // Whatever ended the wait, the shell ends now: an error because
        // it already exited is ignored.
        let _killed = child.kill();
        stop.stop();
        outcome
    });
    // The scope joined the reader: the stop woke its poll even when a
    // descendant a startup file left still holds the shell's end of the
    // socket, so the join above is bounded by the wall clock.
    reap(child);
    outcome
}

#[cfg(test)]
#[path = "capture_tests.rs"]
mod tests;
