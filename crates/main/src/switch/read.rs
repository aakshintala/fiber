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
use support::group::GROUP_POLL;

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
    /// The next spawn's pause before it spawns, armed by a test. Absent
    /// outside the unit tests.
    #[cfg(test)]
    spawn_pause: Mutex<Option<SpawnPause>>,
    /// The next `run`'s pause after its value arrived and before it
    /// answers, armed by a test. Absent outside the unit tests.
    #[cfg(test)]
    answered_pause: Mutex<Option<SpawnPause>>,
    /// The next spawn's pause before it reaps a late child, armed by a
    /// test. Absent outside the unit tests.
    #[cfg(test)]
    reap_pause: Mutex<Option<SpawnPause>>,
    /// The next late spawn's pause after it reaped its leader and before
    /// the count drops, armed by a test. Absent outside the unit tests.
    #[cfg(test)]
    reaped_pause: Mutex<Option<SpawnPause>>,
    /// The next spawn's pause after it lists its group, armed by a test.
    /// Absent outside the unit tests.
    #[cfg(test)]
    listed_pause: Mutex<Option<SpawnPause>>,
    /// The next `command`'s pause before it reaps its leader, armed by a
    /// test. Absent outside the unit tests.
    #[cfg(test)]
    command_pause: Mutex<Option<SpawnPause>>,
    /// The timeout settlement's pause after its first kill, armed by a
    /// test. Absent outside the unit tests.
    #[cfg(test)]
    settle_pause: Mutex<Option<SpawnPause>>,
    /// The timeout settlement's pause at the top of its next pass,
    /// before it captures its generation, armed by a test. Absent
    /// outside the unit tests.
    #[cfg(test)]
    settle_pass_pause: Mutex<Option<SpawnPause>>,
    /// The timeout settlement's pause before the reaper wait, armed by a
    /// test. Absent outside the unit tests.
    #[cfg(test)]
    settle_complete_pause: Mutex<Option<SpawnPause>>,
    /// The credential reaper's pause before its leader's wait, armed by a
    /// test. Shared through an `Arc` so the reaper thread owns it: the
    /// thread outlives the call that armed it. Absent outside the unit
    /// tests.
    #[cfg(test)]
    reaper_pause: Arc<Mutex<Option<SpawnPause>>>,
    /// The `settle` loop's pause before its park, armed by a test. Absent
    /// outside the unit tests.
    #[cfg(test)]
    settle_park_pause: Mutex<Option<SpawnPause>>,
    /// The completion notes a test counts before it releases the settle
    /// park: the reaper thread and the stdout reader thread each send one
    /// after their wake bump. Shared through an `Arc` so the threads own
    /// it. Absent outside the unit tests.
    #[cfg(test)]
    completion_notes: Arc<Mutex<Option<mpsc::Sender<()>>>>,
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

/// One pause before a spawn, for the interleaving tests: the spawn reports
/// it reached the spawn and blocks until the test releases it, so the test
/// lands a cancel between the checks and the spawn on every run.
#[cfg(test)]
struct SpawnPause {
    /// Sent once the spawn reached its pause; the send blocks until the
    /// test receives it, which is the acknowledgement.
    entered: mpsc::Sender<()>,
    /// The test releases the spawn through this.
    release: mpsc::Receiver<()>,
}

/// Everything [`Reads`] guards with its one lock. The lock is never held
/// across a spawn: a spawn checks under the lock, spawns with it released,
/// then lists under the lock. A cancel that landed mid-spawn is seen on
/// the relock, and the late child is killed there, so no command escapes
/// a cancel.
#[derive(Default)]
struct State {
    cancelled: bool,
    /// Each spawn still in flight: counted under the first lock of
    /// `spawn` and dropped once it listed or killed its group and reaped
    /// its leader, so [`Reads::settle`] waits out a spawn that raced a
    /// cancel instead of exiting past it.
    spawning: usize,
    /// Each waiting `run`, by a number of its own: what answers it with
    /// `closing`.
    waiting: BTreeMap<u64, Box<dyn FnOnce() + Send>>,
    next: u64,
    /// Every listed credential command group: one stays listed until no
    /// process is left in it. A spawn that raced a cancel lists its group
    /// for settlement too: the leader's reap never retires surviving
    /// descendants. Settlement retires a group only on SRCH and sends
    /// SIGKILL to the rest again on every pass.
    groups: Vec<u32>,
    /// The foreground shutdown's absolute end, stored once by the first
    /// [`Reads::cancel_and_settle`] before its cancel: every later settle
    /// reuses it, so no retry, no group and no caller earns more time.
    /// Absent outside a foreground shutdown, where each settle keeps its
    /// own bound.
    shutdown_end: Option<Instant>,
    /// Scripted probe/send outcomes for one owned listed group, armed by a
    /// test. Absent outside the unit tests.
    #[cfg(test)]
    script: Option<GroupScript>,
}

/// Scripted probe/send outcomes for one owned listed group: the kernel
/// results a test needs that no external seam can force (a kill the walk
/// loses, EPERM, a persistent errno). Consulted only after the guarded id
/// validation, and only for the armed group; every other id takes the real
/// syscalls. It scripts the repair's policy — what a pending probe earns
/// — never the macOS walk itself.
#[cfg(test)]
struct GroupScript {
    group: u32,
    /// Probe errnos consumed in order; exhausted, the real probe runs.
    probes: std::collections::VecDeque<rustix::io::Result<()>>,
    /// Send outcomes consumed in order; exhausted, an accepted send.
    sends: std::collections::VecDeque<rustix::io::Result<()>>,
    /// Every send to the scripted group, scripted or exhausted.
    sent: usize,
}

impl State {
    /// Drops groups with no member left — only SRCH retires one — without
    /// signalling: a new spawn prunes, it never kills another group.
    fn prune_empty(&mut self) {
        let mut kept = Vec::with_capacity(self.groups.len());
        for group in std::mem::take(&mut self.groups) {
            if self.group_alive(group) {
                kept.push(group);
            }
        }
        self.groups = kept;
    }

    /// Prunes groups with no member left and sends SIGKILL to every group
    /// still pending, through the shared helper: the cancel's immediate
    /// kill and settlement's every pass share this one site.
    fn kill_pending(&mut self) {
        self.prune_empty();
        for group in self.groups.clone() {
            self.kill_listed(group);
        }
    }

    /// Whether a listed group still holds a member: only SRCH retires it.
    /// An accepted probe, PERM and every other errno keep it pending.
    fn group_alive(&mut self, group: u32) -> bool {
        if !signallable(group) || pid(group).is_none() {
            return false;
        }
        #[cfg(test)]
        if let Some(outcome) = self.scripted_probe(group) {
            return probe_means_alive(outcome);
        }
        alive(group)
    }

    /// Sends SIGKILL to a listed group through the shared helper. A send
    /// proves delivery was accepted, never that the group is empty: the
    /// next probe decides that.
    fn kill_listed(&mut self, group: u32) {
        if !signallable(group) || pid(group).is_none() {
            return;
        }
        #[cfg(test)]
        if let Some(outcome) = self.scripted_send(group) {
            outcome.unwrap_or(());
            return;
        }
        kill_group(group)
    }

    /// The next scripted probe outcome for the armed group, if this is it.
    #[cfg(test)]
    fn scripted_probe(&mut self, group: u32) -> Option<rustix::io::Result<()>> {
        let script = self.script.as_mut()?;
        if script.group != group {
            return None;
        }
        script.probes.pop_front()
    }

    /// Records a send to the armed group and takes its next scripted
    /// outcome, if this is it.
    #[cfg(test)]
    fn scripted_send(&mut self, group: u32) -> Option<rustix::io::Result<()>> {
        let script = self.script.as_mut()?;
        if script.group != group {
            return None;
        }
        script.sent += 1;
        Some(script.sends.pop_front().unwrap_or(Ok(())))
    }
}

impl Reads {
    /// Runs `read` on a thread of its own and waits for its value. Once
    /// shutdown has cancelled the reads, it answers `closing` at once
    /// without starting the thread, and a wait already running ends with
    /// `closing`: the read's late value is dropped with its channel. A
    /// cancel that lands after the value arrived still cancels the wait,
    /// as `command` does after it reaps its child: the caller must not
    /// proceed as if no shutdown had started.
    pub(crate) fn run<T: Send + 'static>(
        &self,
        purpose: &str,
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
                format!("a thread for the {purpose}: {error}"),
            )),
        };
        let mut state = self.lock();
        state.waiting.remove(&id);
        drop(state);
        #[cfg(test)]
        self.pause_after_answered();
        if got.is_ok() && self.lock().cancelled {
            return Err(closing());
        }
        got
    }

    /// Runs a credential source's `command` as the leader of a new process
    /// group and collects its output, through the shared spawn step below.
    /// A command cancelled before it starts, or killed by a cancel, is
    /// `Interrupted`.
    pub(crate) fn command(&self, command: &mut Command) -> io::Result<Output> {
        let child = self.spawn(command)?;
        // Held only when a test armed a pause, so the leader stays a
        // zombie and its group stays listed until the test releases it.
        #[cfg(test)]
        self.pause_before_command_reap();
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
    /// not finished at the timeout has its group killed again until no
    /// process is left in it, within one bound anchored at the deadline,
    /// and its leader reaped within the same bound before this returns
    /// `TimedOut`; at the bound the group stays listed for later
    /// settlement even if the leader never exits. The stdout reader is never
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
        // Settlement and the reap below share this one absolute end,
        // anchored at the deadline: neither earns a fresh budget, so a
        // leader that never exits still ends the wait at the end. An end
        // past what the clock can count expires at once, below.
        let end = deadline
            .checked_add(doors::SHUTDOWN_BOUND)
            .unwrap_or(deadline);
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
            #[cfg(test)]
            let reaper_pause = Arc::clone(&self.reaper_pause);
            #[cfg(test)]
            let reaper_notes = Arc::clone(&self.completion_notes);
            match std::thread::Builder::new()
                .name("fiber-credential-wait".to_owned())
                .spawn(move || {
                    #[cfg(test)]
                    Reads::pause_before_reaper_wait(&reaper_pause);
                    let taken = holder.lock().unwrap_or_else(PoisonError::into_inner).take();
                    if let Some(mut child) = taken {
                        let status = child.wait();
                        match status_sent.send(status) {
                            Ok(()) | Err(_) => {}
                        }
                    }
                    exit_wake.bump();
                    #[cfg(test)]
                    Reads::note_completion(&reaper_notes);
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
            #[cfg(test)]
            let reader_notes = Arc::clone(&self.completion_notes);
            match std::thread::Builder::new()
                .name("fiber-credential-stdout".to_owned())
                .spawn(move || {
                    let mut bytes = Vec::new();
                    let read = stdout.read_to_end(&mut bytes).map(|_| bytes);
                    match sent.send(read) {
                        Ok(()) | Err(_) => {}
                    }
                    wake.bump();
                    #[cfg(test)]
                    Reads::note_completion(&reader_notes);
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
                        self.settle_group(group, clock, end);
                        if self.lock().cancelled {
                            kill_group(group);
                            return Err(interrupted());
                        }
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
                self.settle_group(group, clock, end);
                #[cfg(test)]
                self.pause_after_settle_complete();
                if self.lock().cancelled {
                    kill_group(group);
                    return Err(interrupted());
                }
                if exit.is_none() {
                    // The reaper owns the leader's wait: its answer reaps
                    // the leader. An emptied group already delivered it, so
                    // this returns at once; the receive is ignored either
                    // way, since the answer here is `TimedOut`. The reap
                    // shares settlement's absolute end: at the end the
                    // group stays listed for later settlement even if the
                    // leader never exits, so this never blocks past it.
                    loop {
                        // The generation is captured before the checks, as in
                        // `credential`: a finish past this point ends the park
                        // below at once, so no reap lands unheard.
                        let seen = self.wake.generation();
                        match status_received.try_recv() {
                            Ok(_) | Err(mpsc::TryRecvError::Disconnected) => break,
                            Err(mpsc::TryRecvError::Empty) => {}
                        }
                        if self.lock().cancelled {
                            kill_group(group);
                            return Err(interrupted());
                        }
                        if clock.now() >= end {
                            break;
                        }
                        let wait_until = clock
                            .now()
                            .checked_add(GROUP_POLL)
                            .map_or(end, |poll| poll.min(end));
                        self.wake.park(clock, Some(wait_until), seen);
                    }
                }
                if self.lock().cancelled {
                    kill_group(group);
                    return Err(interrupted());
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

    /// Refuses when cancelled, counts the spawn, spawns `command` as a
    /// group leader with the lock released, then lists its group under the
    /// lock. A cancel that landed mid-spawn lists the late group for
    /// settlement under the relock, then kills it with SIGKILL, reaps the
    /// leader with the lock released, and is `Interrupted`, so no command
    /// escapes a cancel and no descendant outlives the settle.
    /// [`Reads::settle`] waits for the count to reach zero, so an exit
    /// after a cancel never leaves a spawn in flight behind.
    fn spawn(&self, command: &mut Command) -> io::Result<Child> {
        {
            let mut state = self.lock();
            if state.cancelled {
                return Err(interrupted());
            }
            state.spawning += 1;
        }
        #[cfg(test)]
        self.pause_before_spawn();
        // The error arm drops the count it just raised: a `?` here would
        // skip it and hang the next settle.
        let mut child = match command.process_group(0).spawn() {
            Ok(child) => child,
            Err(error) => {
                self.spawned_out();
                return Err(error);
            }
        };
        let group = child.id();
        let mut state = self.lock();
        let late = state.cancelled;
        state.prune_empty();
        state.groups.push(group);
        self.wake.bump();
        if late {
            // Reaped with the lock released: a leader in uninterruptible
            // sleep (a hung filesystem) must not stall a cancel behind it.
            // The group stays listed through the reap below, and the count
            // drops only after it, so a settle that returns has seen the
            // leader reaped, never just the kill sent.
            drop(state);
            self.lock().kill_listed(group);
            #[cfg(test)]
            self.pause_before_reap();
            let _reaped = child.wait();
            #[cfg(test)]
            self.pause_after_reaped();
            self.spawned_out();
            return Err(interrupted());
        }
        state.spawning -= 1;
        drop(state);
        #[cfg(test)]
        self.pause_after_listed();
        Ok(child)
    }

    /// Marks one spawn out of flight and wakes every [`Reads::settle`]
    /// wait: the count drops under the lock and the bump follows, so a
    /// settle that wakes on the bump reads the lower count.
    fn spawned_out(&self) {
        self.lock().spawning -= 1;
        self.wake.bump();
    }

    /// Waits until no spawn is still in flight and, once cancelled, every
    /// listed group is empty, or the bound passes. Outside a cancelled
    /// shutdown this keeps its count-only contract and its own bound: the
    /// clock subscription comes before the first count read, so a clock
    /// advance wakes even a settle that started with nothing spawned.
    /// Within a foreground shutdown it reads the end the first
    /// [`Reads::cancel_and_settle`] stored, not a fresh bound: neither a
    /// retry nor another group earns more time. Every pass prunes only
    /// groups observed SRCH and sends SIGKILL to the rest again, together,
    /// rechecking on a bounded cadence within the same end. `true` leaves
    /// no group behind: a spawn that raced the cancel relocked, listed its
    /// group, killed it and reaped the leader. `false` is the same
    /// SIGKILL-or-hung-filesystem case the bound covers elsewhere: the
    /// caller exits anyway.
    pub(crate) fn settle(&self, clock: &dyn Clock, bound: Duration) -> bool {
        let wake = Arc::clone(&self.wake);
        let subscribing: Arc<dyn Wake> = wake.clone();
        clock.subscribe(Arc::downgrade(&subscribing));
        // An end past what the clock can count expires at once, below.
        let stored = self.lock().shutdown_end;
        let now = clock.now();
        let end = stored.unwrap_or_else(|| now.checked_add(bound).unwrap_or(now));
        loop {
            // The generation is captured before the checks, as in
            // `credential`: a finish past this point ends the park below
            // at once, so no decrement lands unheard.
            let seen = self.wake.generation();
            let (done, retrying) = {
                let mut state = self.lock();
                if state.cancelled {
                    state.kill_pending();
                }
                let done = state.spawning == 0 && (!state.cancelled || state.groups.is_empty());
                (done, state.cancelled && !state.groups.is_empty())
            };
            if done {
                return true;
            }
            if clock.now() >= end {
                return false;
            }
            // A pending group is rechecked on a bounded cadence within
            // the same end — the next poll or the end, whichever is
            // first — never a fresh budget. A settle with only a count to
            // wait out parks to the end itself, as before.
            let wait_until = if retrying {
                Some(
                    clock
                        .now()
                        .checked_add(GROUP_POLL)
                        .map_or(end, |poll| poll.min(end)),
                )
            } else {
                Some(end)
            };
            #[cfg(test)]
            self.pause_before_settle_park();
            self.wake.park(clock, wait_until, seen);
        }
    }

    /// Sends SIGKILL to `group` again while a probe still finds a member in
    /// it, on the group poll cadence within one absolute end: only SRCH
    /// retires the group, so a fork that landed inside an earlier kill earns
    /// another send instead of leaving a survivor behind. Scoped to the one
    /// timed-out group through the shared helper: nothing here cancels
    /// another read or kills another group. At the end the group stays
    /// listed for later settlement, as [`Reads::settle`] leaves anything
    /// still pending at its bound. A cancel ends the settlement at once:
    /// every pass observes `cancelled` — a cancel bumps the wake, so the
    /// parked pass ends at once — and the caller answers `Interrupted`, so
    /// a cancel that lands while timing out is never `TimedOut`.
    fn settle_group(&self, group: u32, clock: &dyn Clock, end: Instant) {
        loop {
            // Held at the top of the pass when a test armed one: the
            // take consumes one armed pause per pass, so passes run free
            // unless the test armed again meanwhile.
            #[cfg(test)]
            self.pause_before_settle_pass();
            // The generation is captured before the checks, as in
            // `credential`: a clock move past this point ends the park
            // below at once, so no advance lands unheard.
            let seen = self.wake.generation();
            let empty = {
                let mut state = self.lock();
                state.prune_empty();
                if state.groups.contains(&group) {
                    state.kill_listed(group);
                    false
                } else {
                    true
                }
            };
            if empty {
                return;
            }
            // Held after a kill when a test armed a pause: the take
            // consumes one armed pause per pass, so passes run free
            // unless the test armed again meanwhile.
            #[cfg(test)]
            self.pause_after_settle_kill();
            if self.lock().cancelled {
                return;
            }
            if clock.now() >= end {
                return;
            }
            // A pending group is rechecked on a bounded cadence within
            // the same end — the next poll or the end, whichever is
            // first — never a fresh budget, as in [`Reads::settle`].
            let wait_until = clock
                .now()
                .checked_add(GROUP_POLL)
                .map_or(end, |poll| poll.min(end));
            self.wake.park(clock, Some(wait_until), seen);
        }
    }

    /// Cancels every read: answers each waiting `run` with `closing`, makes
    /// every later `run` and `command` refuse, wakes every `credential`
    /// wait, and sends SIGKILL to every listed group with a process left,
    /// through the shared helper. Only SRCH retires a listed group; the
    /// rest stay listed for settlement. A credential command holds no state
    /// to tidy, so it gets no grace period.
    pub(crate) fn cancel(&self) {
        let mut state = self.lock();
        state.cancelled = true;
        for (_, wake) in std::mem::take(&mut state.waiting) {
            wake();
        }
        self.wake.bump();
        state.kill_pending();
    }

    /// Cancels the reads and settles them within one shared bound. The
    /// first call stores the shutdown end from the clock before its
    /// cancel, and every later call — a repeat signal, another group,
    /// another guard's drop — reuses it, so no retry, no group and no
    /// caller earns more time. An end past what the clock can count
    /// expires at once. Returns what the settle saw: `false` at the bound
    /// with anything still pending.
    pub(crate) fn cancel_and_settle(&self, clock: &dyn Clock, bound: Duration) -> bool {
        {
            let now = clock.now();
            let mut state = self.lock();
            if state.shutdown_end.is_none() {
                state.shutdown_end = Some(now.checked_add(bound).unwrap_or(now));
            }
        }
        self.cancel();
        self.settle(clock, bound)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
impl Reads {
    /// Scripts probe/send outcomes for one owned listed group (see
    /// [`GroupScript`]): the kernel results no external seam can force.
    /// Armed only for a group this `Reads` listed; every other id takes
    /// the real syscalls after the same guarded validation.
    pub(crate) fn script_group(
        &self,
        group: u32,
        probes: Vec<rustix::io::Result<()>>,
        sends: Vec<rustix::io::Result<()>>,
    ) {
        self.lock().script = Some(GroupScript {
            group,
            probes: probes.into(),
            sends: sends.into(),
            sent: 0,
        });
    }

    /// Every send to the scripted group so far, scripted or exhausted.
    pub(crate) fn scripted_sends(&self) -> usize {
        self.lock().script.as_ref().map_or(0, |script| script.sent)
    }

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

    /// Arms one pause before the next spawn: the spawn sends on `entered`
    /// once it reaches the spawn and blocks until `release` arrives. The
    /// pause is one-shot.
    pub(crate) fn pause_at_spawn(&self, entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self
            .spawn_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the spawn reached its pause and blocks until the test
    /// releases it. Runs free unless a test armed a pause meanwhile.
    fn pause_before_spawn(&self) {
        let point = self
            .spawn_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause after the next `run`'s value arrived and before it
    /// answers: the run sends on `entered` once it holds its value and
    /// blocks until `release` arrives. The pause is one-shot.
    pub(crate) fn pause_at_answered(&self, entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self
            .answered_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the run holds its value and blocks until the test
    /// releases it. Runs free unless a test armed a pause meanwhile.
    fn pause_after_answered(&self) {
        let point = self
            .answered_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause after the next spawn lists its group: the spawn
    /// sends on `entered` once the group is listed and blocks until
    /// `release` arrives. The pause is one-shot.
    pub(crate) fn pause_at_listed(&self, entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self
            .listed_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the spawn listed its group and blocks until the test
    /// releases it. Runs free unless a test armed a pause meanwhile.
    fn pause_after_listed(&self) {
        let point = self
            .listed_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause before the next spawn reaps a late child: the spawn
    /// sends on `entered` once it dropped the lock past the kill and
    /// blocks until `release` arrives. The pause is one-shot.
    pub(crate) fn pause_at_reap(&self, entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self
            .reap_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the spawn reached the late child's reap and blocks until
    /// the test releases it. Runs free unless a test armed a pause
    /// meanwhile.
    fn pause_before_reap(&self) {
        let point = self
            .reap_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause after the next late spawn reaps its leader and
    /// before the count drops: the spawn sends on `entered` once the
    /// leader is reaped and blocks until `release` arrives, so the test
    /// holds the raised count past the reap on every run. The pause is
    /// one-shot.
    pub(crate) fn pause_at_reaped(&self, entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self
            .reaped_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the late spawn reaped its leader and blocks until the test
    /// releases it. Runs free unless a test armed a pause meanwhile.
    fn pause_after_reaped(&self) {
        let point = self
            .reaped_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause before the next `command` reaps its leader: the
    /// command thread sends on `entered` once its spawn listed its group
    /// and blocks until `release` arrives, so the leader stays a zombie
    /// and its group stays listed until the test releases it. The pause
    /// is one-shot.
    pub(crate) fn pause_at_command_reap(
        &self,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    ) {
        *self
            .command_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the command reached its leader's reap and blocks until the
    /// test releases it. Runs free unless a test armed a pause meanwhile.
    fn pause_before_command_reap(&self) {
        let point = self
            .command_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause after the timeout settlement kills its group: the
    /// settlement sends on `entered` once it killed its group and blocks
    /// until `release` arrives, so the test holds the settlement past
    /// the kill on every run. Each armed pause holds one pass; re-arming
    /// holds a later pass. The pause is one-shot.
    pub(crate) fn pause_at_settle(&self, entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self
            .settle_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the settlement killed its group and blocks until the test
    /// releases it. Runs free unless a test armed a pause meanwhile; each
    /// pass consumes one armed pause, so re-arming pauses a later pass too.
    fn pause_after_settle_kill(&self) {
        let point = self
            .settle_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause at the top of the timeout settlement's next pass:
    /// the pass sends on `entered` before it captures its generation
    /// and blocks until `release` arrives, so the test lands a wake
    /// before the pass's generation on every run. The pause is one-shot.
    pub(crate) fn pause_at_settle_pass(
        &self,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    ) {
        *self
            .settle_pass_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the settlement reached its next pass and blocks until the
    /// test releases it. Runs free unless a test armed a pause meanwhile;
    /// each pass consumes one armed pause, so re-arming pauses a later
    /// pass too.
    fn pause_before_settle_pass(&self) {
        let point = self
            .settle_pass_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause before the credential reaper's leader wait: the
    /// reaper sends on `entered` once it reached the wait and blocks until
    /// `release` arrives, so the leader stays unreaped until the test
    /// releases it. The pause is one-shot; a closed sender releases it.
    pub(crate) fn pause_at_reaper(&self, entered: mpsc::Sender<()>, release: mpsc::Receiver<()>) {
        *self
            .reaper_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the reaper reached its leader's wait and blocks until the
    /// test releases it. The slot is taken into a local first and the
    /// mutex guard dropped before any send or receive blocks, so no lock
    /// is held across the pause. Runs free unless a test armed a pause
    /// meanwhile.
    fn pause_before_reaper_wait(slot: &Arc<Mutex<Option<SpawnPause>>>) {
        let point = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms one pause before the next `settle` pass's park: the pass sends
    /// on `entered` once it captured its generation and computed its wait,
    /// and blocks until `release` arrives, so the test lands the reaper's
    /// bump between the generation and the park on every run. The pause
    /// is one-shot; a closed sender releases it.
    pub(crate) fn pause_at_settle_park(
        &self,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    ) {
        *self
            .settle_park_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports the settle reached its park and blocks until the test
    /// releases it. The slot is taken into a local first and the mutex
    /// guard dropped before any send or receive blocks, so no lock is
    /// held across the pause. Runs free unless a test armed a pause
    /// meanwhile.
    fn pause_before_settle_park(&self) {
        let point = self
            .settle_park_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
                Ok(()) | Err(_) => {}
            }
            match point.release.recv() {
                Ok(()) | Err(_) => {}
            }
        }
    }

    /// Arms the completion notes a test counts before it releases the
    /// settle park: the reaper thread and the stdout reader thread each
    /// send one after their wake bump, in no claimed order.
    pub(crate) fn note_completions(&self, sender: mpsc::Sender<()>) {
        *self
            .completion_notes
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(sender);
    }

    /// Sends one completion note after a credential thread's wake bump, if
    /// a test armed one. The sender is cloned into a local first and the
    /// mutex guard dropped before the send, so no lock is held across it.
    fn note_completion(slot: &Arc<Mutex<Option<mpsc::Sender<()>>>>) {
        let note = slot.lock().unwrap_or_else(PoisonError::into_inner).clone();
        if let Some(note) = note {
            note.send(()).unwrap_or(());
        }
    }

    /// Arms one pause after the timeout settlement and before the reaper
    /// wait, so a test can inspect group retirement before killing the real
    /// process group. The pause is one-shot.
    pub(crate) fn pause_at_settle_complete(
        &self,
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    ) {
        *self
            .settle_complete_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(SpawnPause { entered, release });
    }

    /// Reports timeout settlement completion and blocks until the test
    /// releases the reaper wait. Runs free unless a test armed a pause.
    fn pause_after_settle_complete(&self) {
        let point = self
            .settle_complete_pause
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(point) = point {
            match point.entered.send(()) {
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

/// Whether a group probe leaves a member in the group: only SRCH — no
/// such group — empties it. An accepted probe, PERM and every other errno
/// keep the group pending for the next pass. A successful send proves
/// delivery was accepted, never that the group is empty.
fn probe_means_alive(outcome: rustix::io::Result<()>) -> bool {
    match outcome {
        Ok(()) => true,
        Err(rustix::io::Errno::SRCH) => false,
        Err(_) => true,
    }
}
/// Whether a process is left in group `group`: only SRCH retires it (see
/// [`probe_means_alive`]).
fn alive(group: u32) -> bool {
    signallable(group)
        && pid(group)
            .is_some_and(|pid| probe_means_alive(rustix::process::test_kill_process_group(pid)))
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
