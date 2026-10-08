//! A switch's credential read, on its own thread (`docs/configuration.md`,
//! "Secrets"): the loop waits for it inside `prepare`, and shutdown ends the
//! wait at once and kills every credential command a read started
//! (`docs/invocation.md`, "Shutdown").

use std::collections::BTreeMap;
use std::io;
use std::os::unix::process::CommandExt as _;
use std::process::{Command, Output};
use std::sync::{Mutex, PoisonError, mpsc};

use contract::ErrorCode;
use contract::shapes::Failure;
use doors::failure;
use rustix::process::{Pid, Signal};

/// Why a read ends once shutdown has started.
const CLOSING: &str = "The session is shutting down.";

/// The reads of one session: whether shutdown cancelled them, who waits on
/// one, and the process group of every credential command they started.
/// The session's worktree creation runs its `git worktree add` through a
/// `Reads` of its own, so a signal kills git and its hook together.
#[derive(Default)]
pub(crate) struct Reads {
    state: Mutex<State>,
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
    /// group and collects its output. It checks for a cancel, spawns and
    /// lists the group under the lock [`Reads::cancel`] takes, so no
    /// command escapes a cancel; it waits for the output outside that lock.
    /// A command cancelled before it starts, or killed by a cancel, is
    /// `Interrupted`.
    pub(crate) fn command(&self, command: &mut Command) -> io::Result<Output> {
        let child = {
            let mut state = self.lock();
            if state.cancelled {
                return Err(interrupted());
            }
            state.groups.retain(|group| alive(*group));
            let child = command.process_group(0).spawn()?;
            state.groups.push(child.id());
            child
        };
        let output = child.wait_with_output()?;
        if self.lock().cancelled {
            return Err(interrupted());
        }
        Ok(output)
    }

    /// Cancels every read: answers each waiting `run` with `closing`, makes
    /// every later `run` and `command` refuse, and sends SIGKILL to every
    /// listed group with a process left. A credential command holds no state
    /// to tidy, so it gets no grace period.
    pub(crate) fn cancel(&self) {
        let mut state = self.lock();
        state.cancelled = true;
        for (_, wake) in std::mem::take(&mut state.waiting) {
            wake();
        }
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
