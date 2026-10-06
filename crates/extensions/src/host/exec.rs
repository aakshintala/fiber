//! `host.exec`'s run (`docs/extensions.md`, "Host calls"): a program in its
//! own process group, stopped on cancel and at shutdown as a tool's is
//! (`docs/tools.md`, "Shell"). Each stream is read whole into its own
//! buffer on its own reader thread, bounded by the extension's memory cap.

use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use rustix::process::{Pid, Signal};

/// SIGKILL follows SIGTERM by this long (`docs/tools.md`, "Stopping a command").
pub(crate) const GRACE: Duration = Duration::from_millis(800);

/// How long output is read after the group is empty or the stop
/// (`docs/tools.md`, "Stopping a command").
pub(crate) const DRAIN: Duration = Duration::from_secs(2);

/// How often the group is re-checked while it may still be occupied.
pub(crate) const GROUP_POLL: Duration = Duration::from_millis(10);

/// What `host.exec` runs.
pub(crate) struct ExecRequest {
    /// The program, resolved on `PATH` as `std::process::Command` does.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Its working directory, already resolved against the workspace.
    pub cwd: PathBuf,
    /// Each stream's bound in bytes: the extension's memory cap.
    pub cap: usize,
}

/// How a run that started ended.
#[derive(Debug, Clone)]
pub(crate) struct Ran {
    /// The exit code, when the process exited.
    pub exit_code: Option<i32>,
    /// The signal's name, when a signal ended the process.
    pub signal: Option<String>,
    /// Its standard output.
    pub stdout: Vec<u8>,
    /// Its standard error.
    pub stderr: Vec<u8>,
    /// Whether the callback's deadline stopped it.
    pub timed_out: bool,
}

/// A run that failed: the message Lua raises, and how the run ended when
/// it started (a spawn failure never ran, so there is nothing to log).
#[derive(Debug)]
pub(crate) struct ExecError {
    /// What `host.exec` raises.
    pub message: String,
    /// How the run ended, when it started.
    pub ran: Option<Ran>,
}

#[derive(Default)]
struct Inner {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_eof: bool,
    stderr_eof: bool,
    reaped: bool,
    status: Option<ExitStatus>,
    seq: u64,
    /// Each stream keeps at most `cap + 1` bytes: one past the cap proves
    /// it fired, and nothing more is retained. Set at spawn from the
    /// extension's memory cap.
    cap: usize,
    /// The run returned: readers drop every later byte instead of
    /// retaining it, so an escaped descendant holding the pipe past the
    /// drain cannot grow Fiber's memory.
    discard: bool,
}

#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
    cv: Condvar,
}

impl contract::clock::Wake for Shared {
    fn wake(&self) {
        self.cv.notify_all();
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

fn bump(inner: &mut Inner, shared: &Shared) {
    inner.seq = inner.seq.wrapping_add(1);
    shared.cv.notify_all();
}

/// Runs `req` until its group is empty, the callback's `deadline`, the
/// `cancel` sender's drop, or the cap. `deadline` is the callback's end on
/// the extension's injected clock; `cancel` is dropped with the callback's
/// end for any other reason (the extension stopped). A program that cannot
/// be spawned never ran: the error names it and nothing is logged. Output
/// past `cap` stops the run and raises the cap error. Every wait reads the
/// injected clock; the two bounds are the shell's: SIGKILL 800 ms after
/// SIGTERM while a member lives, output read at most 2 s more.
pub(crate) fn run(
    req: &ExecRequest,
    clock: &dyn Clock,
    deadline: Option<Instant>,
    cancel: mpsc::Receiver<()>,
) -> Result<Ran, ExecError> {
    let mut cmd = Command::new(&req.program);
    cmd.args(&req.args)
        .current_dir(&req.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Its own process group, stopped as a tool's is. The child's pid is the
    // group id (std, no `unsafe`).
    cmd.process_group(0);
    let mut child = match spawn(&mut cmd) {
        Ok(child) => child,
        Err(source) => {
            return Err(ExecError {
                message: format!("host.exec: {}: {source}", req.program),
                ran: None,
            });
        }
    };
    let pgid = child.id();
    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            cap: req.cap,
            ..Default::default()
        }),
        cv: Condvar::new(),
    });
    clock.subscribe(Arc::downgrade(
        &(Arc::clone(&shared) as Arc<dyn contract::clock::Wake>),
    ));
    // `Stdio::piped()` always yields both streams, so a missing one is a
    // post-spawn failure like a thread that cannot start: the group stops
    // and the run is still logged.
    let (Some(out), Some(err)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(abort_startup(
            req,
            pgid,
            child,
            &shared,
            std::io::Error::other("host.exec pipes missing"),
        ));
    };
    let out_shared = Arc::clone(&shared);
    if let Err(source) = std::thread::Builder::new()
        .name("exec stdout".into())
        .spawn(move || read_into(out, &out_shared, true))
    {
        return Err(abort_startup(req, pgid, child, &shared, source));
    }
    let err_shared = Arc::clone(&shared);
    if let Err(source) = std::thread::Builder::new()
        .name("exec stderr".into())
        .spawn(move || read_into(err, &err_shared, false))
    {
        return Err(abort_startup(req, pgid, child, &shared, source));
    }
    // The waiter takes the child through a handoff, so a spawn failure
    // keeps it here for the stop and the reap instead of dropping it in
    // a closure that never runs.
    let (wait_tx, wait_rx) = mpsc::channel::<Child>();
    let waiter = Arc::clone(&shared);
    if let Err(source) = std::thread::Builder::new()
        .name("exec wait".into())
        .spawn(move || {
            if let Ok(child) = wait_rx.recv() {
                wait_child(child, &waiter);
            }
        })
    {
        return Err(abort_startup(req, pgid, child, &shared, source));
    }
    if let Err(send) = wait_tx.send(child) {
        return Err(abort_startup(
            req,
            pgid,
            send.0,
            &shared,
            std::io::Error::other("host.exec waiter gone"),
        ));
    }

    let mut seen_empty = false;
    let mut sent_term = false;
    let mut timed_out = false;
    let mut capped = false;
    // `None` while running; set when the stop starts.
    let mut kill_at: Option<Instant> = None;
    let mut drain_until: Option<Instant> = None;

    loop {
        let (seen_seq, out_len, err_len, out_eof, err_eof, reaped, status) = {
            let inner = lock(&shared.inner);
            (
                inner.seq,
                inner.stdout.len(),
                inner.stderr.len(),
                inner.stdout_eof,
                inner.stderr_eof,
                inner.reaped,
                inner.status,
            )
        };
        if reaped && !group_alive(pgid) {
            seen_empty = true;
        }
        let eof = out_eof && err_eof;
        let now = clock.now();
        let cancelled = matches!(cancel.try_recv(), Err(mpsc::TryRecvError::Disconnected));
        let deadline_due = deadline.is_some_and(|at| now >= at);

        // The cap fires once, on the first pass past it. No signal to a
        // group already seen empty: its id may be reused.
        if !capped && (out_len > req.cap || err_len > req.cap) {
            capped = true;
            sent_term = send_term(pgid, sent_term, seen_empty);
            kill_at = Some(add(now, GRACE));
        }
        // A stop starts once: the cap, the deadline, or the drop.
        if kill_at.is_none() && (deadline_due || cancelled) {
            timed_out = deadline_due;
            sent_term = send_term(pgid, sent_term, seen_empty);
            kill_at = Some(add(now, GRACE));
        }

        if let Some(kill_at) = kill_at {
            if seen_empty {
                if drain_until.is_none() {
                    drain_until = Some(add(clock.now(), DRAIN));
                }
            } else if clock.now() >= kill_at {
                signal_group(pgid, Signal::KILL);
                if drain_until.is_none() {
                    drain_until = Some(add(clock.now(), DRAIN));
                }
                // One SIGKILL: fall through to the drain below.
            } else {
                park(clock, &shared, seen_seq, Some(kill_at));
                continue;
            }
        } else if seen_empty && eof {
            // A quiet end: no stop, no drain. Nothing more is retained:
            // an escaped descendant holding the pipe past here is dropped.
            let ran = ran_of(&shared, status, false);
            lock(&shared.inner).discard = true;
            finished(pgid, seen_empty);
            return Ok(ran);
        } else {
            // Running: the deadline may stop it, and the group is polled.
            // Always a concrete `until` so a test can wait for the park.
            let poll = add(clock.now(), GROUP_POLL);
            let until = Some(deadline.map_or(poll, |d| d.min(poll)));
            park(clock, &shared, seen_seq, until);
            continue;
        }

        // Draining after a stop: every byte until the bound, then what was read.
        let until = drain_until.unwrap_or_else(|| add(clock.now(), DRAIN));
        if (eof && seen_empty) || clock.now() >= until {
            if capped {
                let ran = ran_of(&shared, status, timed_out);
                lock(&shared.inner).discard = true;
                finished(pgid, seen_empty);
                return Err(ExecError {
                    message: format!(
                        "host.exec: {}: output passed the extension's memory cap of {} bytes",
                        req.program, req.cap
                    ),
                    ran: Some(ran),
                });
            }
            let ran = ran_of(&shared, status, timed_out);
            lock(&shared.inner).discard = true;
            finished(pgid, seen_empty);
            return Ok(ran);
        }
        park(clock, &shared, seen_seq, Some(until));
    }
}

/// Stops a group that started but whose readers or waiter never did: the
/// stop sequence, then the reap. The run started, so its end is still
/// logged; the call raises the spawn error.
fn abort_startup(
    req: &ExecRequest,
    pgid: u32,
    mut child: Child,
    shared: &Shared,
    source: std::io::Error,
) -> ExecError {
    signal_group(pgid, Signal::TERM);
    signal_group(pgid, Signal::KILL);
    let status = child.wait().ok();
    {
        let mut inner = lock(&shared.inner);
        inner.discard = true;
        if !inner.reaped {
            inner.reaped = true;
            inner.status = status;
        }
    }
    let ran = ran_of(shared, lock(&shared.inner).status, false);
    finished(pgid, true);
    ExecError {
        message: format!("host.exec: {}: {source}", req.program),
        ran: Some(ran),
    }
}

/// Sends SIGTERM once, never to a group already seen empty: its id may be
/// reused by an unrelated process.
fn send_term(pgid: u32, sent: bool, seen_empty: bool) -> bool {
    if sent || seen_empty {
        return sent;
    }
    signal_group(pgid, Signal::TERM);
    true
}

fn ran_of(shared: &Shared, status: Option<ExitStatus>, timed_out: bool) -> Ran {
    let (exit_code, signal) = match status {
        Some(status) => match (status.code(), status.signal()) {
            (Some(code), _) => (Some(code), None),
            (None, Some(number)) => (None, Some(signal_name(number))),
            (None, None) => (None, None),
        },
        None => (None, None),
    };
    let inner = lock(&shared.inner);
    Ran {
        exit_code,
        signal,
        stdout: inner.stdout.clone(),
        stderr: inner.stderr.clone(),
        timed_out,
    }
}

/// Parks until the shared state moves or `until` on the injected clock.
/// The condvar always times out at `GROUP_POLL`: a clock move whose wake
/// lands between the register and the wait still surfaces within one poll.
fn park(clock: &dyn Clock, shared: &Shared, seen: u64, until: Option<Instant>) {
    let mut slot = Some(lock(&shared.inner));
    clock.wait_until(until, &mut |bound| {
        let Some(guard) = slot.take() else {
            return;
        };
        let timeout = bound.map_or(GROUP_POLL, |b| b.min(GROUP_POLL));
        if guard.seq != seen {
            slot = Some(guard);
            return;
        }
        slot = Some(
            shared
                .cv
                .wait_timeout(guard, timeout)
                .unwrap_or_else(PoisonError::into_inner)
                .0,
        );
    });
}

fn read_into(mut read: impl Read, shared: &Shared, stdout: bool) {
    let mut buf = [0_u8; 8192];
    loop {
        match read.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let Some(bytes) = buf.get(..n) else {
                    break;
                };
                let mut inner = lock(&shared.inner);
                // Past the drain nothing is retained; past the cap only one
                // byte more is kept to prove it fired. The read continues
                // so the program never blocks on the pipe.
                if !inner.discard {
                    let len = if stdout {
                        inner.stdout.len()
                    } else {
                        inner.stderr.len()
                    };
                    let room = inner.cap.saturating_add(1).saturating_sub(len);
                    if room > 0 {
                        let kept = bytes.get(..room.min(bytes.len())).unwrap_or(&[]);
                        if stdout {
                            inner.stdout.extend_from_slice(kept);
                        } else {
                            inner.stderr.extend_from_slice(kept);
                        }
                    }
                }
                bump(&mut inner, shared);
            }
            Err(_) => break,
        }
    }
    let mut inner = lock(&shared.inner);
    if stdout {
        inner.stdout_eof = true;
    } else {
        inner.stderr_eof = true;
    }
    bump(&mut inner, shared);
}

fn wait_child(mut child: Child, shared: &Shared) {
    let status = child.wait().ok();
    let mut inner = lock(&shared.inner);
    inner.reaped = true;
    inner.status = status;
    bump(&mut inner, shared);
}

fn add(now: Instant, delay: Duration) -> Instant {
    now.checked_add(delay).unwrap_or(now)
}

/// Whether the group still holds a process. A group id of 1 or less is
/// never one of ours.
pub(crate) fn group_alive(pgid: u32) -> bool {
    if refused_group(pgid) {
        return false;
    }
    pid(pgid).is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

fn signal_group(pgid: u32, signal: Signal) {
    if refused_group(pgid) {
        return;
    }
    let Some(pid) = pid(pgid) else {
        return;
    };
    match rustix::process::kill_process_group(pid, signal) {
        Ok(()) | Err(_) => {}
    }
}

/// Group 1 or 0 is not a command's group: `kill(-1)` reaches every process
/// the user owns, and `kill(0)` this process's own group.
pub(crate) fn refused_group(pgid: u32) -> bool {
    pgid <= 1
}

fn pid(raw: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(raw).ok()?)
}

/// The signal's name, such as `SIGKILL`.
pub(crate) fn signal_name(number: i32) -> String {
    for (signal, name) in KNOWN {
        if signal.as_raw() == number {
            return (*name).to_owned();
        }
    }
    format!("SIG{number}")
}

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

mod groups {
    //! Every `host.exec` group that may still hold a process, listed for the
    //! whole process (`docs/invocation.md`, "Shutdown").

    use std::io;
    use std::process::{Child, Command};
    use std::sync::{Mutex, MutexGuard, PoisonError};

    use rustix::process::Signal;

    use super::{group_alive, signal_group};

    static LIVE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

    fn live() -> MutexGuard<'static, Vec<u32>> {
        LIVE.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn spawn(cmd: &mut Command) -> io::Result<Child> {
        let mut live = live();
        let child = cmd.spawn()?;
        live.push(child.id());
        Ok(child)
    }

    /// Lists `pgid`, a group spawned outside [`spawn`].
    #[cfg(test)]
    pub(super) fn register(pgid: u32) {
        live().push(pgid);
    }

    pub(crate) fn finished(pgid: u32, seen_empty: bool) {
        if seen_empty {
            live().retain(|listed| *listed != pgid);
        }
    }

    /// Sends SIGKILL to every listed group still holding a process, all at
    /// once, and drops the empty ones from the list.
    pub(crate) fn kill_every_group() {
        let mut live = live();
        live.retain(|pgid| group_alive(*pgid));
        for pgid in live.iter() {
            signal_group(*pgid, Signal::KILL);
        }
    }
}

pub(crate) use groups::finished;

/// Sends SIGKILL to every listed `host.exec` group still holding a process.
pub fn kill_every_group() {
    groups::kill_every_group()
}

use groups::spawn;

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
