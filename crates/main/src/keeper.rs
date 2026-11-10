//! Session-side keeper lifetime, line transport and bounded start attempts.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(test)]
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::thread;
use std::time::Duration;

use base64::Engine as _;
use contract::clock::{Clock, Wake};
use contract::keeper::{CommandHost, FromKeeper, HostError, HostEvent, Spawn, Started, ToKeeper};
use contract::{JobId, SessionId};

const KEEPER_START: Duration = Duration::from_millis(5000);
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
}
pub(crate) struct Host {
    slot: Arc<Mutex<Option<Arc<Client>>>>,
    spawner: Spawner,
    clock: Arc<dyn Clock>,
    #[cfg(test)]
    clients: Mutex<mpsc::Receiver<Weak<Client>>>,
    #[cfg(test)]
    client_sender: mpsc::Sender<Weak<Client>>,
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
    /// Tests only: a host that never started a keeper, so shutdown sends
    /// nothing and no child exists to reap.
    #[cfg(test)]
    pub(crate) fn idle(clock: Arc<dyn Clock>) -> Arc<Self> {
        Self::new(Arc::new(|| Err(std::io::Error::other("no keeper"))), clock)
    }
    fn new(spawner: Spawner, clock: Arc<dyn Clock>) -> Arc<Self> {
        #[cfg(test)]
        let (client_sender, clients) = mpsc::channel();
        Arc::new(Self {
            slot: Arc::new(Mutex::new(None)),
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
    /// Keep the guard until the session closes. Only durable job ends confirm.
    pub(crate) fn observe(self: &Arc<Self>, log: &log::Log) -> log::Subscription {
        let host = Arc::downgrade(self);
        log.subscribe(Arc::new(move |event, _| {
            if let contract::events::Event::JobCompleted(end) = event
                && let Some(host) = host.upgrade()
                && let Some(client) = lock(&host.slot).as_ref()
                && let Some(id) = lock(&client.adopted).remove(&end.job_id)
            {
                match client.send(&ToKeeper::Confirmed(id)) {
                    Ok(()) | Err(_) => {}
                }
            }
        }))
    }
    pub(crate) fn send_shutdown(&self) {
        if let Some(client) = lock(&self.slot).as_ref() {
            match client.send(&ToKeeper::Shutdown(true)) {
                Ok(()) | Err(_) => {}
            }
        }
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
    ) -> Result<(), HostError> {
        let client = self.live(id)?;
        lock(&client.adopted).insert(job_id.clone(), id.to_owned());
        client
            .send(&ToKeeper::Adopt {
                id: id.to_owned(),
                job_id: job_id.clone(),
                output_path: output_path.to_path_buf(),
                errors_path: errors_path.map(Path::to_path_buf),
            })
            .map_err(|failure| error(id, failure))
    }
}
impl Client {
    fn new(connection: Connection, host: &Host) -> io::Result<Arc<Self>> {
        let reader = connection.stream.try_clone()?;
        let changed = Arc::new(support::clock::Parker::new());
        let waker: Arc<dyn Wake> = changed.clone();
        host.clock.subscribe(Arc::downgrade(&waker));
        let client = Arc::new(Self {
            writer: Mutex::new(connection.stream),
            _lifeline: connection.lifeline,
            pending: Mutex::new(HashMap::new()),
            waiters: Mutex::new(HashMap::new()),
            adopted: Mutex::new(HashMap::new()),
            changed,
            dead: AtomicBool::new(false),
        });
        #[cfg(test)]
        match host.client_sender.send(Arc::downgrade(&client)) {
            Ok(()) | Err(_) => {}
        }
        let weak = Arc::downgrade(&client);
        let slot = Arc::downgrade(&host.slot);
        thread::spawn(move || {
            for line in BufReader::new(reader).lines() {
                let Some(client) = weak.upgrade() else {
                    break;
                };
                let Ok(line) = line else {
                    break;
                };
                let Ok(reply) = serde_json::from_str::<FromKeeper>(&line) else {
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
                if let Some(slot) = slot.upgrade() {
                    let mut slot = lock(&slot);
                    if slot.as_ref().is_some_and(|live| Arc::ptr_eq(live, &client)) {
                        *slot = None;
                    }
                }
            }
            if let Some(mut child) = connection.child {
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
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "keeper_tests.rs"]
mod tests;
