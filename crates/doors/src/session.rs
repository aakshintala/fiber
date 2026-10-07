//! One session process's door side (`docs/invocation.md`, "Lifecycle" and
//! "Processes"): its socket at `run/<session_id>` in Fiber home, its event
//! stream copied to stdout, the clients on that socket, and what is left
//! when it exits.

use std::fs;
use std::io::Write;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::thread::{self, JoinHandle};

use crate::client;
use crate::socket::{bind, remove_socket};
use crate::{failure, mint};
use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{Clients, CommandInfo, Event, ToolInfo};
use contract::inbox::{Ack, Delivery, Message};
use contract::shapes::{ContentPart, Failure, Origin, Sender as CommandSender};
use contract::tool::Tool;
use contract::{CommandId, ErrorCode, SessionId};
use log::{Log, Watcher};

mod accept;
mod conns;
mod event;
mod shells;
#[cfg(test)]
use accept::accept_error_waits;
use accept::accept_loop;
use conns::Conns;
#[cfg(test)]
use conns::{GRACE, grace_remains};
pub(crate) use event::envelope;
use shells::RunningShells;
pub(crate) use shells::Stopped;

/// A running session process's door side: its socket, the clients on it, and
/// the thread copying its events to stdout.
pub struct Session {
    dir: PathBuf,
    socket: PathBuf,
    printer: JoinHandle<()>,
    gate: Arc<Gate>,
    listener: Mutex<Option<UnixListener>>,
    accept: Mutex<Option<JoinHandle<()>>>,
}

/// What every connection thread shares. The session holds it strongly; the
/// log is weak, so [`Session::close`] is what releases the lock.
pub(crate) struct Gate {
    pub(crate) log: Weak<Log>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) session_id: SessionId,
    pub(crate) tools: Vec<ToolInfo>,
    /// What the `commands` command answers with, set by
    /// [`Session::commands`]; empty until then.
    commands: Mutex<Vec<CommandInfo>>,
    inbox: Mutex<Option<Sender<Delivery>>>,
    /// What the `cancel` command asks: whether a turn is running. Stored
    /// by [`Session::run`], so a missing closure is no turn.
    cancel: Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
    /// The tool a driver `shell` runs. None leaves `shell` unknown.
    driver_shell: Mutex<Option<Arc<dyn Tool>>>,
    /// The session's jobs, which `job_stop` and `background` reach. None
    /// leaves both with nothing to act on.
    jobs: Mutex<Option<Arc<dyn contract::jobs::Jobs>>>,
    /// The session's hooks, which receive the loop's inbox so an extension's
    /// program run can be logged as `extension_exec`.
    hooks: Mutex<Option<Arc<dyn contract::hook::Hooks>>>,
    /// The session's extensions as the door reaches them, for the `command`
    /// driver command.
    door: Mutex<Option<Arc<dyn contract::extension::ExtensionDoor>>>,
    /// Ids of accepted `command` commands, remembered for as long as the
    /// process runs, so a retransmission is rejected `duplicate_command`.
    accepted_commands: Mutex<std::collections::BTreeSet<String>>,
    /// The project's `history.jsonl`, set by [`Session::serve`] alone, so
    /// `fiber ask` and a bare [`Session::run`] append no prompt.
    pub(crate) history: OnceLock<Option<PathBuf>>,
    /// Driver shells running now, and whether shutdown has begun, which
    /// cancels a new one as it registers. Both sit under this lock, so a
    /// shell that registers after `close` cannot miss the snapshot.
    shells: Mutex<RunningShells>,
    /// Signalled whenever a driver shell leaves [`Gate::shells`].
    shell_ended: Condvar,
    /// What a test observes, or holds, at a [`tests::Probe`] point.
    #[cfg(test)]
    pub(crate) probe: Mutex<Option<tests::Prober>>,
    stop: AtomicBool,
    /// The `full` connections, and whether `clients` lines are sealed.
    clients: Mutex<(u32, bool)>,
    /// Paired with [`Gate::conns`].
    writers: Condvar,
    conns: Mutex<Conns>,
}

impl Session {
    /// Opens the door side of the session whose directory, just created, is
    /// `dir`: binds its socket in `home`'s `run/` and starts copying every
    /// event to `out`, one JSON line each. `tools` is what `tools` answers
    /// with. A failure is one before any session exists, so it deletes `dir`.
    pub fn open(
        home: &Path,
        dir: &Path,
        log: &Arc<Log>,
        clock: Arc<dyn Clock>,
        tools: Vec<ToolInfo>,
        out: Box<dyn Write + Send>,
    ) -> Result<Self, Failure> {
        let opened = open_in(home, dir, log, clock, tools, out);
        if opened.is_err() {
            fs::remove_dir_all(dir).unwrap_or(());
        }
        opened
    }

    /// Opens the door side of the session whose directory, from an earlier
    /// run, is `dir`: as [`Session::open`], but a failure never deletes
    /// `dir`. A resume names a session that already holds turns, so whatever
    /// stops the resume leaves it for the next one.
    pub fn resume(
        home: &Path,
        dir: &Path,
        log: &Arc<Log>,
        clock: Arc<dyn Clock>,
        tools: Vec<ToolInfo>,
        out: Box<dyn Write + Send>,
    ) -> Result<Self, Failure> {
        open_in(home, dir, log, clock, tools, out)
    }

    /// Sends `first` on the inbox, then serves clients until `run` returns.
    /// `first` is queued before any client's command. The jobs
    /// [`Session::jobs`] set send their ends to the same inbox.
    pub fn run(
        &self,
        first: Vec<Delivery>,
        cancel: Arc<dyn Fn() -> bool + Send + Sync>,
        run: impl FnOnce(Receiver<Delivery>) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let (inbox, waiting) = mpsc::channel();
        for delivery in first {
            // The receiver is still here, so the send cannot fail.
            match inbox.send(delivery) {
                Ok(()) => {}
                Err(mpsc::SendError(delivery)) => drop(delivery),
            }
        }
        // A job's end wakes the loop through the same inbox
        // (`docs/tools.md`, "Background jobs").
        if let Some(jobs) = lock(&self.gate.jobs).as_ref() {
            jobs.deliver_to(inbox.clone());
        }
        if let Some(hooks) = lock(&self.gate.hooks).as_ref() {
            hooks.deliver_to(inbox.clone());
        }
        *lock(&self.gate.inbox) = Some(inbox);
        *lock(&self.gate.cancel) = Some(cancel);
        self.start_accept()?;
        run(waiting)
    }

    /// Runs `fiber ask`'s one turn: sends `prompt` then `close`, so the
    /// loop finishes that turn and exits. A client attached to the socket
    /// neither keeps the session alive nor starts a second turn.
    pub fn ask(
        &self,
        prompt: String,
        cancel: Arc<dyn Fn() -> bool + Send + Sync>,
        run: impl FnOnce(Receiver<Delivery>) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        self.run(
            vec![
                Delivery::Prompt(prompt_message(prompt), ignore()),
                Delivery::Close(ignore()),
            ],
            cancel,
            run,
        )
    }

    /// Runs the internal session command: queues `prompt` when one was
    /// supplied and serves clients until idle exit or `close`. With no
    /// prompt it delivers nothing until a client sends one; each accepted
    /// client `prompt`, not `prompt` itself, joins the project's history.
    pub fn serve(
        &self,
        prompt: Option<String>,
        cancel: Arc<dyn Fn() -> bool + Send + Sync>,
        run: impl FnOnce(Receiver<Delivery>) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let history = crate::prompt_history::path(&self.dir);
        self.gate.history.get_or_init(|| history);
        let first = prompt.map(|prompt| Delivery::Prompt(prompt_message(prompt), ignore()));
        self.run(first.into_iter().collect(), cancel, run)
    }

    /// The tool a driver `shell` runs (`docs/invocation.md`, "Shell").
    /// With none set, `shell` stays an unknown command.
    pub fn shell(&self, tool: Arc<dyn Tool>) {
        *lock(&self.gate.driver_shell) = Some(tool);
    }

    /// Every `/name` the session runs, which the `commands` driver command
    /// answers with verbatim (`docs/invocation.md`, "What each command
    /// does"). Set before [`Session::run`]; with none set, the answer is an
    /// empty list.
    pub fn commands(&self, commands: Vec<CommandInfo>) {
        *lock(&self.gate.commands) = commands;
    }

    /// The session's jobs, which the `job_stop` and `background` driver
    /// commands act on (`docs/invocation.md`, "Driver commands"). With none
    /// set, both are rejected `stale_request`: no job or call is running.
    pub fn jobs(&self, jobs: Arc<dyn contract::jobs::Jobs>) {
        *lock(&self.gate.jobs) = Some(jobs);
    }

    /// The session's hooks, which receive the loop's inbox beside the jobs.
    pub fn hooks(&self, hooks: Arc<dyn contract::hook::Hooks>) {
        *lock(&self.gate.hooks) = Some(hooks);
    }

    /// The session's extensions as the `command` driver command reaches them.
    pub fn extensions(&self, door: Arc<dyn contract::extension::ExtensionDoor>) {
        *lock(&self.gate.door) = Some(door);
    }

    /// What a shutdown calls to stop the door side's work
    /// (`docs/invocation.md`, "Shutdown"): every driver shell is cancelled,
    /// a later one is cancelled as it starts, and the loop is woken. Once
    /// the inbox is gone it wakes nothing.
    pub fn stopper(&self) -> Arc<dyn Fn() + Send + Sync> {
        let gate = Arc::clone(&self.gate);
        Arc::new(move || {
            gate.cancel_shells();
            gate.deliver(Delivery::Cancelled);
        })
    }

    /// Makes `fiber_exited` the last line this process writes: no later
    /// `clients` line is emitted, no new driver shell runs, and every
    /// running one is cancelled and waited for. Called just before
    /// `fiber_exited`. Idempotent; [`Session::close`] behaves as before.
    pub fn quiesce(&self) {
        // An emission holds this lock, so one in flight finishes first.
        lock(&self.gate.clients).1 = true;
        if let Some(door) = lock(&self.gate.door).clone() {
            door.seal();
        }
        self.gate.cancel_shells();
        self.gate.wait_shells();
    }

    /// Ends the door side: stops accepting, unlinks the socket, drops `log`
    /// (the last handle, which releases the lock), waits up to [`conns::GRACE`] for
    /// each writer, then shuts down whatever is still open. Every driver
    /// shell is cancelled first, since shutting its socket does not stop the
    /// tool, and waited for: its thread is not joined, and its answer is
    /// queued before it ends. A reader can still admit one until it is
    /// joined; that shell starts cancelled and is waited for once no reader
    /// is left, though its answer may reach no writer.
    pub fn close(self, log: Arc<Log>) {
        self.gate.cancel_shells();
        self.gate.wait_shells();
        #[cfg(test)]
        self.gate.note(tests::Probe::FirstShellWaitDone);
        self.gate.mark_stopped();
        // Wakes `accept` if it is blocked in `accept`. A connection during
        // teardown is dropped, not served.
        match UnixStream::connect(&self.socket) {
            Ok(_) | Err(_) => {}
        }
        if let Some(accept) = lock(&self.accept).take() {
            join(accept);
        }
        drop(lock(&self.listener).take());
        remove_socket(&self.socket);
        if !prompted(&self.dir) {
            fs::remove_dir_all(&self.dir).unwrap_or(());
        }
        drop(log);
        self.gate.wait_writers();
        self.gate.join_clients();
        self.gate.wait_shells();
        join(self.printer);
    }

    fn start_accept(&self) -> Result<(), Failure> {
        let Some(listener) = lock(&self.listener).take() else {
            return Ok(());
        };
        let gate = Arc::clone(&self.gate);
        let accept = thread::Builder::new()
            .name("accept".to_owned())
            .spawn(move || accept_loop(listener, gate))
            .map_err(|e| failure(ErrorCode::IoFailed, format!("cannot start a thread: {e}")))?;
        *lock(&self.accept) = Some(accept);
        Ok(())
    }
}

impl Gate {
    #[cfg(test)]
    pub(crate) fn note(&self, point: tests::Probe) {
        let probe = lock(&self.probe).clone();
        if let Some(probe) = probe {
            probe(point);
        }
    }

    /// One more `full` connection, and the `clients` line for it.
    pub(crate) fn attach(&self) {
        let mut clients = lock(&self.clients);
        clients.0 += 1;
        if !clients.1 {
            self.emit_clients(clients.0);
        }
    }

    /// One fewer `full` connection, and the `clients` line for it.
    pub(crate) fn detach(&self) {
        let mut clients = lock(&self.clients);
        clients.0 = clients.0.saturating_sub(1);
        if !clients.1 {
            self.emit_clients(clients.0);
        }
    }

    fn emit_clients(&self, count: u32) {
        let Some(log) = self.log.upgrade() else {
            return;
        };
        log.emit(&Event::Clients(Clients { count }));
    }

    pub(crate) fn commands(&self) -> Vec<CommandInfo> {
        lock(&self.commands).clone()
    }

    pub(crate) fn door(&self) -> Option<Arc<dyn contract::extension::ExtensionDoor>> {
        lock(&self.door).clone()
    }

    /// Remembers `id` as an accepted `command` id. Returns false when the id
    /// was already accepted, across connections, for as long as the process
    /// runs. A rejected `command` records nothing.
    pub(crate) fn admit_command_id(&self, id: &contract::CommandId) -> bool {
        lock(&self.accepted_commands).insert(id.0.clone())
    }

    /// Forgets `id`, so a rejected `command` records nothing and a corrected
    /// resend with the same id is admitted.
    pub(crate) fn forget_command_id(&self, id: &contract::CommandId) {
        lock(&self.accepted_commands).remove(&id.0);
    }

    pub(crate) fn jobs(&self) -> Option<Arc<dyn contract::jobs::Jobs>> {
        lock(&self.jobs).clone()
    }

    pub(crate) fn driver_shell(&self) -> Option<Arc<dyn Tool>> {
        lock(&self.driver_shell).clone()
    }

    pub(crate) fn deliver(&self, delivery: Delivery) {
        let inbox = lock(&self.inbox);
        let Some(inbox) = inbox.as_ref() else {
            drop(delivery);
            return;
        };
        if let Err(mpsc::SendError(delivery)) = inbox.send(delivery) {
            drop(delivery);
        }
    }
}

impl Wake for Gate {
    fn wake(&self) {
        // The lock is taken before the notify, so a waiter that has judged
        // and not yet parked cannot miss this wake.
        {
            let _held = lock(&self.conns);
            self.writers.notify_all();
        }
        // A clock move wakes the loop the way an accepted cancel does.
        // The delivery means only that the loop should look again.
        self.deliver(Delivery::Cancelled);
    }
}

fn open_in(
    home: &Path,
    dir: &Path,
    log: &Arc<Log>,
    clock: Arc<dyn Clock>,
    tools: Vec<ToolInfo>,
    out: Box<dyn Write + Send>,
) -> Result<Session, Failure> {
    let (socket, listener) = bind(home, dir)?;
    let session_id = SessionId(
        dir.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    );
    let printer_watcher = log.watch();
    let gate = Arc::new(Gate {
        log: Arc::downgrade(log),
        clock: Arc::clone(&clock),
        session_id,
        tools,
        commands: Mutex::new(Vec::new()),
        inbox: Mutex::new(None),
        cancel: Mutex::new(None),
        driver_shell: Mutex::new(None),
        jobs: Mutex::new(None),
        hooks: Mutex::new(None),
        door: Mutex::new(None),
        accepted_commands: Mutex::new(std::collections::BTreeSet::new()),
        history: OnceLock::new(),
        shells: Mutex::new(RunningShells {
            stopped: false,
            running: Vec::new(),
        }),
        shell_ended: Condvar::new(),
        #[cfg(test)]
        probe: Mutex::new(None),
        stop: AtomicBool::new(false),
        clients: Mutex::new((0, false)),
        writers: Condvar::new(),
        conns: Mutex::new(Conns {
            live: Vec::new(),
            writers_open: 0,
            next: 1,
        }),
    });
    // The session's `Arc<Gate>` keeps this subscription alive: the weak
    // handle upgrades for as long as that allocation lives.
    let wake = Arc::clone(&gate);
    let wake: Arc<dyn Wake> = wake;
    clock.subscribe(Arc::downgrade(&wake));
    let printer = match spawn("stdout", move || print(printer_watcher, out)) {
        Ok(printer) => printer,
        Err(error) => {
            remove_socket(&socket);
            return Err(error);
        }
    };
    Ok(Session {
        dir: dir.to_owned(),
        socket,
        printer,
        gate,
        listener: Mutex::new(Some(listener)),
        accept: Mutex::new(None),
    })
}

fn spawn<F>(name: &str, body: F) -> Result<JoinHandle<()>, Failure>
where
    F: FnOnce() + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map_err(|e| failure(ErrorCode::IoFailed, format!("cannot start a thread: {e}")))
}

fn join(handle: JoinHandle<()>) {
    match handle.join() {
        Ok(()) | Err(_) => {}
    }
}

#[cfg(test)]
pub(crate) fn park_reader_for_test() {
    tests::park_reader();
}

/// Whether the session's log has a `turn_started`, read one line at a time
/// up to the first. A log that cannot be read, or a line that does not
/// parse before the first, keeps the session, so nothing is deleted on a
/// guess.
fn prompted(dir: &Path) -> bool {
    log::lines(dir).map_or(true, |mut lines| {
        lines.any(|line| {
            line.map_or(true, |line| {
                matches!(Event::from_envelope(&line), Ok(Some(Event::TurnStarted(_))))
            })
        })
    })
}

/// An acknowledgement that discards its answer. `fiber ask` has no client
/// waiting on one.
fn ignore() -> Ack {
    Ack(Box::new(|_| {}))
}

/// The first prompt as a driver message: `ask` and `serve` build it the
/// same way, and differ only in what follows it.
fn prompt_message(prompt: String) -> Message {
    Message {
        content: vec![ContentPart::Text { text: prompt }],
        sender: CommandSender {
            origin: Origin::Driver,
            command_id: Some(CommandId(mint("c_"))),
        },
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Copies every event to `out`, one JSON line each, until `fiber_exited` or
/// the end of the log. Durable lines serialize as the log wrote them, so
/// stdout filtered to them is `events.jsonl` byte for byte
/// (`docs/invocation.md`, "What a caller gets back").
fn print(mut watcher: Watcher, mut out: Box<dyn Write + Send>) {
    while let Ok(Some(line)) = watcher.recv() {
        if line.kind == client::STOP {
            return;
        }
        // A reader that went away, or a line that cannot be written, stops
        // the copy, never the session.
        if client::write_line(out.as_mut(), &line).is_err() {
            return;
        }
        if line.kind == "fiber_exited" {
            return;
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
pub(crate) mod tests;
