//! `host.exec`'s run (`docs/extensions.md`, "Host calls"): a program in its
//! own process group, stopped on cancel and at shutdown as a tool's is
//! (`docs/tools.md`, "Shell"). Each stream is read whole into its own
//! buffer on its own reader thread, bounded by the extension's memory cap.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use contract::clock::Clock;
use rustix::process::Signal;
use support::group::Listing;

/// What `host.exec` runs.
pub(crate) struct ExecRequest {
    /// The program: a bare name resolves on `PATH` as
    /// `std::process::Command` does; a Lua `host.exec` program with a `/`
    /// was resolved against the extension directory before it got here.
    pub program: String,
    /// Its arguments.
    pub args: Vec<String>,
    /// Its working directory, already resolved against the workspace.
    pub cwd: PathBuf,
    /// Variables added to the inherited environment, overriding a
    /// same-named variable; empty keeps today's behaviour.
    pub env: BTreeMap<String, String>,
    /// Bytes piped to the child's standard input, then EOF (`None` keeps
    /// today's null stdin; `Some` with no bytes streams empty then EOF).
    pub input: Option<Vec<u8>>,
    /// Each stream's bound in bytes: the extension's memory cap.
    pub cap: usize,
    /// Whether the program runs in its own process group. `host.exec`
    /// keeps `true`. A caller that must stay in Fiber's process group, so
    /// terminal prompts and Ctrl-C still reach the program, passes `false`:
    /// the program is never listed, and only its pid is ever signalled.
    pub own_group: bool,
    /// Test acknowledgement after the stdout reader retains its first bytes.
    #[cfg(test)]
    pub stdout_read: Option<mpsc::Sender<()>>,
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

/// A run that failed: its code and message, which `host.exec` raises as
/// `{ code, message }`, and how the run ended when it started (a spawn
/// failure never ran, so there is nothing to log).
#[derive(Debug)]
pub(crate) struct ExecError {
    /// The failure's code: `not_found` when the program or `cwd` does not
    /// exist, `too_large` past the memory cap, else `io_failed`.
    pub code: contract::ErrorCode,
    /// What `host.exec` raises.
    pub message: String,
    /// How the run ended, when it started.
    pub ran: Option<Ran>,
    /// The spawn failure, set only when the program never ran (`ran` is
    /// `None`). A failure after the spawn leaves it `None`.
    pub source: Option<std::io::Error>,
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
    // Latency only: `park` never blocks past `support::group::GROUP_POLL`,
    // so a clock move this misses is seen on the next poll with the same
    // outcome, and no test can tell the difference without reading the
    // real clock.
    #[cfg_attr(false, mutants::skip)]
    fn wake(&self) {
        self.cv.notify_all();
    }
}

fn lock(inner: &Mutex<Inner>) -> MutexGuard<'_, Inner> {
    inner.lock().unwrap_or_else(PoisonError::into_inner)
}

// Latency only: `park` never blocks past `support::group::GROUP_POLL`,
// so a change this does not announce is seen on the next poll with the
// same outcome, and no test can tell the difference without reading the
// real clock.
#[cfg_attr(false, mutants::skip)]
fn bump(inner: &mut Inner, shared: &Shared) {
    inner.seq = inner.seq.wrapping_add(1);
    shared.cv.notify_all();
}

/// Resolves a Lua `host.exec` `program` against the extension's directory
/// `package`, which is canonical. A bare name resolves on `PATH` and an
/// absolute path is used as written, so both return unchanged. Otherwise the
/// program joins onto `package`, folding `.` and `..` lexically: a `..` that
/// pops past `package` leaves the directory. The deepest existing ancestor
/// of the folded path is canonicalized and must lie under `package`, and
/// every component below `package` that exists must not be a symlink out:
/// one whose target is missing or lies outside `package` leaves the
/// directory too. A path that stays inside but does not exist yet resolves,
/// and the spawn reports it missing. Filesystem reads only.
///
/// Example: `resolve_in_package("bin/acme", "/h/extensions/acme")` is
/// `Ok("/h/extensions/acme/bin/acme")`.
// debt: exec.rs over 800 lines after this change, upgrade when the next change to exec.rs lands: move resolve_in_package to its own module.
pub(crate) fn resolve_in_package(program: &str, package: &Path) -> Result<String, String> {
    if !program.contains('/') || program.starts_with('/') {
        return Ok(program.to_owned());
    }
    let refused = || {
        format!(
            "host.exec: {program} leaves the extension directory {}",
            package.display()
        )
    };
    let package_len = package.components().count();
    let mut folded = PathBuf::new();
    let mut len = 0usize;
    for component in package.join(program).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if len <= package_len {
                    return Err(refused());
                }
                folded.pop();
                len -= 1;
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                folded.push(component.as_os_str());
                len += 1;
            }
        }
    }
    let mut ancestor = folded.clone();
    let canonical = loop {
        match ancestor.canonicalize() {
            Ok(canonical) => break canonical,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                if !ancestor.pop() {
                    return Err(refused());
                }
            }
            Err(_) => return Err(refused()),
        }
    };
    if !canonical.starts_with(package) {
        return Err(refused());
    }
    let mut prefix = PathBuf::new();
    let mut depth = 0usize;
    for component in folded.components() {
        prefix.push(component.as_os_str());
        depth += 1;
        if depth <= package_len {
            continue;
        }
        // Only the missing tail ends the walk: the ancestor check above
        // already refused every other failure (a loop, a permission
        // error, a path through a file), so any error here is the tail
        // not existing yet. Any other filesystem failure below the
        // package would have failed that check first, absent a
        // concurrent change to the directory.
        let Ok(meta) = std::fs::symlink_metadata(&prefix) else {
            break;
        };
        if meta.file_type().is_symlink() {
            // A dangling target cannot be shown inside, and neither
            // can a loop: any failure to resolve is a refusal.
            let escapes = match prefix.canonicalize() {
                Ok(target) => !target.starts_with(package),
                Err(_) => true,
            };
            if escapes {
                return Err(refused());
            }
        }
    }
    folded.into_os_string().into_string().map_err(|_| refused())
}

/// Runs `req` until its group is empty, the callback's `deadline`, the
/// `cancel` sender's drop, or the cap. `deadline` is the callback's end on
/// the extension's injected clock; `cancel` is dropped with the callback's
/// end for any other reason (the extension stopped). A program that cannot
/// be spawned never ran: the error names it and nothing is logged. Output
/// past `cap` stops the run and raises the cap error. Every wait reads the
/// injected clock; the two bounds are the shell's: SIGKILL 800 ms after
/// SIGTERM while a member lives, output read at most 2 s more. With
/// `own_group` the program runs in its own listed group, stopped as a
/// tool's is; without it the program stays in this process's group, no
/// waiter thread starts, and the stop signals only its pid.
pub(crate) fn run(
    req: &ExecRequest,
    clock: &dyn Clock,
    deadline: Option<Instant>,
    cancel: mpsc::Receiver<()>,
) -> Result<Ran, Box<ExecError>> {
    let mut cmd = Command::new(&req.program);
    cmd.args(&req.args)
        .current_dir(&req.cwd)
        .envs(&req.env)
        // Piped stdin carries `input`, then EOF; null stdin is today's
        // behaviour when no input was given.
        .stdin(if req.input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if req.own_group {
        // Its own process group, stopped as a tool's is. The child's pid is the
        // group id (std, no `unsafe`). Without its own group the program stays
        // in this process's group, so terminal prompts and Ctrl-C reach it.
        cmd.process_group(0);
    }
    let (mut child, mut listing): (Child, Option<Listing>) = if req.own_group {
        // Its own listed group, stopped as a tool's is and killed at
        // shutdown with every other listed group.
        match support::group::spawn(&mut cmd) {
            Ok((child, listing)) => (child, Some(listing)),
            Err(error) => return Err(spawn_error(req, error)),
        }
    } else {
        match cmd.spawn() {
            Ok(child) => (child, None),
            Err(source) => return Err(spawn_failed(req, source)),
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
            listing.take(),
            child,
            &shared,
            clock,
            [false, false],
            std::io::Error::other("host.exec pipes missing"),
        ));
    };
    let out_shared = Arc::clone(&shared);
    #[cfg(test)]
    let stdout_read = req.stdout_read.clone();
    if let Err(source) = std::thread::Builder::new()
        .name("exec stdout".into())
        .spawn(move || {
            #[cfg(test)]
            read_into(out, &out_shared, true, stdout_read);
            #[cfg(not(test))]
            read_into(out, &out_shared, true);
        })
    {
        return Err(abort_startup(
            req,
            listing.take(),
            child,
            &shared,
            clock,
            [false, false],
            source,
        ));
    }
    let err_shared = Arc::clone(&shared);
    if let Err(source) = std::thread::Builder::new()
        .name("exec stderr".into())
        .spawn(move || {
            #[cfg(test)]
            read_into(err, &err_shared, false, None);
            #[cfg(not(test))]
            read_into(err, &err_shared, false);
        })
    {
        return Err(abort_startup(
            req,
            listing.take(),
            child,
            &shared,
            clock,
            [true, false],
            source,
        ));
    }
    // The stdin writer starts after the readers and before the waiter
    // handoff below moves the child: only the waiter arms and the two
    // supervising tails below own its teardown.
    let stdin_stop = match start_stdin_writer(req, &mut child) {
        Ok(writer) => writer,
        Err(source) => {
            return Err(abort_startup(
                req,
                listing.take(),
                child,
                &shared,
                clock,
                [true, true],
                source,
            ));
        }
    };
    if req.own_group {
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
            drop(stdin_stop);
            return Err(abort_startup(
                req,
                listing.take(),
                child,
                &shared,
                clock,
                [true, true],
                source,
            ));
        }
        if let Err(send) = wait_tx.send(child) {
            drop(stdin_stop);
            return Err(abort_startup(
                req,
                listing.take(),
                send.0,
                &shared,
                clock,
                [true, true],
                std::io::Error::other("host.exec waiter gone"),
            ));
        }
        // The stdin writer started before the handoff above moved the
        // child, so only the two supervising tails below own its teardown.
        let outcome = supervise(
            req,
            &shared,
            clock,
            deadline,
            &cancel,
            &mut || {},
            StopTarget::Group(pgid),
            listing.take(),
        );
        drop(stdin_stop);
        return outcome;
    }
    // Without its own group no waiter thread starts: the supervising thread
    // below reaps the child itself, so a reaped pid, which the system may
    // reuse, is never signalled.
    let mut child = child;
    // The child stays here in pid mode, but the writer started above, so
    // this tail owns its teardown as the group tail does.
    let outcome = supervise(
        req,
        &shared,
        clock,
        deadline,
        &cancel,
        &mut || {},
        StopTarget::Pid(&mut child),
        None,
    );
    // The child is reaped, or dead or dying after the stop: `wait` returns
    // at once then, instead of leaving a zombie.
    let _reaped = child.wait();
    drop(stdin_stop);
    outcome
}

/// A spawn that never ran names its failure: `NotFound` for a missing
/// program or working directory, anything else for a program that could
/// not start.
fn spawn_failed(req: &ExecRequest, source: std::io::Error) -> Box<ExecError> {
    let code = if source.kind() == std::io::ErrorKind::NotFound {
        contract::ErrorCode::NotFound
    } else {
        contract::ErrorCode::IoFailed
    };
    Box::new(ExecError {
        code,
        message: format!("host.exec: {}: {source}", req.program),
        ran: None,
        source: Some(source),
    })
}

/// A group spawn that never ran names its failure: `NotFound` when the
/// program or working directory does not exist, `io_failed` for anything
/// else. A refused id or a signal failure never reaches here: the child
/// just spawned, so its id is a real pid, and listing a live group sends
/// no signal.
fn spawn_error(req: &ExecRequest, error: support::group::Error) -> Box<ExecError> {
    match error {
        support::group::Error::Spawn(source) => spawn_failed(req, source),
        refused @ support::group::Error::Refused(_) => no_spawn(req, &refused),
        signal @ support::group::Error::Signal { .. } => no_spawn(req, &signal),
    }
}

/// A group error no spawn returns, reported as `io_failed` with nothing
/// logged: the program never ran.
fn no_spawn(req: &ExecRequest, error: &dyn std::fmt::Display) -> Box<ExecError> {
    Box::new(ExecError {
        code: contract::ErrorCode::IoFailed,
        message: format!("host.exec: {}: {error}", req.program),
        ran: None,
        source: None,
    })
}

/// What a stop signals: the listed group, or the pid alone when the run
/// stays in this process's group.
enum StopTarget<'a> {
    Group(u32),
    Pid(&'a mut Child),
}

impl StopTarget<'_> {
    fn pgid(&self) -> u32 {
        match self {
            StopTarget::Group(pgid) => *pgid,
            StopTarget::Pid(child) => child.id(),
        }
    }
}

/// Unlists the run's registration once its group is seen empty: a pid-mode
/// run holds no token, so it never unlists another registration of a
/// reused id.
fn unlist(listing: &mut Option<Listing>, seen_empty: bool) {
    if seen_empty && let Some(registration) = listing.take() {
        support::group::live().unlist(registration);
    }
}

/// Watches a started run until its group is empty and its output read,
/// stopping it at the cap, the `deadline` or the `cancel` sender's drop:
/// SIGTERM, SIGKILL [`support::group::GRACE`] later while a member lives,
/// then output read for at most [`support::group::DRAIN`] more. `reap` runs
/// first on every pass, for a caller
/// that reaps the child itself instead of a waiter thread. A [`StopTarget::Pid`]
/// run reaps the child on this thread and signals only its pid, never a
/// group; a [`StopTarget::Group`] run leaves the reap to its waiter thread.
#[allow(
    clippy::too_many_arguments,
    reason = "the run's inputs and its listing token are one call"
)]
fn supervise(
    req: &ExecRequest,
    shared: &Shared,
    clock: &dyn Clock,
    deadline: Option<Instant>,
    cancel: &mpsc::Receiver<()>,
    reap: &mut dyn FnMut(),
    target: StopTarget<'_>,
    mut listing: Option<Listing>,
) -> Result<Ran, Box<ExecError>> {
    let pgid = target.pgid();
    // Without its own group the child is this process's member, so its reap
    // alone proves the run ended: a helper it started may outlive it and is
    // never waited on.
    let mut pid_child = match target {
        StopTarget::Pid(child) => Some(child),
        StopTarget::Group(_) => None,
    };
    let pid_mode = pid_child.is_some();
    let mut seen_empty = false;
    let mut timed_out = false;
    let mut capped = false;
    // `None` while running; set when the stop starts.
    let mut kill_at: Option<Instant> = None;
    let mut drain_until: Option<Instant> = None;

    loop {
        reap();
        if let Some(child) = pid_child.as_deref_mut() {
            reap_now(child, shared);
        }
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
        if reaped && (pid_mode || !support::group::alive(pgid)) {
            seen_empty = true;
        }
        let eof = out_eof && err_eof;
        let now = clock.now();
        let cancelled = matches!(cancel.try_recv(), Err(mpsc::TryRecvError::Disconnected));
        let deadline_due = deadline.is_some_and(|at| now >= at);
        if out_len > req.cap || err_len > req.cap {
            capped = true;
        }
        if pid_mode && reaped && kill_at.is_none() && drain_until.is_none() {
            // The program ended but a helper holding the pipe may never
            // close it, so EOF may never come: the run ends at the drain
            // bound with the child's own status, no timeout.
            drain_until = Some(add(now, support::group::DRAIN));
        }
        // A stop starts once: the cap, the deadline, or the drop. No signal
        // to a group already seen empty: its id may be reused. Once the
        // reap starts draining, the run ends at the drain bound with the
        // child's own status, so the deadline never starts a stop after that.
        let stop_due = capped || cancelled || (deadline_due && drain_until.is_none());
        if kill_at.is_none() && stop_due {
            timed_out = deadline_due;
            if !seen_empty {
                if let Some(child) = pid_child.as_deref_mut() {
                    stop_pid(child, shared, Signal::TERM);
                } else {
                    match support::group::signal(pgid, Signal::TERM) {
                        Ok(()) | Err(_) => {}
                    }
                }
            }
            kill_at = Some(add(now, support::group::GRACE));
        }

        if let Some(kill_at) = kill_at {
            if seen_empty {
                if drain_until.is_none() {
                    drain_until = Some(add(clock.now(), support::group::DRAIN));
                }
            } else if clock.now() >= kill_at {
                // Unreaped, the child holds the id; reaped, a member was
                // alive just above (`tools` accepts the same window).
                if let Some(child) = pid_child.as_deref_mut() {
                    stop_pid(child, shared, Signal::KILL);
                } else {
                    match support::group::signal(pgid, Signal::KILL) {
                        Ok(()) | Err(_) => {}
                    }
                }
                if drain_until.is_none() {
                    drain_until = Some(add(clock.now(), support::group::DRAIN));
                }
                // One SIGKILL: fall through to the drain below.
            } else {
                park(clock, shared, seen_seq, Some(kill_at));
                continue;
            }
        } else if seen_empty && eof {
            // A quiet end: no stop, no drain. Nothing more is retained:
            // an escaped descendant holding the pipe past here is dropped.
            let ran = ran_of(shared, status, false);
            lock(&shared.inner).discard = true;
            unlist(&mut listing, seen_empty);
            return Ok(ran);
        } else if pid_mode && reaped {
            // The program ended behind a helper holding the pipe: EOF may
            // never come, so the run ends at the drain bound above with the
            // child's own status and no timeout.
            let until = drain_until.unwrap_or_else(|| add(clock.now(), support::group::DRAIN));
            if clock.now() >= until {
                let ran = ran_of(shared, status, false);
                lock(&shared.inner).discard = true;
                unlist(&mut listing, seen_empty);
                return Ok(ran);
            }
            park(clock, shared, seen_seq, Some(until));
            continue;
        } else {
            // Running: the deadline may stop it, and the group is polled.
            // Always a concrete `until` so a test can wait for the park.
            let poll = add(clock.now(), support::group::GROUP_POLL);
            let until = Some(deadline.map_or(poll, |d| d.min(poll)));
            park(clock, shared, seen_seq, until);
            continue;
        }

        // Draining after a stop: every byte until the bound, then what was read.
        let until = drain_until.unwrap_or_else(|| add(clock.now(), support::group::DRAIN));
        if (eof && seen_empty) || clock.now() >= until {
            let ran = ran_of(shared, status, timed_out);
            lock(&shared.inner).discard = true;
            unlist(&mut listing, seen_empty);
            if capped {
                return Err(Box::new(ExecError {
                    code: contract::ErrorCode::TooLarge,
                    message: format!(
                        "host.exec: {}: output passed the extension's memory cap of {} bytes",
                        req.program, req.cap
                    ),
                    ran: Some(ran),
                    source: None,
                }));
            }
            return Ok(ran);
        }
        park(clock, shared, seen_seq, Some(until));
    }
}

/// Stops a group that started but whose readers or waiter never did, by
/// [`supervise`]'s stop sequence, reaping the child itself. `reading` names
/// the streams whose reader thread started; a stream with none never
/// reaches EOF. The run started, so its end is still logged; the call raises
/// the startup error.
fn abort_startup(
    req: &ExecRequest,
    listing: Option<Listing>,
    mut child: Child,
    shared: &Shared,
    clock: &dyn Clock,
    reading: [bool; 2],
    source: std::io::Error,
) -> Box<ExecError> {
    {
        let mut inner = lock(&shared.inner);
        let [out, err] = reading;
        inner.stdout_eof |= !out;
        inner.stderr_eof |= !err;
    }
    // A dropped sender: the stop starts on the first pass. Without its own
    // group the stop signals only the pid, through the same reap the run
    // itself uses.
    let (_, stopped) = mpsc::channel::<()>();
    let ended = if req.own_group {
        let pgid = listing
            .as_ref()
            .map(Listing::pgid)
            .unwrap_or_else(|| child.id());
        supervise(
            req,
            shared,
            clock,
            None,
            &stopped,
            &mut || {
                reap_now(&mut child, shared);
            },
            StopTarget::Group(pgid),
            listing,
        )
    } else {
        supervise(
            req,
            shared,
            clock,
            None,
            &stopped,
            &mut || {},
            StopTarget::Pid(&mut child),
            None,
        )
    };
    // The drain after a SIGKILL may end before the reap; the child is dead
    // or dying, and a reaped one answers at once. Its status then completes
    // the snapshot `supervise` took.
    let status = child.wait().ok();
    let ran = match ended {
        Ok(ran) => Some(ran),
        Err(capped) => capped.ran,
    }
    .map(|ran| completed(ran, status));
    Box::new(ExecError {
        code: contract::ErrorCode::IoFailed,
        message: format!("host.exec: {}: {source}", req.program),
        ran,
        // After the spawn: a reader or waiter thread that could not start.
        source: None,
    })
}

/// Signals `child` with `signal` when it is still running, reaping it when
/// it already ended but is not yet reaped. Returns whether it signalled.
/// Only the supervising thread reaps the child, through `try_wait` here and
/// in [`supervise`]'s pass, and a signal is sent only after a `try_wait` on
/// this same thread returned `None`: a reaped pid, which the system may
/// reuse, is never signalled.
fn stop_pid(child: &mut Child, shared: &Shared, signal: Signal) -> bool {
    match child.try_wait() {
        Ok(None) => {}
        Ok(Some(status)) => {
            let mut inner = lock(&shared.inner);
            inner.reaped = true;
            inner.status = Some(status);
            return false;
        }
        Err(_) => {
            lock(&shared.inner).reaped = true;
            return false;
        }
    }
    // Still running, and this thread alone reaps it, so the pid cannot be
    // reused under the signal. A refused id, or one already gone, sends
    // nothing.
    support::group::signal_process(child.id(), signal).is_ok()
}

/// Records the child's end once it has one; an error means nothing is left
/// to reap, so there is no status to report.
fn reap_now(child: &mut Child, shared: &Shared) {
    let status = match child.try_wait() {
        Ok(None) => return,
        Ok(Some(status)) => Some(status),
        Err(_) => None,
    };
    let mut inner = lock(&shared.inner);
    inner.reaped = true;
    inner.status = status;
}

/// A run snapshot with no end yet takes the end `status` reports.
fn completed(mut ran: Ran, status: Option<ExitStatus>) -> Ran {
    if ran.exit_code.is_none() && ran.signal.is_none() {
        (ran.exit_code, ran.signal) = ended(status);
    }
    ran
}

/// How `status` ended: its exit code, or the name of the signal that ended it.
fn ended(status: Option<ExitStatus>) -> (Option<i32>, Option<String>) {
    match status {
        Some(status) => match (status.code(), status.signal()) {
            (Some(code), _) => (Some(code), None),
            (None, Some(number)) => (None, Some(support::group::signal_name(number))),
            (None, None) => (None, None),
        },
        None => (None, None),
    }
}

fn ran_of(shared: &Shared, status: Option<ExitStatus>, timed_out: bool) -> Ran {
    let (exit_code, signal) = ended(status);
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
/// The condvar always times out at `support::group::GROUP_POLL`: a clock
/// move whose wake lands between the register and the wait still surfaces
/// within one poll.
// Latency only: every caller re-reads the state and the clock after it
// returns, and it never blocks past `support::group::GROUP_POLL`, so a
// mutant that waits too long or not at all changes when a pass runs,
// never what it decides.
#[cfg_attr(false, mutants::skip)]
fn park(clock: &dyn Clock, shared: &Shared, seen: u64, until: Option<Instant>) {
    // The guard is held from the check into the condvar wait, so a wake
    // under the same lock is never missed.
    let _guard = support::clock::park(
        clock,
        until,
        Some(support::group::GROUP_POLL),
        &shared.cv,
        lock(&shared.inner),
        |guard| guard.seq != seen,
    );
}

/// Starts the thread streaming `req`'s input to the child's stdin, then
/// closing it (EOF). `None` when no input was given: stdin stays null.
/// The writer owns the child's stdin and touches no shared state. The
/// returned sender stops it: dropping it tells the writer the run ended,
/// and the run never waits for that exit — every loop iteration in
/// `write_input` checks the sender first and the poll is bounded by
/// `GROUP_POLL`, so the writer stops on its own within one poll. A
/// thread that cannot start is a post-spawn failure like a reader that
/// cannot start: the group stops and the run is still logged.
fn start_stdin_writer(
    req: &ExecRequest,
    child: &mut Child,
) -> Result<Option<mpsc::Sender<()>>, std::io::Error> {
    let Some(input) = req.input.clone() else {
        return Ok(None);
    };
    let Some(stdin) = child.stdin.take() else {
        return Err(std::io::Error::other("host.exec pipes missing"));
    };
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    match std::thread::Builder::new()
        .name("exec stdin".into())
        .spawn(move || {
            write_input(stdin, &input, &stop_rx);
        }) {
        Ok(_) => Ok(Some(stop_tx)),
        Err(source) => {
            // The closure never ran: dropping it closes stdin (EOF), and
            // no writer exists, so the run's teardown waits on nothing.
            Err(source)
        }
    }
}

/// Streams `input` to `stdin`, then closes it (EOF). The pipe is
/// non-blocking, so a full pipe never parks the writer: the loop polls
/// writable and stops when the input is written, the pipe breaks, or the
/// run ends (`stop` drops). Bytes already written are kept; a broken pipe
/// just ends the write, since the child is gone.
fn write_input(stdin: std::process::ChildStdin, input: &[u8], stop: &mpsc::Receiver<()>) {
    use std::os::fd::AsFd as _;
    let fd = stdin.as_fd();
    if rustix::fs::fcntl_setfl(fd, rustix::fs::OFlags::NONBLOCK).is_err() {
        return;
    }
    let mut remaining = input;
    while !remaining.is_empty() {
        if matches!(
            stop.try_recv(),
            Ok(()) | Err(mpsc::TryRecvError::Disconnected)
        ) {
            return;
        }
        let slice = support::group::GROUP_POLL;
        let timeout = rustix::event::Timespec::try_from(slice).unwrap_or_default();
        let mut fds = [rustix::event::PollFd::new(
            &fd,
            rustix::event::PollFlags::OUT,
        )];
        match rustix::event::poll(&mut fds, Some(&timeout)) {
            Ok(0) => {}
            Ok(_) => match rustix::io::write(fd, remaining) {
                Ok(0) => return,
                Ok(n) => {
                    let Some(next) = remaining.get(n..) else {
                        return;
                    };
                    remaining = next;
                }
                Err(rustix::io::Errno::AGAIN) => {}
                Err(_) => return,
            },
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return,
        }
    }
}

fn read_into(
    mut read: impl Read,
    shared: &Shared,
    stdout: bool,
    #[cfg(test)] mut stdout_read: Option<mpsc::Sender<()>>,
) {
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
                    // No room keeps an empty slice.
                    let room = inner.cap.saturating_add(1).saturating_sub(len);
                    let kept = bytes.get(..room.min(bytes.len())).unwrap_or(&[]);
                    if stdout {
                        inner.stdout.extend_from_slice(kept);
                    } else {
                        inner.stderr.extend_from_slice(kept);
                    }
                }
                bump(&mut inner, shared);
                drop(inner);
                #[cfg(test)]
                if stdout && let Some(ack) = stdout_read.take() {
                    let _sent = ack.send(());
                }
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

#[cfg(test)]
#[path = "exec_tests.rs"]
mod tests;
