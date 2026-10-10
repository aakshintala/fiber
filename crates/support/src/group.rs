//! A process group Fiber started, from spawn to empty: the process-wide list,
//! the probe, the signals, and the refusal of an id of 1 or less
//! (`docs/testing.md`, "Running tests").

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rustix::process::{Pid, Signal};

/// SIGKILL follows SIGTERM by this long (`docs/tools.md`, "Stopping a command").
pub const GRACE: Duration = Duration::from_millis(800);

/// How long output is read after the group is empty or the stop
/// (`docs/tools.md`, "Stopping a command").
pub const DRAIN: Duration = Duration::from_secs(2);

/// How often a group is re-checked while the shell has exited and members
/// remain. Picked, not measured.
pub const GROUP_POLL: Duration = Duration::from_millis(10);

/// Whether `id` is refused before anything runs: an id of 1 or less is never
/// a run's group. `kill(-1)` reaches every process the user owns, and
/// `kill(0)` the caller's own group, so every function below that reaches a
/// syscall refuses through this before converting to a [`Pid`]: no id of 1 or
/// less reaches `kill`, on any path, whatever the caller checked.
#[must_use]
pub fn refused(id: u32) -> bool {
    id <= 1
}

/// What signalling or spawning a process group can refuse or fail with.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An id of 1 or less, or one past what a pid holds: signalling it could
    /// reach processes outside the run.
    #[error("refusing to signal process group or process {0}")]
    Refused(u32),
    /// The program could not start. The error is kept whole so callers still
    /// read its `kind()`, as for a missing program.
    #[error("the program could not start: {0}")]
    Spawn(#[source] io::Error),
    /// Signalling failed with an errno other than `ESRCH`. A group already
    /// gone reads as `Ok(())`.
    #[error("signalling {id}: {source}")]
    Signal {
        /// The group or process the signal did not reach.
        id: u32,
        /// The errno behind the failure.
        #[source]
        source: io::Error,
    },
}

/// Whether the group still holds a process, through `kill(-pgid, 0)`. A
/// refused id, or one past what a pid holds, reads empty: the probe sends
/// nothing for it.
#[must_use]
pub fn alive(pgid: u32) -> bool {
    let Some(pid) = pid_of(pgid) else {
        return false;
    };
    rustix::process::test_kill_process_group(pid).is_ok()
}

/// Sends `signal` to process group `pgid`, through `kill(-pgid, signal)`. A
/// refused id, or one past what a pid holds, is [`Error::Refused`], returned
/// before any conversion. A group already gone (`ESRCH`) is `Ok(())`; any
/// other errno is [`Error::Signal`].
///
/// # Errors
///
/// When the id is refused or past what a pid holds, or the signal fails with
/// an errno other than `ESRCH`.
pub fn signal(pgid: u32, signal: Signal) -> Result<(), Error> {
    let Some(pid) = pid_of(pgid) else {
        return Err(Error::Refused(pgid));
    };
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(errno) => Err(Error::Signal {
            id: pgid,
            source: io::Error::from_raw_os_error(errno.raw_os_error()),
        }),
    }
}

/// Sends `signal` to the single process `pid`, through `kill(pid, signal)`.
/// Only for a child the caller owns and has not reaped: a reaped id may
/// already belong to another process. Refusal and the gone-process read are
/// the same as [`signal`]'s.
///
/// # Errors
///
/// When the id is refused or past what a pid holds, or the signal fails with
/// an errno other than `ESRCH`.
pub fn signal_process(pid: u32, signal: Signal) -> Result<(), Error> {
    let Some(id) = pid_of(pid) else {
        return Err(Error::Refused(pid));
    };
    match rustix::process::kill_process(id, signal) {
        Ok(()) | Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(errno) => Err(Error::Signal {
            id: pid,
            source: io::Error::from_raw_os_error(errno.raw_os_error()),
        }),
    }
}

/// One registration in the process-wide list. Only this token's holder, or
/// [`kill_every_group`]'s prune of an empty group, removes its entry, so a
/// reused id never loses another run's registration.
#[must_use]
pub struct Listing {
    serial: u64,
    pgid: u32,
}

impl Listing {
    /// The group this registration lists.
    #[must_use]
    pub fn pgid(&self) -> u32 {
        self.pgid
    }
}

/// What is listed now: each entry is one registration's serial and group id.
struct State {
    entries: Vec<(u64, u32)>,
    /// Set by the first [`kill_every_group`], never cleared: a shutdown runs
    /// once, after which the process exits.
    killed: bool,
}

/// The one process-wide list of live groups.
static LIVE: Mutex<State> = Mutex::new(State {
    entries: Vec::new(),
    killed: false,
});

/// The next registration's serial, never reused in the process.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// The process-wide list, locked until the guard drops. Never hold it across
/// [`Command::spawn`]: spawn through [`spawn`], which lists the child after
/// the fork, so a stalled spawn can never delay [`kill_every_group`].
#[must_use]
pub fn live() -> Live {
    Live {
        guard: crate::lock(&LIVE),
    }
}

/// The process-wide list, locked until the guard drops. No method takes the
/// lock again, so a caller holding `Live` may still call [`alive`],
/// [`signal`] and [`signal_process`]: they take no lock.
pub struct Live {
    guard: std::sync::MutexGuard<'static, State>,
}

impl Live {
    /// Whether any registration lists `pgid`.
    #[must_use]
    pub fn contains(&self, pgid: u32) -> bool {
        self.guard.entries.iter().any(|(_, id)| *id == pgid)
    }

    /// How many registrations are listed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.guard.entries.len()
    }

    /// Whether no registration is listed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.guard.entries.is_empty()
    }

    /// Lists `pgid` and returns the registration for it. A refused id is
    /// never listed and reads as `None`. When a shutdown already ran, the
    /// group is listed and sent SIGKILL before returning, still under the
    /// lock: the leader is unreaped and owned by the caller, so the id
    /// cannot have been reused.
    pub fn list(&mut self, pgid: u32) -> Option<Listing> {
        if refused(pgid) {
            return None;
        }
        let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
        self.guard.entries.push((serial, pgid));
        if self.guard.killed {
            send_group(pgid, Signal::KILL);
        }
        Some(Listing { serial, pgid })
    }

    /// Removes the entry with the token's serial, if still there; nothing
    /// else. The token is consumed, so one registration unlists once.
    pub fn unlist(&mut self, listing: Listing) {
        let serial = listing.serial;
        self.guard.entries.retain(|(listed, _)| *listed != serial);
    }
}

/// Spawns `cmd` and lists the child's group. The caller has already made the
/// child a group leader, through `process_group(0)` or `setsid`. The lock is
/// never held across the fork or the exec: the spawn runs with no lock held,
/// then the new group is listed. A spawn failure is [`Error::Spawn`] and
/// lists nothing.
///
/// A child listed after a shutdown already ran is SIGKILLed at its listing
/// and still returned as `Ok`: the caller's run sees the child die and ends
/// as on any kill.
///
/// # Errors
///
/// When the program cannot start, or the child's id is refused (never for a
/// child the caller just spawned).
pub fn spawn(cmd: &mut Command) -> Result<(Child, Listing), Error> {
    let child = cmd.spawn().map_err(Error::Spawn)?;
    let pgid = child.id();
    match live().list(pgid) {
        Some(listing) => Ok((child, listing)),
        None => Err(Error::Refused(pgid)),
    }
}

/// Makes the child its own session leader, and, for a terminal command,
/// the secondary already on fd 0 its controlling terminal. Called before
/// [`spawn`], so the child the spawn lists is already a group leader.
#[allow(
    unsafe_code,
    reason = "setsid between fork and exec, which CommandExt::pre_exec requires"
)]
pub fn detach(cmd: &mut Command, controlling_tty: bool) {
    // SAFETY: the closure runs in the child between fork and exec, where only
    // async-signal-safe calls are sound. It calls only setsid and, for a
    // terminal, the TIOCSCTTY ioctl on fd 0, both system calls. Neither
    // allocates; the error path builds an `io::Error` from a raw errno, which
    // does not allocate either. The child is single-threaded.
    unsafe {
        cmd.pre_exec(move || {
            rustix::process::setsid().map_err(raw_error)?;
            if controlling_tty {
                // The session has no terminal yet; the secondary, already
                // fd 0, becomes it.
                // SAFETY: fd 0 is open, the secondary `Command` dup'd onto it.
                let stdin = rustix::fd::BorrowedFd::borrow_raw(0);
                rustix::process::ioctl_tiocsctty(stdin).map_err(raw_error)?;
            }
            Ok(())
        });
    }
}

fn raw_error(err: rustix::io::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(err.raw_os_error())
}

/// Sends SIGKILL to every listed group still holding a process, all at once,
/// and drops the empty ones from the list. Under one lock hold it sets the
/// sticky shutdown flag, prunes every entry whose group is not [`alive`],
/// then signals each group left, all before releasing. It never waits beyond
/// taking the lock, and nothing it waits for (the lock) is ever held across
/// a spawn, so it returns within the time of its own syscalls.
///
/// Only call it when no caller holds [`Live`] on the calling thread: the
/// lock is not re-entrant.
pub fn kill_every_group() {
    let mut live = live();
    live.guard.killed = true;
    live.guard.entries.retain(|(_, pgid)| alive(*pgid));
    for (_, pgid) in live.guard.entries.iter() {
        send_group(*pgid, Signal::KILL);
    }
}

/// Sends `signal` to process group `pgid` and drops the result: a group
/// already gone, or one that vanished mid-kill, reads as done. A refused id
/// sends nothing.
fn send_group(pgid: u32, signal: Signal) {
    let Some(pid) = pid_of(pgid) else {
        return;
    };
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) | Err(_) => {}
    }
}

/// The raw pid for an id, or `None` for a refused id or one past what a pid
/// holds: no caller converts an id to a pid any other way.
fn pid_of(raw: u32) -> Option<Pid> {
    if refused(raw) {
        return None;
    }
    Pid::from_raw(i32::try_from(raw).ok()?)
}

/// The signal's name, such as `SIGKILL`.
#[must_use]
pub fn signal_name(number: i32) -> String {
    for (signal, name) in KNOWN {
        if signal.as_raw() == number {
            return (*name).to_owned();
        }
    }
    format!("SIG{number}")
}

/// Every signal Fiber names, with its name.
const KNOWN: &[(Signal, &str)] = &[
    (Signal::HUP, "SIGHUP"),
    (Signal::INT, "SIGINT"),
    (Signal::QUIT, "SIGQUIT"),
    (Signal::ILL, "SIGILL"),
    (Signal::TRAP, "SIGTRAP"),
    (Signal::ABORT, "SIGABRT"),
    (Signal::BUS, "SIGBUS"),
    (Signal::FPE, "SIGFPE"),
    (Signal::KILL, "SIGKILL"),
    (Signal::USR1, "SIGUSR1"),
    (Signal::SEGV, "SIGSEGV"),
    (Signal::USR2, "SIGUSR2"),
    (Signal::PIPE, "SIGPIPE"),
    (Signal::ALARM, "SIGALRM"),
    (Signal::TERM, "SIGTERM"),
    (Signal::CHILD, "SIGCHLD"),
    (Signal::CONT, "SIGCONT"),
    (Signal::STOP, "SIGSTOP"),
    (Signal::TSTP, "SIGTSTP"),
    (Signal::TTIN, "SIGTTIN"),
    (Signal::TTOU, "SIGTTOU"),
    (Signal::URG, "SIGURG"),
    (Signal::XCPU, "SIGXCPU"),
    (Signal::XFSZ, "SIGXFSZ"),
    (Signal::VTALARM, "SIGVTALRM"),
    (Signal::PROF, "SIGPROF"),
    (Signal::WINCH, "SIGWINCH"),
    (Signal::SYS, "SIGSYS"),
];

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
