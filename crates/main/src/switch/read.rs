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
    /// returns at once, on any clock. A run not finished at the timeout
    /// has its group killed and its leader reaped before this returns
    /// `TimedOut`; the stdout reader is never waited on after the kill, so
    /// a process that left the group and holds stdout keeps only that
    /// reader thread alive, never the caller. Once [`Reads::cancel`] ran,
    /// this returns `Interrupted`, whether it was waiting, timing out or
    /// not yet spawned. A `timeout` no deadline on `clock` can hold starts
    /// nothing and is `InvalidInput`.
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
        let wake = Arc::clone(&self.wake);
        let subscribing: Arc<dyn Wake> = wake.clone();
        let (sent, received) = mpsc::channel();
        if let Some(mut stdout) = child.stdout.take() {
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
        let mut status = None;
        loop {
            if status.is_none() {
                status = child.try_wait()?;
            }
            if let Some(exit) = status {
                match received.try_recv() {
                    Ok(Ok(stdout)) => {
                        return Ok(Output {
                            status: exit,
                            stdout,
                            stderr: Vec::new(),
                        });
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return Err(io::Error::other(
                            "the credential read ended without its output",
                        ));
                    }
                }
            }
            if self.lock().cancelled {
                kill_group(group);
                if status.is_none() {
                    child.wait()?;
                }
                return Err(interrupted());
            }
            if deadline_due(clock.now(), deadline) {
                kill_group(group);
                if status.is_none() {
                    child.wait()?;
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the command was still running at its timeout",
                ));
            }
            // The generation is read after the checks above, and the park
            // returns at once on any bump past it, so a finish, a cancel
            // or a clock move that lands between the checks and the park
            // still ends the park.
            let seen = self.wake.generation();
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
