//! Session-side keeper lifetime, line transport and bounded start attempts.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use contract::clock::{Clock, Wake};
use contract::keeper::{
    CommandHost, FromKeeper, HostError, HostEvent, KeptJob, Spawn, Started, ToKeeper,
};
use contract::{JobId, SessionId};
use support::peer::Pid;

pub(crate) const KEEPER_START: Duration = Duration::from_millis(5000);
type Spawner = Arc<dyn Fn() -> io::Result<Connection> + Send + Sync>;
struct Connection {
    stream: UnixStream,
    lifeline: Box<dyn Send + Sync>,
    child: Option<Child>,
}
struct Pending {
    reply: mpsc::Sender<Result<Started, HostError>>,
    events: mpsc::Sender<HostEvent>,
}
struct Client {
    writer: Mutex<UnixStream>,
    _lifeline: Box<dyn Send + Sync>,
    pending: Mutex<HashMap<String, Pending>>,
    waiters: Mutex<HashMap<String, mpsc::Sender<HostEvent>>>,
    adopted: Mutex<HashMap<JobId, String>>,
    changed: Arc<support::clock::Parker>,
    dead: AtomicBool,
    /// The kernel's answer for the process on the other end, for a keeper
    /// that handed its jobs over; never read from arguments or the wire.
    peer: Option<Pid>,
    /// Where `adopt_done` goes: set only on a client that took a handover.
    done: Mutex<Option<mpsc::Sender<()>>>,
}
pub(crate) struct Host {
    slot: Arc<Mutex<Option<Arc<Client>>>>,
    /// Keepers that handed their jobs over, each kept until its last job's
    /// ending is confirmed. None of them is the slot a new command starts in.
    retained: Arc<Mutex<Vec<Arc<Client>>>>,
    spawner: Spawner,
    clock: Arc<dyn Clock>,
    #[cfg(test)]
    clients: Mutex<mpsc::Receiver<Weak<Client>>>,
    #[cfg(test)]
    client_sender: mpsc::Sender<Weak<Client>>,
}
/// Reads one line from a peer within the wire's line cap, without its line
/// feed. `Ok(None)` is a clean end; a line cut short or longer than the cap
/// is `InvalidData`, never handed on to a parser.
pub(crate) fn read_frame<R: BufRead>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let cap = support::framing::MAX_LINE;
    let mut line = Vec::new();
    let read = Read::by_ref(reader)
        .take(u64::try_from(cap).unwrap_or(u64::MAX).saturating_add(1))
        .read_until(b'\n', &mut line)?;
    if read == 0 {
        return Ok(None);
    }
    if line.pop() != Some(b'\n') || line.len() > cap {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a line is cut short or longer than its cap",
        ));
    }
    Ok(Some(line))
}

/// The next request a candidate sends the keeper.
pub(crate) fn read_request<R: BufRead>(reader: &mut R) -> io::Result<Option<ToKeeper>> {
    read_frame(reader)?
        .map(|line| serde_json::from_slice(&line).map_err(io::Error::from))
        .transpose()
}

/// The next line the keeper sends a session.
pub(crate) fn read_reply<R: BufRead>(reader: &mut R) -> io::Result<Option<FromKeeper>> {
    read_frame(reader)?
        .map(|line| serde_json::from_slice(&line).map_err(io::Error::from))
        .transpose()
}

/// Closes a connection at a deadline on the injected clock unless it is
/// finished first, so a read blocked on the connection ends instead of
/// outliving its wait.
pub(crate) struct Expiry {
    done: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    parker: Arc<support::clock::Parker>,
    worker: thread::JoinHandle<()>,
}

impl Expiry {
    pub(crate) fn start(clock: &Arc<dyn Clock>, stream: &UnixStream, within: Duration) -> Self {
        let parker = Arc::new(support::clock::Parker::new());
        let wake: Arc<dyn Wake> = parker.clone();
        clock.subscribe(Arc::downgrade(&wake));
        let until = clock.now().checked_add(within);
        let done = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let worker = thread::spawn({
            let (clock, parker) = (Arc::clone(clock), Arc::clone(&parker));
            let (done, fired) = (Arc::clone(&done), Arc::clone(&fired));
            let stream = stream.try_clone().ok();
            move || {
                loop {
                    let seen = parker.generation();
                    if done.load(Ordering::Acquire) {
                        return;
                    }
                    if until.is_some_and(|at| clock.now() >= at) {
                        fired.store(true, Ordering::Release);
                        if let Some(stream) = &stream {
                            match stream.shutdown(std::net::Shutdown::Both) {
                                Ok(()) | Err(_) => {}
                            }
                        }
                        return;
                    }
                    parker.park(&*clock, until, seen);
                }
            }
        });
        Self {
            done,
            fired,
            parker,
            worker,
        }
    }

    /// Cancels the wait and joins its thread. True when the deadline had
    /// already closed the connection.
    pub(crate) fn finish(self) -> bool {
        self.done.store(true, Ordering::Release);
        self.parker.bump();
        match self.worker.join() {
            Ok(()) | Err(_) => {}
        }
        self.fired.load(Ordering::Acquire)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
fn error(id: &str, io: impl ToString) -> HostError {
    HostError {
        id: id.to_owned(),
        io: io.to_string(),
    }
}
impl Host {
    fn new(spawner: Spawner, clock: Arc<dyn Clock>) -> Arc<Self> {
        #[cfg(test)]
        let (client_sender, clients) = mpsc::channel();
        Arc::new(Self {
            slot: Arc::new(Mutex::new(None)),
            retained: Arc::new(Mutex::new(Vec::new())),
            spawner,
            clock,
            #[cfg(test)]
            clients: Mutex::new(clients),
            #[cfg(test)]
            client_sender,
        })
    }
    pub(crate) fn session(
        fiber: PathBuf,
        home: PathBuf,
        id: SessionId,
        clock: Arc<dyn Clock>,
    ) -> Arc<Self> {
        Self::new(
            Arc::new(move || {
                let (stream, child_end) = UnixStream::pair()?;
                let mut command = Command::new(&fiber);
                command
                    .args(["keeper", "--id", &id.0])
                    .env("FIBER_HOME", &home)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::from(std::os::fd::OwnedFd::from(child_end)))
                    .stderr(Stdio::null());
                support::group::detach(&mut command, false);
                let mut child = command.spawn()?;
                let Some(lifeline) = child.stdin.take() else {
                    match child.kill() {
                        Ok(()) | Err(_) => {}
                    }
                    match child.wait() {
                        Ok(_) | Err(_) => {}
                    }
                    return Err(io::Error::other("keeper lifeline missing"));
                };
                Ok(Connection {
                    stream,
                    lifeline: Box::new(lifeline),
                    child: Some(child),
                })
            }),
            clock,
        )
    }
    /// Keep the guard until the session closes. Only durable job ends confirm,
    /// each on the connection that holds the job.
    pub(crate) fn observe(self: &Arc<Self>, log: &log::Log) -> log::Subscription {
        let host = Arc::downgrade(self);
        log.subscribe(Arc::new(move |event, _| {
            if let contract::events::Event::JobCompleted(end) = event
                && let Some(host) = host.upgrade()
            {
                for client in host.clients() {
                    if let Some(id) = lock(&client.adopted).remove(&end.job_id) {
                        match client.send(&ToKeeper::Confirmed(id)) {
                            Ok(()) | Err(_) => {}
                        }
                        break;
                    }
                }
            }
        }))
    }
    pub(crate) fn send_shutdown(&self) {
        for client in self.clients() {
            match client.send(&ToKeeper::Shutdown(true)) {
                Ok(()) | Err(_) => {}
            }
        }
    }
    /// The current keeper for new commands, then every keeper that handed
    /// its jobs over.
    fn clients(&self) -> Vec<Arc<Client>> {
        let mut clients: Vec<_> = lock(&self.slot).iter().cloned().collect();
        clients.extend(lock(&self.retained).iter().cloned());
        clients
    }
    /// The kernel's process ids for the keepers that handed their jobs over
    /// and still hold some, for a permission check to attribute a socket to
    /// them (#4042). The ids come from the connection, never from arguments.
    #[allow(
        dead_code,
        reason = "the permission check of the session sockets reads the credential; only tests do yet"
    )]
    pub(crate) fn handover_peers(&self) -> Vec<Pid> {
        lock(&self.retained)
            .iter()
            .filter(|client| !client.dead.load(Ordering::Acquire))
            .filter_map(|client| client.peer)
            .collect()
    }
    /// Takes over the connection of a keeper that handed its jobs over. The
    /// connection is retained for as long as the keeper holds jobs, and is
    /// never the slot a new command starts in: its call identifiers are the
    /// old keeper's own and may repeat the new keeper's.
    pub(crate) fn attach_handover(self: &Arc<Self>, handover: Handover) -> io::Result<Attached> {
        let Handover {
            stream,
            reader,
            pid,
            running,
        } = handover;
        let mut waiters = HashMap::new();
        let mut adopted = HashMap::new();
        let mut kept = Vec::new();
        for job in running {
            let (events, received) = mpsc::channel();
            if waiters.insert(job.id.clone(), events).is_some()
                || adopted.insert(job.job_id.clone(), job.id.clone()).is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "the keeper handed over one job twice",
                ));
            }
            kept.push((job, received));
        }
        let (done, finished) = mpsc::channel();
        let client = Client::start(
            self,
            Parts {
                writer: stream,
                reader,
                lifeline: Box::new(()),
                child: None,
                peer: Some(pid),
                waiters,
                adopted,
                done: Some(done),
            },
        )?;
        lock(&self.retained).push(Arc::clone(&client));
        let owner = Owner(Arc::clone(&client));
        Ok(Attached {
            jobs: kept
                .into_iter()
                .map(|(job, events)| Adopted {
                    job,
                    events,
                    owner: owner.clone(),
                })
                .collect(),
            client,
            finished,
            retained: Arc::downgrade(&self.retained),
            clock: Arc::clone(&self.clock),
            keep: false,
        })
    }
    fn live(&self, id: &str) -> Result<Arc<Client>, HostError> {
        lock(&self.slot)
            .as_ref()
            .filter(|c| !c.dead.load(Ordering::Acquire))
            .cloned()
            .ok_or_else(|| error(id, "keeper connection closed"))
    }
    fn start_on(
        &self,
        client: &Arc<Client>,
        spec: Spawn,
        events: mpsc::Sender<HostEvent>,
    ) -> Result<Started, HostError> {
        let id = spec.id.clone();
        let (reply, received) = mpsc::channel();
        if lock(&client.waiters).contains_key(&id) {
            return Err(error(&id, "call already started"));
        }
        {
            let mut pending = lock(&client.pending);
            if client.dead.load(Ordering::Acquire) {
                return Err(error(&id, "keeper connection closed"));
            }
            if pending.contains_key(&id) {
                return Err(error(&id, "call already started"));
            }
            pending.insert(id.clone(), Pending { reply, events });
        }
        if let Err(failure) = client.send(&ToKeeper::Start(spec)) {
            client.retire(&id, Err(error(&id, failure)));
        }
        let until = self.clock.now().checked_add(KEEPER_START);
        loop {
            let seen = client.changed.generation();
            match received.try_recv() {
                Ok(result) => return result,
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(error(&id, "keeper connection closed"));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if until.is_none_or(|until| self.clock.now() >= until) {
                client.retire(&id, Err(error(&id, "keeper start timed out")));
                // Another retirement may have removed the entry and be delivering its reply.
                continue;
            }
            client.changed.park(&*self.clock, until, seen);
        }
    }
}
impl CommandHost for Host {
    fn start(&self, spec: Spawn, events: mpsc::Sender<HostEvent>) -> Result<Started, HostError> {
        let mut slot = lock(&self.slot);
        if slot
            .as_ref()
            .is_none_or(|client| client.dead.load(Ordering::Acquire))
        {
            let connection = (self.spawner)().map_err(|failure| error(&spec.id, failure))?;
            *slot =
                Some(Client::new(connection, self).map_err(|failure| error(&spec.id, failure))?);
        }
        let Some(client) = slot.as_ref() else {
            return Err(error(&spec.id, "keeper connection closed"));
        };
        self.start_on(client, spec, events)
    }
    fn stop(&self, id: &str) -> bool {
        self.live(id)
            .is_ok_and(|client| client.send(&ToKeeper::Stop(id.to_owned())).is_ok())
    }
    fn write(&self, id: &str, text: &str) -> Result<(), HostError> {
        self.live(id)?
            .send(&ToKeeper::Write {
                id: id.to_owned(),
                text: text.to_owned(),
            })
            .map_err(|failure| error(id, failure))
    }
    fn adopt(
        &self,
        id: &str,
        job_id: &JobId,
        output_path: &Path,
        errors_path: Option<&Path>,
        lines: bool,
    ) -> Result<(), HostError> {
        let client = self.live(id)?;
        lock(&client.adopted).insert(job_id.clone(), id.to_owned());
        client
            .send(&ToKeeper::Adopt {
                id: id.to_owned(),
                job_id: job_id.clone(),
                output_path: output_path.to_path_buf(),
                errors_path: errors_path.map(Path::to_path_buf),
                lines,
            })
            .map_err(|failure| error(id, failure))
    }
}
/// What a [`Client`] starts from: its connection and what it already knows.
struct Parts {
    writer: UnixStream,
    reader: BufReader<UnixStream>,
    lifeline: Box<dyn Send + Sync>,
    child: Option<Child>,
    peer: Option<Pid>,
    waiters: HashMap<String, mpsc::Sender<HostEvent>>,
    adopted: HashMap<JobId, String>,
    done: Option<mpsc::Sender<()>>,
}
impl Client {
    fn new(connection: Connection, host: &Host) -> io::Result<Arc<Self>> {
        let reader = BufReader::new(connection.stream.try_clone()?);
        Self::start(
            host,
            Parts {
                writer: connection.stream,
                reader,
                lifeline: connection.lifeline,
                child: connection.child,
                peer: None,
                waiters: HashMap::new(),
                adopted: HashMap::new(),
                done: None,
            },
        )
    }
    fn start(host: &Host, parts: Parts) -> io::Result<Arc<Self>> {
        let Parts {
            writer,
            mut reader,
            lifeline,
            child,
            peer,
            waiters,
            adopted,
            done,
        } = parts;
        let changed = Arc::new(support::clock::Parker::new());
        let waker: Arc<dyn Wake> = changed.clone();
        host.clock.subscribe(Arc::downgrade(&waker));
        let client = Arc::new(Self {
            writer: Mutex::new(writer),
            _lifeline: lifeline,
            pending: Mutex::new(HashMap::new()),
            waiters: Mutex::new(waiters),
            adopted: Mutex::new(adopted),
            changed,
            dead: AtomicBool::new(false),
            peer,
            done: Mutex::new(done),
        });
        #[cfg(test)]
        match host.client_sender.send(Arc::downgrade(&client)) {
            Ok(()) | Err(_) => {}
        }
        let weak = Arc::downgrade(&client);
        let slot = Arc::downgrade(&host.slot);
        let retained = Arc::downgrade(&host.retained);
        thread::spawn(move || {
            // The reader a handover negotiated on keeps its read-ahead: a
            // frame the keeper sent behind its acknowledgement is not lost.
            while let Ok(Some(reply)) = read_reply(&mut reader) {
                let Some(client) = weak.upgrade() else {
                    break;
                };
                if client.reply(reply).is_err() {
                    break;
                }
            }
            if let Some(client) = weak.upgrade() {
                client.dead.store(true, Ordering::Release);
                let ids: Vec<_> = lock(&client.pending).keys().cloned().collect();
                for id in ids {
                    client.retire(&id, Err(error(&id, "keeper connection closed")));
                }
                lock(&client.waiters).clear();
                *lock(&client.done) = None;
                client.changed.bump();
                if let Some(slot) = slot.upgrade() {
                    let mut slot = lock(&slot);
                    if slot.as_ref().is_some_and(|live| Arc::ptr_eq(live, &client)) {
                        *slot = None;
                    }
                }
                // Only this connection's own entry goes: a keeper that
                // ended never retires the slot a newer one holds.
                if let Some(retained) = retained.upgrade() {
                    lock(&retained).retain(|held| !Arc::ptr_eq(held, &client));
                }
            }
            if let Some(mut child) = child {
                match child.wait() {
                    Ok(_) | Err(_) => {}
                }
            }
        });
        Ok(client)
    }
    fn send(&self, line: &ToKeeper) -> io::Result<()> {
        let mut writer = lock(&self.writer);
        serde_json::to_writer(&mut *writer, line)?;
        writer.write_all(b"\n")
    }
    /// First remover owns completion; send outside pending and before clearing slot.
    fn retire(&self, id: &str, result: Result<Started, HostError>) {
        let pending = lock(&self.pending).remove(id);
        if let Some(pending) = pending {
            if result.is_ok() {
                lock(&self.waiters).insert(id.to_owned(), pending.events);
            }
            match pending.reply.send(result) {
                Ok(()) | Err(_) => {}
            }
            self.changed.bump();
        }
    }
    fn reply(&self, reply: FromKeeper) -> io::Result<()> {
        match reply {
            FromKeeper::Started { id, pgid } => self.retire(&id, Ok(Started { pgid })),
            FromKeeper::Failed { id, error: failure } => self.retire(&id, Err(error(&id, failure))),
            FromKeeper::Output { id, stream, bytes } => {
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(bytes)
                    .map_err(io::Error::other)?;
                if let Some(events) = lock(&self.waiters).get(&id) {
                    match events.send(HostEvent::Output { stream, bytes }) {
                        Ok(()) | Err(_) => {}
                    }
                }
            }
            FromKeeper::Exit { id, exit } => {
                if let Some(events) = lock(&self.waiters).remove(&id) {
                    match events.send(HostEvent::Exit(exit)) {
                        Ok(()) | Err(_) => {}
                    }
                }
            }
            // Only a client that took a handover expects the keeper's
            // acknowledgement that every job now sends frames; any other
            // frame of the negotiation, on any client, is out of place.
            FromKeeper::AdoptDone(true) => {
                let Some(done) = lock(&self.done).take() else {
                    return Err(io::Error::other("handover frame on an ordinary client"));
                };
                match done.send(()) {
                    Ok(()) | Err(_) => {}
                }
                self.changed.bump();
            }
            FromKeeper::AdoptDone(false)
            | FromKeeper::Identity { .. }
            | FromKeeper::Handover { .. } => {
                return Err(io::Error::other("handover frame out of place"));
            }
        }
        Ok(())
    }
}

/// A keeper that released the session's log and listed the jobs it still
/// runs (`docs/tools.md`, "The keeper"). Holding it holds the connection and
/// the framing reader, so a frame the keeper sent behind its answer is not
/// lost when the connection becomes a client. Dropping it ends the
/// connection, and the keeper takes the log back.
pub(crate) struct Handover {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    pid: Pid,
    running: Vec<KeptJob>,
}

impl Handover {
    /// The jobs the keeper still runs, each with its files.
    pub(crate) fn running(&self) -> &[KeptJob] {
        &self.running
    }

    /// The keeper's process id, as the kernel reports it for this connection.
    #[allow(
        dead_code,
        reason = "the permission check of the session sockets reads the credential; only tests do yet"
    )]
    pub(crate) fn pid(&self) -> Pid {
        self.pid
    }
}

fn send_to(stream: &UnixStream, request: &ToKeeper) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    let mut stream = stream;
    stream.write_all(&bytes)
}

fn refused(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason.to_owned())
}

/// Asks the keeper that holds `id`'s log, on `run/<session_id>`, to hand over
/// the log and its jobs (`docs/tools.md`, "The keeper"). The caller then
/// opens the log. The keeper's answer to its own identity is the proof; the
/// connection is not trusted until the id matches the session selected, and
/// nothing is released before it does.
pub(crate) fn takeover(
    home: &Path,
    id: &SessionId,
    clock: &Arc<dyn Clock>,
) -> io::Result<Handover> {
    negotiate(
        UnixStream::connect(home.join("run").join(&id.0))?,
        id,
        clock,
    )
}

/// Identity and the log's release, within one deadline from the first byte.
fn negotiate(stream: UnixStream, id: &SessionId, clock: &Arc<dyn Clock>) -> io::Result<Handover> {
    // The credential is the kernel's answer for this very connection.
    let pid = support::peer::peer_pid(&stream).map_err(io::Error::other)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let expiry = Expiry::start(clock, &stream, KEEPER_START);
    let negotiated = (|| {
        send_to(&stream, &ToKeeper::Identify(id.clone()))?;
        match read_reply(&mut reader)? {
            Some(FromKeeper::Identity { session_id }) if &session_id == id => {}
            Some(_) | None => return Err(refused("the keeper holds another session")),
        }
        send_to(&stream, &ToKeeper::Handover(true))?;
        match read_reply(&mut reader)? {
            Some(FromKeeper::Handover { running }) => Ok(running),
            Some(_) | None => Err(refused("the keeper did not hand over")),
        }
    })();
    let expired = expiry.finish();
    match negotiated {
        Ok(running) if !expired => Ok(Handover {
            stream,
            reader,
            pid,
            running,
        }),
        Ok(_) | Err(_) if expired => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the keeper did not answer in time",
        )),
        Err(error) => Err(error),
        Ok(_) => Err(refused("the handover ended")),
    }
}

/// A handed-over job's control, on the connection of the keeper that runs it.
#[derive(Clone)]
pub(crate) struct Owner(Arc<Client>);

impl Owner {
    /// Asks the owning keeper to stop the job's process.
    pub(crate) fn stop(&self, id: &str) -> bool {
        !self.0.dead.load(Ordering::Acquire) && self.0.send(&ToKeeper::Stop(id.to_owned())).is_ok()
    }

    /// Writes into the job's terminal on the owning keeper.
    pub(crate) fn write(&self, id: &str, text: &str) -> Result<(), HostError> {
        if self.0.dead.load(Ordering::Acquire) {
            return Err(error(id, "keeper connection closed"));
        }
        self.0
            .send(&ToKeeper::Write {
                id: id.to_owned(),
                text: text.to_owned(),
            })
            .map_err(|failure| error(id, failure))
    }
}

/// One handed-over job with its events and its keeper's control.
pub(crate) struct Adopted {
    pub(crate) job: KeptJob,
    pub(crate) events: mpsc::Receiver<HostEvent>,
    pub(crate) owner: Owner,
}

/// A handover whose connection the host retains. Dropped before [`keep`],
/// it ends the connection, so the keeper takes the log back.
///
/// [`keep`]: Attached::keep
pub(crate) struct Attached {
    jobs: Vec<Adopted>,
    client: Arc<Client>,
    finished: mpsc::Receiver<()>,
    retained: Weak<Mutex<Vec<Arc<Client>>>>,
    clock: Arc<dyn Clock>,
    keep: bool,
}

impl Attached {
    /// The jobs to restore, once.
    pub(crate) fn take_jobs(&mut self) -> Vec<Adopted> {
        std::mem::take(&mut self.jobs)
    }

    /// Tells the keeper the session holds the lock and every runner is ready,
    /// then waits for its acknowledgement that each job now sends frames.
    /// One deadline covers the wait. Output and endings are already flowing
    /// to the runners, so the acknowledgement is not a delivery barrier.
    pub(crate) fn ready(&self) -> io::Result<()> {
        self.client.send(&ToKeeper::Adopted(true))?;
        let until = self.clock.now().checked_add(KEEPER_START);
        loop {
            let seen = self.client.changed.generation();
            match self.finished.try_recv() {
                Ok(()) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "the keeper went away before it finished handing over",
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if until.is_none_or(|until| self.clock.now() >= until) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the keeper did not finish handing over in time",
                ));
            }
            self.client.changed.park(&*self.clock, until, seen);
        }
    }

    /// The handover is complete: the host keeps the connection until the
    /// keeper has no job left.
    pub(crate) fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        if self.keep {
            return;
        }
        match lock(&self.client.writer).shutdown(std::net::Shutdown::Both) {
            Ok(()) | Err(_) => {}
        }
        if let Some(retained) = self.retained.upgrade() {
            lock(&retained).retain(|held| !Arc::ptr_eq(held, &self.client));
        }
    }
}

#[cfg(test)]
#[path = "keeper_tests.rs"]
mod tests;
