//! The one way a test signals a process group or a single process
//! (`docs/testing.md`, "Running tests"). `kill(-1)` signals every process the
//! user owns, and `kill(-0)` or `kill -- 0` the caller's own group, so an id
//! of 1 or less is refused before anything runs. The command-line form
//! refuses an empty match, which reaches every process the user owns.
//!
//! Signals and the exit probes (`alive`, `group_lives`) go through the
//! kill(2) call itself, starting no process; the waits built on the probes
//! (`pids_exit`, `matching_exits`) start none per pass. The one child left,
//! `pgrep`, is waited for under a deadline on the wall clock.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::Duration;

use rustix::process::{Pid, Signal};

use crate::deadline::Deadline;

/// The shell script of a watchdog: `sh -c WATCHDOG_SCRIPT watchdog <group>`.
/// Reading a line from stdin means the run finished; EOF means the test
/// process died, so the script kills `<group>`. An argument of 1 or less
/// exits with status 2 before `kill` can run.
pub const WATCHDOG_SCRIPT: &str =
    r#"[ "$1" -gt 1 ] || exit 2; read -r line || kill -s KILL -- "-$1""#;

/// Sends `signal` (`HUP`, `INT`, `KILL`, `TERM`, `WINCH`, or `0` to probe) to
/// process group `group` through kill(-group, signal), starting no process,
/// and returns whether the kernel accepted it. A refused send (no such
/// group, no permission) is `Ok(false)`.
///
/// # Errors
///
/// When `signal` is not one of the names above, or `group` does not fit in
/// a pid.
///
/// # Panics
///
/// When `group` is 1 or less, before anything else.
pub fn kill_group(group: u32, signal: &str) -> io::Result<bool> {
    signal_group(group, signal, |id, sig| match sig {
        Some(sig) => rustix::process::kill_process_group(id, sig),
        None => rustix::process::test_kill_process_group(id),
    })
}

/// [`kill_group`] with the kernel call injected: the refusal of a group of 1
/// or less comes before the name is read or `deliver` runs.
fn signal_group(
    group: u32,
    signal: &str,
    deliver: impl FnOnce(Pid, Option<Signal>) -> rustix::io::Result<()>,
) -> io::Result<bool> {
    send(checked(group, "process group")?, signal, deliver)
}

/// [`kill_pid`] with the kernel call injected: the refusal of a pid of 1 or
/// less comes before the name is read or `deliver` runs.
fn signal_pid(
    pid: u32,
    signal: &str,
    deliver: impl FnOnce(Pid, Option<Signal>) -> rustix::io::Result<()>,
) -> io::Result<bool> {
    send(checked(pid, "pid")?, signal, deliver)
}

/// The pid `id` names for `kind` (`"pid"` or `"process group"`): panics
/// when `id` is 1 or less, before anything runs, since the signal would
/// reach processes the test does not own. An id past the pid range is an
/// `InvalidInput` error that sends nothing.
pub(crate) fn checked(id: u32, kind: &str) -> io::Result<Pid> {
    assert!(
        id > 1,
        "refusing to signal {kind} {id}: an id of 1 or less reaches processes the test does not own"
    );
    i32::try_from(id)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("{id} is not a pid")))
}

/// Delivers `signal` to the checked `id`; `None` is the probe `0`.
/// Called only after the refusal of an id of 1 or less.
fn send(
    id: Pid,
    signal: &str,
    deliver: impl FnOnce(Pid, Option<Signal>) -> rustix::io::Result<()>,
) -> io::Result<bool> {
    let signal = signal_named(signal)?;
    Ok(deliver(id, signal).is_ok())
}

/// The signal a name stands for, `None` for the probe `0`.
fn signal_named(name: &str) -> io::Result<Option<Signal>> {
    match name {
        "0" => Ok(None),
        "HUP" => Ok(Some(Signal::HUP)),
        "INT" => Ok(Some(Signal::INT)),
        "KILL" => Ok(Some(Signal::KILL)),
        "TERM" => Ok(Some(Signal::TERM)),
        "WINCH" => Ok(Some(Signal::WINCH)),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("no signal named {name:?}"),
        )),
    }
}

/// Waits up to `deadline` on the wall clock for process group `group` to
/// empty, and returns whether it did. One probe right after the group's
/// leader exits is a race: a transient child, such as a `cat` in a command
/// substitution, can outlive it for a moment under load. The probes run on
/// a thread, so the deadline holds even when a probe is slow. Any probe
/// error (no such group, no permission) counts as empty.
///
/// # Panics
///
/// When `group` is 1 or less (see [`kill_group`]), on the probe thread, so the
/// wait then returns `false`.
#[must_use]
pub fn group_empties(group: u32, deadline: Duration) -> bool {
    let (emptied, empty) = mpsc::channel();
    let (stop, stopped) = mpsc::channel::<()>();
    thread::spawn(move || {
        while group_lives(group) {
            if stop_closed(&stopped) {
                return;
            }
        }
        match emptied.send(()) {
            Ok(()) | Err(_) => {}
        }
    });
    let result = Deadline::after(deadline).recv(&empty).is_ok();
    drop(stop);
    result
}

/// Sends `signal` (`HUP`, `INT`, `KILL`, `TERM`, `WINCH`, or `0` to probe) to
/// process `pid` through kill(pid, signal), starting no process, and
/// returns whether the kernel accepted it. A refused send (no such process,
/// no permission) is `Ok(false)`.
///
/// # Errors
///
/// When `signal` is not one of the names above, or `pid` does not fit in a
/// pid.
///
/// # Panics
///
/// When `pid` is 1 or less, before anything else. kill(0) signals the
/// caller's own process group.
pub fn kill_pid(pid: u32, signal: &str) -> io::Result<bool> {
    signal_pid(pid, signal, |id, sig| match sig {
        Some(sig) => rustix::process::kill_process(id, sig),
        None => rustix::process::test_kill_process(id),
    })
}

/// The variable naming a matching watchdog's pattern: the environment, not
/// the command line, so the watchdog never matches itself.
pub(crate) const MATCHING_PATTERN_VAR: &str = "FIBER_WATCHDOG_PATTERN";

/// The shell script of a matching watchdog, `sh -c MATCHING_WATCHDOG_SCRIPT
/// watchdog` with [`MATCHING_PATTERN_VAR`] set to a [`pattern`]. Reading a
/// line from stdin means the run finished; EOF means the test process died,
/// so the script kills every process whose command line matches, and its
/// process group. An empty pattern, which matches every process, exits with
/// status 2 before `pgrep` can run.
pub(crate) const MATCHING_WATCHDOG_SCRIPT: &str = r#"[ -n "$FIBER_WATCHDOG_PATTERN" ] || exit 2; read -r line || for p in $(pgrep -f -- "$FIBER_WATCHDOG_PATTERN"); do [ "$p" -gt 1 ] || continue; kill -s KILL -- "-$p"; kill -s KILL "$p"; done"#;

/// `text` as an extended regular expression matching itself, for `pgrep -f`.
/// A leading plain character becomes a bracket expression (`/tmp/x` is
/// `[/]tmp/x`), so the pattern's spelling in a pgrep's command line does not
/// match that command line, and a listing pgrep or a watchdog sweep does not
/// list a peer's pgrep.
/// debt: only for the path-shaped texts callers pass (a plain run after the
/// first character); `a`, `a]` and `.abc` still match their own spelling.
/// The first caller to pass one breaks the self-match another way.
///
/// # Panics
///
/// When `text` is empty: it matches every process the user owns.
pub(crate) fn pattern(text: &str) -> String {
    assert!(
        !text.is_empty(),
        "refusing an empty command-line match: it matches every process the user owns"
    );
    let mut escaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    match chars.next() {
        Some(first) if ".[]()*+?{}|^$\\".contains(first) => {
            escaped.push('\\');
            escaped.push(first);
        }
        Some(first) => {
            escaped.push('[');
            escaped.push(first);
            escaped.push(']');
        }
        None => {}
    }
    for c in chars {
        if ".[]()*+?{}|^$\\".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }
    escaped
}

/// How long [`matching`] waits on the wall clock for `pgrep` to list and
/// exit. A listing takes milliseconds; the bound only reports a hung one.
const PGREP_DEADLINE: Duration = Duration::from_secs(5);

/// The pids of the processes whose command line contains `text`.
///
/// # Errors
///
/// When `pgrep` cannot be run or fails, or has not exited within
/// [`PGREP_DEADLINE`] (`TimedOut`; the `pgrep` is killed).
///
/// # Panics
///
/// When `text` is empty, before running anything.
pub fn matching(text: &str) -> io::Result<Vec<u32>> {
    let pattern = pattern(text);
    // Checked again on the result: an empty pattern matches every process.
    assert!(
        !pattern.is_empty(),
        "refusing an empty command-line match: it matches every process the user owns"
    );
    let pgrep = Command::new("pgrep")
        .args(["-f", "--", &pattern])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let (status, stdout) = bounded(pgrep, "pgrep", PGREP_DEADLINE)?;
    // 1 is "no process matched".
    match status.code() {
        Some(0 | 1) => {}
        _ => return Err(io::Error::other(format!("pgrep failed: {status}"))),
    }
    Ok(String::from_utf8_lossy(&stdout)
        .split_whitespace()
        .filter_map(|pid| pid.parse().ok())
        .collect())
}

/// What a bounded child left: its exit status and everything it wrote to
/// stdout.
type Finished = (ExitStatus, Vec<u8>);

/// Waits up to `deadline` on the wall clock for `child`, whose stdout is
/// piped, to close its stdout and exit. A thread reads stdout to its end,
/// then polls `try_wait` under a short lock, releasing it between polls, so
/// a child that closed its stdout but hangs never holds the lock. On a miss
/// the lock is free, so the child killed here is still unreaped and its pid
/// cannot have gone to another process; the thread then reaps it. A miss is
/// a `TimedOut` error naming `what`.
fn bounded(mut child: Child, what: &str, deadline: Duration) -> io::Result<Finished> {
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other(format!("{what} has no piped stdout")))?;
    let child = Arc::new(Mutex::new(child));
    let reaping = Arc::clone(&child);
    let (done, finished) = mpsc::channel::<io::Result<Finished>>();
    thread::spawn(move || {
        let mut out = Vec::new();
        // `tick` never sends: holding it turns the receive below into a
        // bounded wait that holds no lock.
        let (tick, tock) = mpsc::channel::<()>();
        let result = stdout.read_to_end(&mut out).and_then(|_| {
            loop {
                let status = {
                    let mut child = reaping.lock().unwrap_or_else(PoisonError::into_inner);
                    child.try_wait()
                };
                match status {
                    Ok(Some(status)) => break Ok((status, out)),
                    Ok(None) => {}
                    Err(err) => break Err(err),
                }
                // `tock` never carries a message: this is the shared pause
                // between polls, holding no lock.
                stop_closed(&tock);
            }
        });
        drop(tick);
        match done.send(result) {
            Ok(()) | Err(_) => {}
        }
    });
    match Deadline::after(deadline).recv(&finished) {
        Ok(result) => result,
        Err(_) => {
            // The worker holds the lock only across the non-blocking
            // `try_wait`, so this acquires at once; the child killed here
            // is still unreaped. The thread never panics while holding it,
            // so it is never poisoned.
            match child.lock().unwrap_or_else(PoisonError::into_inner).kill() {
                Ok(()) | Err(_) => {}
            }
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("{what} did not exit within {deadline:?}"),
            ))
        }
    }
}

/// Sends SIGKILL to every process whose command line contains `text`, and
/// to its process group.
///
/// # Errors
///
/// When `pgrep` cannot be run or misses its deadline ([`matching`]).
///
/// # Panics
///
/// When `text` is empty, before running anything, or when a match is pid 1
/// or less ([`kill_group`], [`kill_pid`]).
pub fn kill_matching(text: &str) -> io::Result<()> {
    for pid in matching(text)? {
        kill_group(pid, "KILL")?;
        kill_pid(pid, "KILL")?;
    }
    Ok(())
}

/// Whether `pid` exists: kill(pid, 0), starting no process. Panics when
/// `pid` is 1 or less.
#[allow(
    clippy::expect_used,
    reason = "a probe of an id past the pid range panics, as it did before the shared guard"
)]
fn alive(pid: u32) -> bool {
    let id = checked(pid, "pid").expect("a pid past the pid range is never alive");
    rustix::process::test_kill_process(id).is_ok()
}

/// Whether process group `group` has a member: kill(-group, 0), starting no
/// process. Panics when `group` is 1 or less.
#[allow(
    clippy::expect_used,
    reason = "a probe of an id past the pid range panics, as it did before the shared guard"
)]
fn group_lives(group: u32) -> bool {
    let id = checked(group, "process group").expect("a group past the pid range is never live");
    rustix::process::test_kill_process_group(id).is_ok()
}

/// How long each liveness wait holds between probes: one bounded wait
/// against its stop channel, so no wait spins.
const POLL: Duration = Duration::from_millis(10);

/// True once `stop` sent or disconnected: the one bounded wait every probe
/// loop shares, holding no lock between probes.
fn stop_closed(stop: &mpsc::Receiver<()>) -> bool {
    !matches!(
        Deadline::after(POLL).recv(stop),
        Err(mpsc::RecvTimeoutError::Timeout)
    )
}

/// The probe loop: true once every pid fails the probe; false once `stop`
/// disconnects. The first probe runs at once, the rest one shared poll
/// interval apart, so the wait holds no core between them.
fn wait_exits(pids: &[u32], stop: &mpsc::Receiver<()>, mut probe: impl FnMut(u32) -> bool) -> bool {
    loop {
        if !pids.iter().any(|pid| probe(*pid)) {
            return true;
        }
        if stop_closed(stop) {
            return false;
        }
    }
}

/// Waits up to `deadline` on the wall clock for every pid in `pids` to exit,
/// probing with kill(pid, 0) and starting no process. Runs on a thread.
/// Panics (on that thread, so the result is `false`) when a pid is 1 or
/// less.
#[must_use]
pub fn pids_exit(pids: &[u32], deadline: Duration) -> bool {
    let pids = pids.to_vec();
    let (done, finished) = mpsc::channel::<bool>();
    let (stop, stopped) = mpsc::channel::<()>();
    thread::spawn(move || {
        let exited = wait_exits(&pids, &stopped, alive);
        match done.send(exited) {
            Ok(()) | Err(_) => {}
        }
    });
    let result = Deadline::after(deadline)
        .recv(&finished)
        .unwrap_or_default();
    drop(stop);
    result
}

/// What a command line lookup prints when the process is already gone:
/// the pid in the expiry message still names the holder.
const GONE_ARGS: &str = "<gone>";

/// What a command line lookup prints when the lookup itself hung and was
/// killed: nothing is known about the pid, not even that it is gone.
const PS_TIMEOUT: &str = "<ps timed out>";

/// Spawns a command line lookup: `program` with `args`, stdio closed but
/// stdout piped for [`bounded`]. Its own process group, so a watchdog or a
/// group kill for the lookup never reaches the test, and the test's own
/// group kills never reach the lookup.
fn spawn_lookup(program: &str, args: &[&str]) -> io::Result<Child> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
}

/// Reads a spawned lookup to its end under `deadline`: its trimmed stdout,
/// `<gone>` when it fails or prints nothing, `<ps timed out>` when the
/// lookup itself hung and was killed. [`bounded`] reaps the killed child.
fn read_lookup(child: Child, what: &str, deadline: Duration) -> String {
    match bounded(child, what, deadline) {
        Ok((status, out)) if status.success() => {
            let args = String::from_utf8_lossy(&out).trim().to_owned();
            if args.is_empty() {
                GONE_ARGS.to_owned()
            } else {
                args
            }
        }
        Ok(_) => GONE_ARGS.to_owned(),
        Err(err) if err.kind() == io::ErrorKind::TimedOut => PS_TIMEOUT.to_owned(),
        Err(_) => GONE_ARGS.to_owned(),
    }
}

/// `pid`'s command line through `ps -o args= -p <pid>`, bounded like
/// [`matching`]'s `pgrep`: a hung `ps` is killed, never waited out.
fn command_line(pid: u32) -> String {
    match spawn_lookup("ps", &["-o", "args=", "-p", &pid.to_string()]) {
        Ok(child) => read_lookup(child, "ps", PGREP_DEADLINE),
        Err(_) => GONE_ARGS.to_owned(),
    }
}

/// Each outstanding pid with its command line, for the expiry message.
fn describe_holders(pids: &[u32]) -> String {
    pids.iter()
        .map(|pid| format!("{pid} {}", command_line(*pid)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The deadline's error: `TimedOut`, naming every pid from the latest
/// listing still outstanding with its command line, so the failure
/// diagnoses itself. With no listing yet there is nothing to name.
fn expiry_error(pids: &[u32]) -> io::Error {
    if pids.is_empty() {
        io::Error::new(io::ErrorKind::TimedOut, "deadline expired waiting for exit")
    } else {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "deadline expired waiting for exit: {}",
                describe_holders(pids)
            ),
        )
    }
}

/// Publishes the latest listing for the expiry error: the wait holds no
/// lock, so the deadline's read never blocks on it.
fn publish(published: &Mutex<Vec<u32>>, pids: &[u32]) {
    *published.lock().unwrap_or_else(PoisonError::into_inner) = pids.to_vec();
}

/// `matching_exits` with the listing injected: `list` runs before the
/// probes and again after every wait. The test seam for the final check.
fn listed_exit(
    list: impl FnMut() -> io::Result<Vec<u32>> + Send + 'static,
    deadline: Duration,
) -> io::Result<()> {
    let (done, finished) = mpsc::channel::<io::Result<()>>();
    let (stop, stopped) = mpsc::channel::<()>();
    // The latest listing, published after each one: the deadline's read
    // names the holders even while the wait still runs.
    let outstanding = Arc::new(Mutex::new(Vec::<u32>::new()));
    let published = Arc::clone(&outstanding);
    thread::spawn(move || {
        let mut list = list;
        // The first listing's failure names itself; every later listing
        // ran after a wait, so each names the second.
        let mut pids = match list() {
            Ok(pids) => pids,
            Err(err) => {
                match done.send(Err(io::Error::other(format!("first pgrep failed: {err}")))) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
        };
        loop {
            publish(&published, &pids);
            if !wait_exits(&pids, &stopped, alive) {
                match done.send(Err(expiry_error(&pids))) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
            pids = match list() {
                Ok(pids) => pids,
                Err(err) => {
                    match done.send(Err(io::Error::other(format!("second pgrep failed: {err}")))) {
                        Ok(()) | Err(_) => {}
                    }
                    return;
                }
            };
            // A process that started after the listing just waited on
            // shows here; it is waited on like every earlier match.
            if pids.is_empty() {
                match done.send(Ok(())) {
                    Ok(()) | Err(_) => {}
                }
                return;
            }
        }
    });
    let result = match Deadline::after(deadline).recv(&finished) {
        Ok(result) => result,
        Err(_) => {
            let pids = outstanding
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            Err(expiry_error(&pids))
        }
    };
    drop(stop);
    result
}

/// Waits up to `deadline` on the wall clock for every process whose command
/// line contains `text` to exit: one `pgrep` lists the matches, the probes
/// wait for each listed pid, and `pgrep` lists again after every wait until
/// nothing matches. One thread, one deadline; a process that starts after
/// an earlier listing is waited on like every earlier match.
///
/// # Errors
///
/// When a `pgrep` fails or when the deadline expires first. The message
/// names the path; the expiry names the holders too.
pub fn try_matching_exits(text: &str, deadline: Duration) -> io::Result<()> {
    let text = text.to_owned();
    listed_exit(move || matching(&text), deadline)
}

/// [`try_matching_exits`] as a boolean: `false` on any failure path.
#[must_use]
pub fn matching_exits(text: &str, deadline: Duration) -> bool {
    try_matching_exits(text, deadline).is_ok()
}

#[cfg(test)]
#[path = "process_group_tests.rs"]
mod tests;
