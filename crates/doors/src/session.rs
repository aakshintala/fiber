//! One session process's door side (`docs/invocation.md`, "Lifecycle" and
//! "Processes"): its socket at `run/<session_id>` in Fiber home, its event
//! stream copied to stdout, the clients on that socket, and what is left
//! when it exits.

use std::fs::{self, DirBuilder, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, UNIX_EPOCH};

use contract::clock::{Clock, Wake};
use contract::emit::Emit;
use contract::events::{Clients, Event, ToolInfo};
use contract::inbox::{Ack, Delivery, Message};
use contract::shapes::{ContentPart, Failure, Origin, Sender as CommandSender};
use contract::tool::Tool;
use contract::{CommandId, ErrorCode, SCHEMA_VERSION, SessionId};
use log::{Log, Watcher};
use serde_json::Map;

use crate::client;
use crate::{failure, mint};

/// The longest socket path the platform binds: `sun_path` less its
/// terminating byte (`docs/state.md`, "Sockets").
const SOCKET_PATH_MAX: usize = if cfg!(target_os = "macos") { 103 } else { 107 };

// debt: 2 s grace is picked, not measured; a slow client's measured drain time would set it.
/// How long [`Session::close`] waits for a connection's writer to finish
/// before it shuts the socket.
const GRACE: Duration = Duration::from_secs(2);

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
    pub(crate) dir: PathBuf,
    pub(crate) tools: Vec<ToolInfo>,
    inbox: Mutex<Option<Sender<Delivery>>>,
    /// What the `cancel` command asks: whether a turn is running. Stored
    /// by [`Session::run`], so a missing closure is no turn.
    cancel: Mutex<Option<Arc<dyn Fn() -> bool + Send + Sync>>>,
    /// The tool a driver `shell` runs. None leaves `shell` unknown.
    driver_shell: Mutex<Option<Arc<dyn Tool>>>,
    /// Driver shells running now, and whether `close` has stopped new ones.
    /// Both sit under this lock, so a shell that registers after `close`
    /// cannot miss the snapshot.
    shells: Mutex<RunningShells>,
    stop: AtomicBool,
    clients: Mutex<u32>,
    /// Paired with [`Gate::conns`].
    writers: Condvar,
    conns: Mutex<Conns>,
}

struct Live {
    reader: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    shutdown: Option<Box<dyn Fn() + Send + Sync>>,
}

struct Conns {
    live: Vec<(u64, Live)>,
    writers_open: u32,
    /// The next connection id. Starts at 1, so a missed store cannot look
    /// like the first connection.
    next: u64,
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
    /// `first` is queued before any client's command.
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
        let message = Message {
            content: vec![ContentPart::Text { text: prompt }],
            sender: CommandSender {
                origin: Origin::Driver,
                command_id: CommandId(mint("c_")),
            },
        };
        self.run(
            vec![
                Delivery::Prompt(message, ignore()),
                Delivery::Close(ignore()),
            ],
            cancel,
            run,
        )
    }

    /// The tool a driver `shell` runs (`docs/invocation.md`, "Shell").
    /// With none set, `shell` stays an unknown command.
    pub fn shell(&self, tool: Arc<dyn Tool>) {
        *lock(&self.gate.driver_shell) = Some(tool);
    }

    /// Ends the door side: stops accepting, unlinks the socket, drops `log`
    /// (the last handle, which releases the lock), waits up to [`GRACE`] for
    /// each writer, then shuts down whatever is still open. A driver shell
    /// is cancelled first: shutting its socket does not stop the tool.
    pub fn close(self, log: Arc<Log>) {
        self.gate.cancel_shells();
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
    fn wait_writers(&self) {
        let until = self.clock.now() + GRACE;
        let mut conns = lock(&self.conns);
        while conns.writers_open > 0 && grace_remains(self.clock.now(), until) {
            let writers = &self.writers;
            let mut slot = Some(conns);
            self.clock.wait_until(Some(until), &mut |bound| {
                let Some(guard) = slot.take() else {
                    return;
                };
                slot = Some(match bound {
                    Some(limit) => {
                        writers
                            .wait_timeout(guard, limit)
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                    None => writers.wait(guard).unwrap_or_else(PoisonError::into_inner),
                });
            });
            conns = match slot {
                Some(guard) => guard,
                None => lock(&self.conns),
            };
        }
    }

    fn join_clients(&self) {
        let live = {
            let mut conns = lock(&self.conns);
            std::mem::take(&mut conns.live)
        };
        for (_, live) in live {
            reap(live);
        }
    }

    /// One more `full` connection, and the `clients` line for it.
    pub(crate) fn attach(&self) {
        let mut count = lock(&self.clients);
        *count += 1;
        self.emit_clients(*count);
    }

    /// One fewer `full` connection, and the `clients` line for it.
    pub(crate) fn detach(&self) {
        let mut count = lock(&self.clients);
        *count = count.saturating_sub(1);
        self.emit_clients(*count);
    }

    fn emit_clients(&self, count: u32) {
        let Some(log) = self.log.upgrade() else {
            return;
        };
        log.emit(&Event::Clients(Clients { count }));
    }

    /// Stops a running turn and every driver shell. The shells are
    /// cancelled after the registry lock is released, so a shell's return
    /// can remove its entry without waiting on this call.
    pub(crate) fn stop_running(&self) -> Stopped {
        let turn = lock(&self.cancel).as_ref().is_some_and(|cancel| cancel());
        let shells = lock(&self.shells).running.clone();
        cancel_each(&shells);
        Stopped {
            turn,
            shell: !shells.is_empty(),
        }
    }

    /// Marks the gate so a shell that registers later is cancelled at once,
    /// and cancels the shells already running. Closing the socket does not
    /// stop a tool blocked in `run`.
    fn cancel_shells(&self) {
        let running = {
            let mut shells = lock(&self.shells);
            shells.stopped = true;
            shells.running.clone()
        };
        cancel_each(&running);
    }

    pub(crate) fn driver_shell(&self) -> Option<Arc<dyn Tool>> {
        lock(&self.driver_shell).clone()
    }

    pub(crate) fn track_shell(&self, cancel: Arc<crate::shell::ShellCancel>) {
        let stopped = {
            let mut shells = lock(&self.shells);
            if shells.stopped {
                true
            } else {
                shells.running.push(Arc::clone(&cancel));
                false
            }
        };
        // After the lock: `cancel` wakes the tool, which must not need this lock.
        if stopped {
            cancel.cancel();
        }
    }

    pub(crate) fn untrack_shell(&self, cancel: &Arc<crate::shell::ShellCancel>) {
        lock(&self.shells)
            .running
            .retain(|tracked| !Arc::ptr_eq(tracked, cancel));
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

    /// Records `handle` and `shutdown` together, and returns the id `serve`
    /// finishes the connection with. `close` joins the reader; the shutdown
    /// is what unblocks it. A published connection never lacks one.
    pub(crate) fn push_reader(
        &self,
        handle: JoinHandle<()>,
        shutdown: Box<dyn Fn() + Send + Sync>,
    ) -> u64 {
        let mut conns = lock(&self.conns);
        let id = conns.next;
        conns.next = conns.next.wrapping_add(1);
        conns.live.push((
            id,
            Live {
                reader: Some(handle),
                writer: None,
                shutdown: Some(shutdown),
            },
        ));
        id
    }

    pub(crate) fn push_writer(&self, id: u64, handle: JoinHandle<()>) {
        let mut conns = lock(&self.conns);
        if let Some((_, live)) = conns.live.iter_mut().find(|(slot, _)| *slot == id) {
            live.writer = Some(handle);
        }
        drop(conns);
        self.writers.notify_all();
    }

    /// Drops this connection's socket and joins its writer. The reader calls
    /// it as it exits.
    pub(crate) fn finish(&self, id: u64) {
        let taken = {
            let mut conns = lock(&self.conns);
            let pos = conns.live.iter().position(|(slot, _)| *slot == id);
            pos.map(|pos| conns.live.swap_remove(pos).1)
        };
        if let Some(live) = taken {
            reap(live);
        }
        self.writers.notify_all();
    }

    fn mark_stopped(&self) {
        let _conns = lock(&self.conns);
        self.stop.store(true, Ordering::Relaxed);
        self.writers.notify_all();
    }

    fn wait_for_room(&self) {
        let conns = lock(&self.conns);
        if self.stopped() {
            return;
        }
        #[cfg(test)]
        tests::note_accept_wait();
        drop(
            self.writers
                .wait(conns)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }

    pub(crate) fn begin_writer(&self) {
        lock(&self.conns).writers_open += 1;
    }

    pub(crate) fn end_writer(&self) {
        let mut conns = lock(&self.conns);
        conns.writers_open = conns.writers_open.saturating_sub(1);
        drop(conns);
        self.writers.notify_all();
    }

    pub(crate) fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
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

struct RunningShells {
    stopped: bool,
    running: Vec<Arc<crate::shell::ShellCancel>>,
}

fn cancel_each(shells: &[Arc<crate::shell::ShellCancel>]) {
    for shell in shells {
        shell.cancel();
    }
}

/// What [`Gate::stop_running`] found running.
pub(crate) struct Stopped {
    pub(crate) turn: bool,
    pub(crate) shell: bool,
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
        dir: dir.to_owned(),
        tools,
        inbox: Mutex::new(None),
        cancel: Mutex::new(None),
        driver_shell: Mutex::new(None),
        shells: Mutex::new(RunningShells {
            stopped: false,
            running: Vec::new(),
        }),
        stop: AtomicBool::new(false),
        clients: Mutex::new(0),
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

fn accept_loop(listener: UnixListener, gate: Arc<Gate>) {
    loop {
        if gate.stopped() {
            return;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if gate.stopped() {
                    return;
                }
                let Ok(shutdown_stream) = stream.try_clone() else {
                    continue;
                };
                let (tx, rx) = mpsc::channel();
                let child = Arc::clone(&gate);
                if let Ok(handle) = spawn("client", move || {
                    let Ok(id) = rx.recv() else {
                        return;
                    };
                    client::serve(stream, child, id);
                }) {
                    let id = gate.push_reader(handle, client::shutdown_both(shutdown_stream));
                    if tx.send(id).is_err() {
                        gate.finish(id);
                    }
                }
            }
            // `Interrupted` is a stale wake. Any other error, such as too
            // many open files, waits until a connection ends or the session
            // stops, so the loop does not spin.
            Err(error) => {
                if gate.stopped() {
                    return;
                }
                if accept_error_waits(error.kind()) {
                    gate.wait_for_room();
                }
            }
        }
    }
}

/// `Interrupted` is a stale wake and is retried. Any other accept error waits
/// so the loop does not spin.
fn accept_error_waits(kind: io::ErrorKind) -> bool {
    kind != io::ErrorKind::Interrupted
}

/// True while the grace has not been reached. An equal instant is the
/// deadline itself. Waiting on through it would spin: the clock does not
/// park for a time that has already arrived.
fn grace_remains(now: Instant, until: Instant) -> bool {
    now < until
}

/// Shuts the connection's socket and joins the threads still running on it.
/// A reader reaping itself detaches its own handle; joining it would deadlock.
fn reap(live: Live) {
    if let Some(shutdown) = live.shutdown {
        shutdown();
    }
    if let Some(writer) = live.writer {
        join(writer);
    }
    if let Some(reader) = live.reader {
        if reader.thread().id() == thread::current().id() {
            drop(reader);
        } else {
            join(reader);
        }
    }
}

#[cfg(test)]
pub(crate) fn park_reader_for_test() {
    tests::park_reader();
}

/// Whether the session's log has a `turn_started`. A log that cannot be read
/// is kept, so nothing is deleted on a guess.
fn prompted(dir: &Path) -> bool {
    log::read(dir).map_or(true, |lines| {
        lines
            .iter()
            .any(|line| matches!(Event::from_envelope(line), Ok(Some(Event::TurnStarted(_)))))
    })
}

/// Binds the session's socket at `run/<session_id>`, mode 0600 in a 0700
/// directory. The session's lock holder owns the socket, so a stale one left
/// by a dead process is removed first.
fn bind(home: &Path, dir: &Path) -> Result<(PathBuf, UnixListener), Failure> {
    let run = home.join("run");
    let socket = run.join(dir.file_name().unwrap_or_default());
    if socket.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(failure(
            ErrorCode::Usage,
            format!(
                "FIBER_HOME is too long: a session's socket path must fit in \
                 {SOCKET_PATH_MAX} bytes. Set FIBER_HOME to a shorter path."
            ),
        ));
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run)
        .map_err(|e| io_failed(&run, &e))?;
    remove_socket(&socket);
    let listener = UnixListener::bind(&socket).map_err(|e| io_failed(&socket, &e))?;
    if let Err(e) = fs::set_permissions(&socket, Permissions::from_mode(0o600)) {
        remove_socket(&socket);
        return Err(io_failed(&socket, &e));
    }
    Ok((socket, listener))
}

fn remove_socket(socket: &Path) {
    // Nothing there is the usual case.
    fs::remove_file(socket).unwrap_or(());
}

/// An acknowledgement that discards its answer. `fiber ask` has no client
/// waiting on one.
fn ignore() -> Ack {
    Ack(Box::new(|_| {}))
}

fn io_failed(path: &Path, e: &io::Error) -> Failure {
    failure(ErrorCode::IoFailed, format!("{}: {e}", path.display()))
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

/// Milliseconds since the epoch, for an acknowledgement's `ts`.
pub(crate) fn now_ms(clock: &dyn Clock) -> u64 {
    clock
        .wall()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// An envelope doors builds itself: an acknowledgement, or a control line
/// that never leaves the process.
pub(crate) fn envelope(
    session: &SessionId,
    clock: &dyn Clock,
    event: &Event,
) -> contract::Envelope {
    contract::Envelope {
        kind: event.kind().to_owned(),
        session_id: session.clone(),
        ts: now_ms(clock),
        schema_version: SCHEMA_VERSION,
        turn_id: None,
        action_id: None,
        seq: None,
        payload: match event.payload() {
            Ok(payload) => payload,
            Err(_) => Map::new(),
        },
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
