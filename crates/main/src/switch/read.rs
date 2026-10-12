//! A session's credential and worktree reads (`docs/configuration.md`,
//! "Secrets"): a switch reads on its own thread (`Reads::run`), a
//! `command` credential source reads under its `timeout`
//! (`Reads::credential`), and shutdown ends every wait at once and kills
//! every group a read started (`docs/invocation.md`, "Shutdown").

use std::collections::BTreeMap;
use std::io;
use std::io::Read as _;
use std::os::unix::process::CommandExt as _;
use std::process::{Child, Command, Output};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use contract::ErrorCode;
use contract::clock::{Clock, Wake};
use contract::shapes::Failure;
use doors::failure;
use rustix::process::{Pid, Signal};
use support::clock::Parker;

/// Why a read ends once shutdown has started.
const CLOSING: &str = "The session is shutting down.";

/// The reads of one session: whether shutdown cancelled them, who waits on
/// one, and the process group of every credential command they started.
/// The session's worktree creation runs its `git worktree add` through a
/// `Reads` of its own, so a signal kills git and its hook together. Every
/// credential command a session process starts runs through the same
/// `Reads`, so one cancel reaches them all.
#[derive(Default)]
pub(crate) struct Reads {
    state: Mutex<State>,
    /// Wakes every `credential` wait: each finished run, each cancel, and
    /// each move of a clock a wait subscribed it to. A wait re-checks its
    /// own run, the cancel flag and its deadline after every wake, so a
    /// wake meant for another wait only parks again.
    wake: Arc<Parker>,
    /// The next `credential` wait's pause at its checks, armed by a test.
    /// Absent outside the unit tests.
    #[cfg(test)]
    checkpoint: Mutex<Option<Checkpoint>>,
}

/// One pause of a `credential` wait at its checks, for the interleaving
/// tests (`docs/testing.md`, "Waits and timeouts"): the wait reports a
/// pass through its checks and blocks until the test releases it, so the
/// test lands a finish between the checks and the park on every run. A
/// `#[cfg(test)]` hook is the only seam that reaches there: the checks
/// read only channels and the clock, and the state lock covers none of
/// them, so holding it cannot hold the wait between its checks and its
/// park.
#[cfg(test)]
struct Checkpoint {
    /// Sent once the wait passed its checks; the send blocks until the
    /// test receives it, which is the acknowledgement.
    checked: mpsc::Sender<()>,
    /// The test releases the wait through this.
    release: mpsc::Receiver<()>,
}

/// Everything [`Reads`] guards with its one lock, so a cancel and a spawn
/// never interleave.
#[derive(Default)]
struct State {
    cancelled: bool,
    /// Each waiting `run`, by a number of its own: what answers it with
    /// `closing`.
    waiting: BTreeMap<u64, Box<dyn FnOnce() + Send>>,
    next: u64,
    /// Every listed credential command group: one stays listed until no
    /// process is left in it.
    groups: Vec<u32>,
}

impl Reads {
    /// Runs `read` on a thread of its own and waits for its value. Once
    /// shutdown has cancelled the reads, it answers `closing` at once
    /// without starting the thread, and a wait already running ends with
    /// `closing`: the read's late value is dropped with its channel.
    pub(crate) fn run<T: Send + 'static>(
        &self,
        read: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, Failure> {
        let (answer, answered) = mpsc::channel();
        let id = {
            let mut state = self.lock();
            if state.cancelled {
                return Err(closing());
            }
            let id = state.next;
            state.next += 1;
            let cancel = answer.clone();
            state.waiting.insert(
                id,
                Box::new(move || cancel.send(Err(closing())).unwrap_or(())),
            );
            id
        };
        let spawned = std::thread::Builder::new()
            .name("fiber-switch-read".to_owned())
            .spawn(move || answer.send(Ok(read())).unwrap_or(()));
        let got = match spawned {
            Ok(_) => answered.recv().unwrap_or_else(|_| Err(closing())),
            Err(error) => Err(failure(
                ErrorCode::IoFailed,
                format!("a thread for the credential read: {error}"),
            )),
        };
        self.lock().waiting.remove(&id);
        got
    }

    /// Runs a credential source's `command` as the leader of a new process
    /// group and collects its output, through the shared spawn step below.
    /// A command cancelled before it starts, or killed by a cancel, is
    /// `Interrupted`.
    pub(crate) fn command(&self, command: &mut Command) -> io::Result<Output> {
        let child = self.spawn(command)?;
        let output = child.wait_with_output()?;
        if self.lock().cancelled {
            return Err(interrupted());
        }
        Ok(output)
    }

    /// Runs a credential source's `command` as the leader of a new process
    /// group and collects its output, giving up at `timeout` on `clock`.
    /// A run finished by then (its leader exited and stdout at end-of-file)
    /// returns at once, on any clock. A key finished after the wait's own
    /// end is not a key: a run done past its deadline, or after shutdown
    /// cancelled the reads, is killed and reported as the wait ended. A run
    /// not finished at the timeout has its group killed and its leader
    /// reaped before this returns `TimedOut`; the stdout reader is never
    /// waited on after the kill, so a process that left the group and holds
    /// stdout keeps only that reader thread alive, never the caller. Once
    /// [`Reads::cancel`] ran, this returns `Interrupted`, whether it was
    /// waiting, timing out or not yet spawned. A `timeout` no deadline on
    /// `clock` can hold starts nothing and is `InvalidInput`.
    pub(crate) fn credential(
        &self,
        command: &mut Command,
        timeout: Duration,
        clock: &dyn Clock,
    ) -> io::Result<Output> {
        let deadline = clock.now().checked_add(timeout).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "its timeout is longer than this system's clock can count",
            )
        })?;
        let mut child = self.spawn(command)?;
        let group = child.id();
        // Taken before the reaper below owns the child.
        let stdout = child.stdout.take();
        // The leader's exit wakes the wait on its own: a helper that prints
        // its key, closes stdout, then finishes cleanup bumps here when it
        // exits, after the stdout reader's bump was already spent waiting
        // for it. The reaper owns the leader's wait, so the timeout and
        // cancel paths below receive its answer to reap the leader. The
        // holder keeps the leader ours when the reaper's spawn fails, so
        // that failure path can still kill and reap it instead of dropping
        // it unreaped.
        let reaper: Arc<Mutex<Option<Child>>> = Arc::new(Mutex::new(Some(child)));
        let (status_sent, status_received) = mpsc::channel();
        {
            let exit_wake = Arc::clone(&self.wake);
            let holder = Arc::clone(&reaper);
            match std::thread::Builder::new()
                .name("fiber-credential-wait".to_owned())
                .spawn(move || {
                    let taken = holder.lock().unwrap_or_else(PoisonError::into_inner).take();
                    if let Some(mut child) = taken {
                        let status = child.wait();
                        match status_sent.send(status) {
                            Ok(()) | Err(_) => {}
                        }
                    }
                    exit_wake.bump();
                }) {
                Ok(_) => {}
                Err(error) => {
                    kill_group(group);
                    if let Some(mut child) =
                        reaper.lock().unwrap_or_else(PoisonError::into_inner).take()
                    {
                        // The kill above ends the leader at once; the wait
                        // here reaps it, so no failure path leaves a zombie.
                        let _reaped = child.wait();
                    }
                    return Err(io::Error::other(format!(
                        "a thread for the credential read: {error}"
                    )));
                }
            }
        }
        let wake = Arc::clone(&self.wake);
        let subscribing: Arc<dyn Wake> = wake.clone();
        let (sent, received) = mpsc::channel();
        if let Some(mut stdout) = stdout {
            let (sent, wake) = (sent, Arc::clone(&wake));
            match std::thread::Builder::new()
                .name("fiber-credential-stdout".to_owned())
                .spawn(move || {
                    let mut bytes = Vec::new();
                    let read = stdout.read_to_end(&mut bytes).map(|_| bytes);
                    match sent.send(read) {
                        Ok(()) | Err(_) => {}
                    }
                    wake.bump();
                }) {
                Ok(_) => {}
                Err(error) => {
                    kill_group(group);
                    return Err(io::Error::other(format!(
                        "a thread for the credential read: {error}"
                    )));
                }
            }
        } else {
            match sent.send(Ok(Vec::new())) {
                Ok(()) | Err(_) => {}
            }
        }
        clock.subscribe(Arc::downgrade(&subscribing));
        let mut exit: Option<std::process::ExitStatus> = None;
        let mut output: Option<io::Result<Vec<u8>>> = None;
        loop {
            // The generation is captured before the checks: a finish, a
            // cancel or a clock move past this point ends the park below
            // at once, so no wake lands between a check and the park
            // unheard.
            let seen = self.wake.generation();
            if exit.is_none() {
                match status_received.try_recv() {
                    Ok(Ok(status)) => exit = Some(status),
                    Ok(Err(error)) => return Err(error),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(io::Error::other(
                            "the credential read ended without its leader's exit",
                        ));
                    }
                }
            }
            if output.is_none() {
                match received.try_recv() {
                    Ok(outcome) => output = Some(outcome),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(io::Error::other(
                            "the credential read ended without its output",
                        ));
                    }
                }
            }
            // A run is finished only once its leader exited and stdout is
            // at end-of-file: either alone parks again below.
            match (exit.take(), output.take()) {
                (Some(status), Some(outcome)) => {
                    // A key that arrived after the wait's own end is not a
                    // key: the group is killed and the wait reports its
                    // end, never the late key.
                    if self.lock().cancelled {
                        kill_group(group);
                        return Err(interrupted());
                    }
                    if deadline_due(clock.now(), deadline) {
                        kill_group(group);
                        return Err(timed_out());
                    }
                    {
                        let stdout = outcome?;
                        return Ok(Output {
                            status,
                            stdout,
                            stderr: Vec::new(),
                        });
                    }
                }
                (status, outcome) => {
                    exit = status;
                    output = outcome;
                }
            }
            if self.lock().cancelled {
                kill_group(group);
                if exit.is_none() {
                    // The reaper owns the leader's wait: its answer reaps
                    // the leader. After SIGKILL it answers at once; the
                    // receive is ignored either way, since the answer here
                    // is `Interrupted`.
                    let _reaped = status_received.recv();
                }
                return Err(interrupted());
            }
            if deadline_due(clock.now(), deadline) {
                kill_group(group);
                if exit.is_none() {
                    let _reaped = status_received.recv();
                }
                return Err(timed_out());
            }
            // The checkpoint holds the wait here, past its checks and
            // before its park, only when a test armed one.
            #[cfg(test)]
            self.checkpoint();
            self.wake.park(clock, Some(deadline), seen);
        }
    }

    /// Checks for a cancel, spawns `command` as a group leader and lists
    /// its group, all under the state lock, so no command escapes a cancel.
    fn spawn(&self, command: &mut Command) -> io::Result<Child> {
        let mut state = self.lock();
        if state.cancelled {
            return Err(interrupted());
        }
        state.groups.retain(|group| alive(*group));
        let child = command.process_group(0).spawn()?;
        state.groups.push(child.id());
        Ok(child)
    }

    /// Cancels every read: answers each waiting `run` with `closing`, makes
    /// every later `run` and `command` refuse, wakes every `credential`
    /// wait, and sends SIGKILL to every listed group with a process left.
    /// A credential command holds no state to tidy, so it gets no grace
    /// period.
    pub(crate) fn cancel(&self) {
        let mut state = self.lock();
        state.cancelled = true;
        for (_, wake) in std::mem::take(&mut state.waiting) {
            wake();
        }
        self.wake.bump();
        state.groups.retain(|group| alive(*group));
        for group in &state.groups {
            if let Some(pid) = signallable(*group).then(|| pid(*group)).flatten() {
                rustix::process::kill_process_group(pid, Signal::KILL).unwrap_or(());
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
impl Reads {
    /// Arms one pause of the next `credential` wait at its checks: the
    /// wait sends on `checked` after passing its checks and blocks until
    /// `release` arrives (`docs/testing.md`, "Waits and timeouts"). The
    /// pause is one-shot; arming again while the wait is held pauses its
    /// next pass too.
    pub(crate) fn pause_at_checks(&self, checked: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self
            .checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Checkpoint { checked, release });
    }

    /// Reports one pass through the `credential` wait's checks and blocks
    /// until the test releases it. The first pass consumes the armed
    /// pause; later passes run free unless a test armed again meanwhile.
    fn checkpoint(&self) {
        let point = self
            .checkpoint
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.checked.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

/// Whether `now` has reached `deadline`: a credential wait ends once it
/// has. A flipped comparison kills a live group before its timeout.
fn deadline_due(now: Instant, deadline: Instant) -> bool {
    now >= deadline
}

/// Cancels the session's [`Reads`] when the session function returns,
/// whatever the result: a startup failure or a session's end kills what
/// the process's credential commands left in their groups, the same as a
/// shutdown does. Cancelling twice is harmless.
pub(crate) struct CancelOnDrop(pub(crate) Arc<Reads>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Sends SIGKILL to `group` when it can be one of ours; a group already
/// gone is left alone.
fn kill_group(group: u32) {
    if let Some(pid) = signallable(group).then(|| pid(group)).flatten() {
        rustix::process::kill_process_group(pid, Signal::KILL).unwrap_or(());
    }
}
/// A read shutdown cancelled: the switch is rejected `closing`.
fn closing() -> Failure {
    failure(ErrorCode::Closing, CLOSING)
}

fn interrupted() -> io::Error {
    io::Error::new(io::ErrorKind::Interrupted, "the session is shutting down")
}

/// A command still running at its timeout, or finished past it: its group
/// was killed, and reading the key fails.
fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "the command was still running at its timeout",
    )
}

/// Whether a process is left in group `group`.
fn alive(group: u32) -> bool {
    signallable(group)
        && pid(group).is_some_and(|pid| rustix::process::test_kill_process_group(pid).is_ok())
}

/// Group 1 or 0 is never a credential command's: `kill(-1)` reaches every
/// process the user owns, and `kill(0)` Fiber's own group.
fn signallable(group: u32) -> bool {
    group > 1
}

fn pid(raw: u32) -> Option<Pid> {
    Pid::from_raw(i32::try_from(raw).ok()?)
}

#[cfg(test)]
#[path = "read_tests.rs"]
mod tests;
