//! One client connection: `hub_hello` first, hub-command dispatch for
//! `start`, `status`, `prompt_history`, `feed`, `dismiss`, `recent`,
//! `sessions`, `delete`, `directories`, `read_file`, `pairing_code`, `devices`
//! and `revoke`, the machine's `configure`, `read_config`, `write_config`,
//! `search_files`, `device_login`, `device_login_result` and `device_login_cancel`,
//! and the relay to session sockets. Every connection also hears
//! `attention` and `device_changed` (`crate::attention`) after its hello.
//!
//! A command with a `session_id` is for that session: the relay passes it
//! to the session's socket (`crate::relay`). A command without one is for
//! the hub. Every connection
//! opens with `hub_hello`, before any acknowledgement.

use std::collections::HashMap;
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use contract::clock::{Clock, Wake, wall_ms};
use contract::commands::SentPart;
use contract::diag::DebugLog;
use contract::events::CommandResult;
use contract::events::VariablesSource;
use contract::{CommandId, ErrorCode, HubLine, SCHEMA_VERSION, SessionId};
use log::diag::Severity;
use serde_json::{Map, Value};

use crate::Starter;
use crate::feed::{Feed, answer, on_feed};
use crate::first::{FIRST_PROMPT_WAIT, First};
use crate::relay::Relays;
use crate::start::{self, Outcome};
use crate::tick::Tick;

/// What the hub shares across its connections: home, the version it
/// reports, how it starts sessions, the clock, the diagnostic log, and the
/// open connections `status` answers with.
pub(crate) struct Hub {
    /// Fiber home: `run/<session_id>` is resolved under it.
    pub(crate) home: PathBuf,
    /// The version `hub_hello` and `status` report.
    pub(crate) fiber_version: String,
    /// How `start` runs the session command.
    pub(crate) starter: Arc<dyn Starter>,
    /// The injected clock, for `ts`, `start`'s deadline and the idle wait.
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) diag: log::diag::Diag,
    /// The feed `feed`, `dismiss` and `recent` answer from.
    pub(crate) feed: Arc<Feed>,
    /// Pending pairing codes and authenticated connections.
    pub(crate) pairing: crate::pairing::Pairing,
    pub(crate) machine: Option<Arc<dyn crate::Machine>>,
    next_client: AtomicU64,
    /// The open connections and the idle timer, under one lock.
    conns: Mutex<Conns>,
    /// Woken on every clock move, every change to `conns` and every signal.
    pub(crate) tick: Arc<Tick>,
    /// Set while a `stop` with `sessions` runs: admission refuses new
    /// binds with `the hub is stopping.` No store without the exit after
    /// it: only the serving thread sets it, and the hub exits after
    /// answering.
    pub(crate) sessions_stopping: AtomicBool,
    /// Binds in flight: incremented before the `sessions_stopping` load
    /// in admission, decremented after the bind lands or the starter is
    /// killed. The sessions stop waits for zero before each scan, so no
    /// session admitted before the store escapes it.
    pub(crate) inflight_binds: AtomicUsize,
    /// `stop` commands with `sessions` now answering: incremented on
    /// entry, before `sessions_stopping` is stored, decremented only
    /// after the answer is written and, on a websocket, its drain
    /// barrier ends. The Signal arm aborts the sessions stop only while
    /// this is above 0, and waits only for its flush.
    pub(crate) sessions_answering: AtomicUsize,
    /// Set by a local `stop`: the idle wait ends the hub as SIGTERM does.
    stop: AtomicBool,
    /// `tick` as the clock's subscriber: kept alive so advances wake the
    /// idle wait.
    wake: Arc<dyn Wake>,
    /// Held across a resume: one at a time per hub, so two commands for
    /// one exited session start one process.
    pub(crate) resume_gate: Mutex<()>,
    /// The environment the hub's sessions start with: the login-shell
    /// capture's publish, once the first capture ends (`docs/invocation.md`,
    /// "A session's environment").
    pub(crate) environments: Arc<crate::Environments>,
    /// Every served connection's writer and relays, for the rejoin sweep.
    pub(crate) rejoins: crate::rejoin::Connections,
    /// One start at a time per next session: a slow rewind start blocks
    /// only the threads starting that same session, never a resume, which
    /// takes the shared gate instead (`docs/invocation.md`, "`rewind`
    /// starts a new session process").
    pub(crate) starting: Mutex<HashMap<SessionId, Arc<Mutex<()>>>>,
    /// Tests only: a one-shot pause run before `hub_hello` is sent.
    #[cfg(test)]
    pub(crate) before_hello: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: a one-shot pause before the next forwarded session line.
    #[cfg(test)]
    pub(crate) before_forward: ForwardHook,
    /// Tests only: observes and pauses after the relay filters a session line.
    #[cfg(test)]
    pub(crate) after_replay_filter: ReplayFilterHook,
    /// Tests only: a one-shot pause before an accepted `subscribe` is kept.
    #[cfg(test)]
    pub(crate) before_accepted: ForwardHook,
    /// Tests only: runs after a client command's route returns.
    #[cfg(test)]
    pub(crate) after_relay: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: a one-shot pause in `route` after its opening is taken
    /// and before the connect.
    #[cfg(test)]
    pub(crate) before_open: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: runs handing a queued command over, or clearing it.
    #[cfg(test)]
    pub(crate) on_pass_on: PassOnHook,
    /// Tests only: a one-shot pause run after the read loop ends and
    /// before the write shutdown, with its own mutex released: the forced
    /// overflow races hold it to order connection teardown against the
    /// bound report. No other race hook is required: queue fullness, EOF
    /// and joins need none.
    #[cfg(test)]
    pub(crate) before_disconnect_cleanup: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: a one-shot pause in admission after the increment and
    /// before the flag load.
    #[cfg(test)]
    pub(crate) between_admit_steps: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: fired when a sessions-stop zero-wait first finds a
    /// nonzero bind count.
    #[cfg(test)]
    pub(crate) on_inflight_wait: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: fired on the serving thread when the sessions-stop
    /// fan-out wait first parks with a session unconfirmed.
    #[cfg(test)]
    pub(crate) on_fanout_wait: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: fired on the serving thread after the sessions-stop
    /// scan lists `run/`, before any connect: a test starts or exits a
    /// session in the window between enumeration and connect.
    #[cfg(test)]
    pub(crate) on_scan_listed: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: fired when the Signal arm's flush wait first finds
    /// `sessions_answering` above 0.
    #[cfg(test)]
    pub(crate) on_abort_flush: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: fired on the serving thread after it sees the abort
    /// and before it writes the answer.
    #[cfg(test)]
    pub(crate) before_abort_answer: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Tests only: names `hub-ws-*` threads whose spawns fail, so the
    /// port's spawn-failure paths run without an outside seam.
    #[cfg(test)]
    pub(crate) fail_spawn: Mutex<Option<&'static str>>,
    /// The longest client line the read loop takes, in bytes
    /// before its line feed: tests lower it so the cap is reachable
    /// without a 64 MiB line, production takes `MAX_LINE`. One field
    /// and one function, so the mutation check pins the production
    /// value through the test seam instead of leaving a fallback
    /// no test compiles.
    pub(crate) line_cap: AtomicUsize,
}

/// The open connections and the idle timer. `zero_since` is `Some` exactly
/// while `open` is empty: the instant the count last reached 0.
struct Conns {
    /// Every connection the idle wait counts, and its shutdown handle.
    open: Vec<(u64, Closer)>,
    zero_since: Option<Instant>,
}

/// How one counted connection shuts down: a local socket, or a
/// websocket's shared sink.
pub(crate) enum Closer {
    Local(UnixStream),
    Web(Arc<crate::port::Web>),
}

/// What one pass of [`Hub::idle_wait`] found.
pub(crate) enum Idle {
    /// A signal arrived, or a local `stop`: its number, SIGTERM for a
    /// stop, which ends the hub as a SIGTERM does.
    Signal(i32),
    /// No client for the whole `idle_exit`: the exit is claimed.
    Expired,
    /// Woken by a clock move or a change: check again.
    Woken,
}

impl Hub {
    /// Opens the hub in `home`: the diagnostic log, pruned, before the
    /// caller writes `hub_started`. The open count starts at 0, so the idle
    /// timer starts now.
    pub(crate) fn new(
        home: &Path,
        fiber_version: &str,
        starter: Arc<dyn Starter>,
        clock: Arc<dyn Clock>,
        diag: log::diag::Diag,
    ) -> Self {
        let tick = Arc::new(Tick::default());
        let wake = Arc::clone(&tick);
        let wake: Arc<dyn Wake> = wake;
        clock.subscribe(Arc::downgrade(&wake));
        let zero_since = Some(clock.now());
        let feed = Arc::new(Feed::new(home, Arc::clone(&clock)));
        Self {
            home: home.to_path_buf(),
            fiber_version: fiber_version.to_owned(),
            starter,
            clock,
            diag,
            feed,
            pairing: crate::pairing::Pairing::new(),
            machine: None,
            next_client: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            sessions_stopping: AtomicBool::new(false),
            inflight_binds: AtomicUsize::new(0),
            sessions_answering: AtomicUsize::new(0),
            conns: Mutex::new(Conns {
                open: Vec::new(),
                zero_since,
            }),
            tick,
            wake,
            resume_gate: Mutex::new(()),
            environments: crate::Environments::ready(crate::Environment {
                vars: Vec::new(),
                source: VariablesSource::Inherited,
            }),
            rejoins: crate::rejoin::Connections::default(),
            starting: Mutex::new(HashMap::new()),
            #[cfg(test)]
            before_hello: Mutex::new(None),
            #[cfg(test)]
            before_forward: Mutex::new(None),
            #[cfg(test)]
            after_replay_filter: Mutex::new(None),
            #[cfg(test)]
            before_accepted: Mutex::new(None),
            #[cfg(test)]
            after_relay: Mutex::new(None),
            #[cfg(test)]
            before_open: Mutex::new(None),
            #[cfg(test)]
            on_pass_on: Mutex::new(None),
            #[cfg(test)]
            before_disconnect_cleanup: Mutex::new(None),
            #[cfg(test)]
            between_admit_steps: Mutex::new(None),
            #[cfg(test)]
            on_inflight_wait: Mutex::new(None),
            #[cfg(test)]
            on_fanout_wait: Mutex::new(None),
            #[cfg(test)]
            on_scan_listed: Mutex::new(None),
            #[cfg(test)]
            on_abort_flush: Mutex::new(None),
            #[cfg(test)]
            before_abort_answer: Mutex::new(None),
            #[cfg(test)]
            fail_spawn: Mutex::new(None),
            line_cap: AtomicUsize::new(support::framing::MAX_LINE),
        }
    }

    /// The environment the hub's sessions start with: `serve` sets the
    /// shared capture, and every other caller keeps the published default.
    pub(crate) fn with_environments(mut self, environments: Arc<crate::Environments>) -> Self {
        self.environments = environments;
        self
    }

    /// Supplies configuration on this hub's machine.
    pub(crate) fn with_machine(mut self, machine: Arc<dyn crate::Machine>) -> Self {
        self.machine = Some(machine);
        self
    }

    /// The open client connections, this connection's asker included.
    pub(crate) fn clients(&self) -> usize {
        lock(&self.conns).open.len()
    }

    /// When the open count last reached 0; `None` while a client is open.
    #[cfg(test)]
    pub(crate) fn zero_since(&self) -> Option<Instant> {
        lock(&self.conns).zero_since
    }

    /// Wakes the idle wait: the signal arm calls it after recording a
    /// signal.
    pub(crate) fn waker(&self) -> Arc<dyn Wake> {
        Arc::clone(&self.wake)
    }

    /// Counts a connection the accept loop holds: under the connection
    /// lock, so the idle exit's claim sees it. Exiting once the hub is
    /// exiting: the caller drops the stream unanswered, EOF with no
    /// `hub_hello`, and the client retries. Dropped when the shutdown clone
    /// fails: count nothing, serve nothing, and continue accepting.
    pub(crate) fn poll_accept(&self, stream: &UnixStream, stop: &AtomicBool) -> Accept {
        let mut conns = lock(&self.conns);
        // Either flag ends the hub: the run loop sets its own once it
        // observes the hub flag, and an accept racing the exit is dropped
        // unanswered under this lock, EOF with no `hub_hello`.
        if stop.load(Ordering::SeqCst) || self.stop.load(Ordering::SeqCst) {
            return Accept::Exiting;
        }
        let counted = self.register_with(|| stream.try_clone().map(Closer::Local), &mut conns);
        drop(conns);
        // Woken after the connection lock is released: the idle wait takes
        // the tick lock before the connection lock.
        self.tick.wake();
        match counted {
            Some(n) => Accept::Counted(n),
            None => Accept::Dropped,
        }
    }

    /// Counts one connection, for a caller with no exit to check.
    /// `None` when the shutdown clone fails: count nothing, serve nothing.
    #[cfg(test)]
    pub(crate) fn register(&self, stream: &UnixStream) -> Option<u64> {
        let mut conns = lock(&self.conns);
        let counted = self.register_with(|| stream.try_clone().map(Closer::Local), &mut conns);
        drop(conns);
        self.tick.wake();
        counted
    }

    /// Counts a websocket the port acceptor holds: under the connection
    /// lock, so the idle exit's claim sees it. Exiting once the hub is
    /// exiting: the caller shuts the stream down unanswered, EOF with no
    /// `hub_hello`, and the client retries.
    pub(crate) fn poll_accept_web(&self, web: &Arc<crate::port::Web>, stop: &AtomicBool) -> Accept {
        let mut conns = lock(&self.conns);
        if stop.load(Ordering::SeqCst) || self.stop.load(Ordering::SeqCst) {
            return Accept::Exiting;
        }
        let counted = self.register_with(|| Ok(Closer::Web(Arc::clone(web))), &mut conns);
        drop(conns);
        // Woken after the connection lock is released: the idle wait takes
        // the tick lock before the connection lock.
        self.tick.wake();
        match counted {
            Some(n) => Accept::Counted(n),
            None => Accept::Dropped,
        }
    }

    /// Tests only: every websocket's shared sink, oldest first.
    #[cfg(test)]
    pub(crate) fn webs(&self) -> Vec<Arc<crate::port::Web>> {
        lock(&self.conns)
            .open
            .iter()
            .filter_map(|(_, closer)| match closer {
                Closer::Local(_) => None,
                Closer::Web(web) => Some(Arc::clone(web)),
            })
            .collect()
    }

    fn register_with(
        &self,
        clone: impl FnOnce() -> std::io::Result<Closer>,
        conns: &mut Conns,
    ) -> Option<u64> {
        // Clone first: a failure counts nothing, serves nothing, and
        // consumes no client number.
        let Ok(shutdown) = clone() else {
            return None;
        };
        let n = self.next_client.fetch_add(1, Ordering::SeqCst) + 1;
        conns.open.push((n, shutdown));
        conns.zero_since = None;
        Some(n)
    }

    /// Rolls back a counted connection whose serving thread never started.
    /// Never connected, so no `client_disconnected` line.
    pub(crate) fn rollback(&self, n: u64) {
        self.release(n);
    }

    /// Removes connection `n`; when the count reaches 0 the idle timer
    /// starts now. Wakes the idle wait.
    fn release(&self, n: u64) {
        let mut conns = lock(&self.conns);
        if let Some(at) = conns.open.iter().position(|(id, _)| *id == n) {
            conns.open.remove(at);
            if conns.open.is_empty() {
                conns.zero_since = Some(self.clock.now());
            }
        }
        drop(conns);
        self.tick.wake();
    }

    /// One pass of the idle wait. A pending signal returns it. With no
    /// client open since `zero_since` for the whole `idle_exit`, claims the
    /// exit under the connection lock: sets `stop`, so an accept racing the
    /// claim sees it and drops its stream unanswered. Otherwise parks on the
    /// clock until `zero_since + idle_exit`, or with no deadline while a
    /// client is open, woken by clock moves, changes and signals.
    pub(crate) fn idle_wait(
        &self,
        idle_exit: Duration,
        stop: &AtomicBool,
        got: &AtomicI32,
    ) -> Idle {
        // Taken before the checks and held into the wait: a change or a
        // signal that lands in between blocks in `Tick::wake` until this
        // thread waits, instead of waking nobody.
        let guard = self.tick.hold();
        // A local `stop` ends the hub as a SIGTERM does, checked before
        // the signal so the order of arrival decides nothing.
        if self.stop.load(Ordering::SeqCst) {
            return Idle::Signal(signal_hook::consts::SIGTERM);
        }
        let signal = got.swap(0, Ordering::SeqCst);
        if signal != 0 {
            return Idle::Signal(signal);
        }
        let until = {
            let conns = lock(&self.conns);
            // An `idle_exit` past the end of time never expires.
            match conns
                .zero_since
                .and_then(|since| since.checked_add(idle_exit))
            {
                Some(until) if self.clock.now() >= until => {
                    stop.store(true, Ordering::SeqCst);
                    return Idle::Expired;
                }
                until => until,
            }
        };
        // Held into the park: a change or a signal that lands after the
        // checks wakes this thread instead of nobody.
        self.tick.park(self.clock.as_ref(), guard, until);
        Idle::Woken
    }

    /// Whether a signal or a bare `stop` asked the hub to exit: every
    /// wait the sessions-stop loop makes ends on it, so a stop with
    /// `sessions` answers inside the signal bound instead of waiting the
    /// loop out.
    pub(crate) fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Admits one bind: the increment always precedes the
    /// `sessions_stopping` load, so a sessions stop stored before this
    /// call refuses it, and one stored after waits for it in its
    /// zero-wait. The guard drops only after the bind lands or the
    /// starter is killed.
    pub(crate) fn admit(&self) -> Result<Admitted<'_>, Refused> {
        self.inflight_binds.fetch_add(1, Ordering::SeqCst);
        #[cfg(test)]
        if let Some(step) = lock(&self.between_admit_steps).take() {
            step();
        }
        if self.sessions_stopping.load(Ordering::SeqCst) {
            self.inflight_binds.fetch_sub(1, Ordering::SeqCst);
            self.tick.wake();
            return Err(stopping());
        }
        Ok(Admitted { hub: self })
    }

    /// Records a local `stop`: under the tick hold, so a waiter that has
    /// checked and not yet parked blocks until the store lands instead of
    /// missing it. Returns unwoken: the `stop` arm wakes after writing the
    /// acknowledgement, so the client always reads the acceptance before
    /// the exit closes its socket. Joins as a waker, never as a waiter.
    /// In tests, tells the armed `attempt` sender once this call reaches
    /// the hold, so a test proves its stop thread arrived before asserting
    /// the store is blocked.
    pub(crate) fn request_stop(&self) {
        #[cfg(test)]
        let attempt = self.tick.tell_if_contended();
        let _held = self.tick.hold();
        self.stop.store(true, Ordering::SeqCst);
        #[cfg(test)]
        if let Some(attempt) = attempt {
            attempt.send(()).unwrap_or(());
        }
    }

    /// Shuts down every client connection: each session sees its clients
    /// leave. Local sockets shut down under the connection lock; each
    /// websocket gets its stopping close once its sink is free, then every
    /// stream is shut down, inside the 5 s bound. Sessions are untouched.
    pub(crate) fn shutdown_clients(&self) {
        let webs = {
            let conns = lock(&self.conns);
            let mut webs = Vec::new();
            for (_, closer) in &conns.open {
                match closer {
                    Closer::Local(stream) => {
                        stream.shutdown(Shutdown::Both).unwrap_or(());
                    }
                    Closer::Web(web) => webs.push(Arc::clone(web)),
                }
            }
            webs
        };
        crate::port::stop_all(self, webs);
    }
}

/// The longest client line the read loop takes: the test seam lowers it
/// so the cap is reachable without a 64 MiB line.
fn line_cap(hub: &Hub) -> usize {
    hub.line_cap.load(Ordering::SeqCst)
}

/// Tests only: a one-shot pause before the next forwarded session line:
/// the line about to be written, and the relays, with no lock held.
#[cfg(test)]
type ForwardHook = Mutex<Option<Box<dyn FnOnce(&[u8], &Arc<Mutex<Relays>>) + Send>>>;

/// Tests only: a one-shot pause after a session line's replay-filter decision.
#[cfg(test)]
type ReplayFilterHook = Mutex<Option<Box<dyn FnOnce(&[u8], bool) + Send>>>;

/// Tests only: a relay thread's handover hook.
#[cfg(test)]
type PassOnHook = Mutex<Option<Box<dyn FnOnce(crate::retire::PassOn) + Send>>>;
/// What `poll_accept` decided for one accepted stream.
pub(crate) enum Accept {
    /// Counted under the connection lock: serve it as `n`.
    Counted(u64),
    /// The shutdown clone failed: count nothing, serve nothing.
    Dropped,
    /// The hub is exiting: drop unanswered.
    Exiting,
}

/// Serves one client connection: `hub_hello`, then one acknowledgement per
/// hub command, and a relay thread per session commanded on it. Returning
/// shuts down every relay stream of the connection, so each session sees
/// this client leave. Tests only: production counts through
/// [`Hub::poll_accept`], then serves here.
#[cfg(test)]
pub(crate) fn serve_connection(stream: UnixStream, hub: Arc<Hub>) {
    let Some(n) = hub.register(&stream) else {
        return;
    };
    serve_counted(stream, hub, n);
}

/// Serves an accepted connection already counted by [`Hub::poll_accept`].
pub(crate) fn serve_counted(stream: UnixStream, hub: Arc<Hub>, n: u64) {
    serve_from(stream, hub, n, true, crate::pairing::Who::Local, None);
}

/// Serves an authenticated websocket's pair end, already counted by
/// [`Hub::poll_accept_web`]: no second `hub_hello`, since the port sent
/// one before authentication.
pub(crate) fn serve_authenticated(
    stream: UnixStream,
    hub: Arc<Hub>,
    n: u64,
    who: crate::pairing::Who,
    web: Option<Arc<crate::port::Web>>,
) {
    serve_from(stream, hub, n, false, who, web);
}

/// Serves a counted connection: with `hello`, `hub_hello` first, then one
/// acknowledgement per hub command and a relay thread per session
/// commanded on it.
fn serve_from(
    stream: UnixStream,
    hub: Arc<Hub>,
    n: u64,
    hello: bool,
    who: crate::pairing::Who,
    web: Option<Arc<crate::port::Web>>,
) {
    hub.diag.line(
        Severity::Info,
        None,
        "client_connected",
        &format!("Client {n} connected."),
    );
    let writer = match stream.try_clone() {
        Ok(writer) => Arc::new(Mutex::new(writer)),
        Err(_) => {
            hub.pairing.forget(&hub, &who);
            disconnect(&hub, n);
            return;
        }
    };
    // Each registration's own shutdown, cloned from the connection
    // stream before it is read from, without the shared writer mutex:
    // cleanup never clones at the moment it runs. A bridged connection
    // also carries its websocket's TCP shutdown.
    let shutdown_of = |stream: &UnixStream| {
        stream.try_clone().ok().map(|stream| crate::feed::Closer {
            stream,
            web: web.clone(),
        })
    };
    let (Some(feed_closer), Some(heard_closer), Some(heard_retired)) = (
        shutdown_of(&stream),
        shutdown_of(&stream),
        shutdown_of(&stream),
    ) else {
        hub.pairing.forget(&hub, &who);
        disconnect(&hub, n);
        return;
    };
    hub.pairing.attach(&who, &writer);
    if hello {
        #[cfg(test)]
        if let Some(pause) = lock(&hub.before_hello).take() {
            pause();
        }
        send(
            &writer,
            &hub,
            "hub_hello",
            Map::from_iter([(
                "fiber_version".to_owned(),
                Value::String(hub.fiber_version.clone()),
            )]),
        );
    }
    let mut fresh = Relays::default();
    // Replay timing rides this connection's hub: at debug the transfer
    // path stamps failures on its writer, at info nothing is kept.
    if hub.diag.debugging() {
        fresh.timing_hub = Some(Arc::downgrade(&hub));
    }
    let relays: Arc<Mutex<Relays>> = Arc::new(Mutex::new(fresh));
    hub.rejoins.register(n, &writer, &relays);
    let Some(heard) = hub
        .feed
        .attention
        .listen(Arc::clone(&writer), heard_closer, heard_retired)
    else {
        stream.shutdown(Shutdown::Both).unwrap_or(());
        hub.rejoins.unregister(n);
        hub.pairing.forget(&hub, &who);
        disconnect(&hub, n);
        return;
    };
    hub.pairing.announce_owed(&hub, &who);
    let mut read = BufReader::new(stream);
    let mut remote = Remote::default();
    loop {
        match support::framing::read_line(&mut read, line_cap(&hub)) {
            Ok(None) | Err(_) => break,
            Ok(Some(buf)) => {
                on_command(
                    &buf,
                    &hub,
                    &writer,
                    &feed_closer,
                    &relays,
                    &who,
                    &mut remote,
                    web.as_ref(),
                );
            }
        }
    }
    // The test pause runs after the read loop and before the write
    // shutdown, with its mutex released: taking it out drops the guard
    // before the callback runs.
    #[cfg(test)]
    let before_disconnect_cleanup = lock(&hub.before_disconnect_cleanup).take();
    #[cfg(test)]
    if let Some(cleanup) = before_disconnect_cleanup {
        cleanup();
    }
    // Fails a blocked writer: the write half only, since a client that
    // half-closed already disconnected the socket, and shutting down a
    // disconnected read half fails without unblocking anything.
    read.get_ref().shutdown(Shutdown::Write).unwrap_or(());
    let fed = remote.fed.take();
    drop(remote);
    if let Some(fed) = fed {
        hub.feed.unsubscribe(fed);
    }
    hub.feed.attention.unlisten(heard);
    hub.pairing.forget(&hub, &who);
    // A first prompt still waiting for this connection's subscription goes
    // out now: the connection will never subscribe. Released once the
    // relays lock is dropped.
    hub.rejoins.unregister(n);
    let waiting = {
        let mut held = lock(&relays);
        held.close_all();
        std::mem::take(&mut held.awaiting)
    };
    for (_, first) in waiting {
        first.release();
    }
    disconnect(&hub, n);
}

/// Logs the departure, then releases the count: once the count shows it
/// gone, the line is written, and the idle exit's `hub_stopped` follows it.
fn disconnect(hub: &Hub, n: u64) {
    hub.diag.line(
        Severity::Info,
        None,
        "client_disconnected",
        &format!("Client {n} disconnected."),
    );
    hub.release(n);
}

#[derive(Default)]
struct Remote {
    search: crate::file_search::Search,
    login: crate::device_login::Login,
    fed: Option<u64>,
}

fn on_command(
    bytes: &[u8],
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    closer: &crate::feed::Closer,
    relays: &Arc<Mutex<Relays>>,
    who: &crate::pairing::Who,
    remote: &mut Remote,
    web: Option<&Arc<crate::port::Web>>,
) {
    if who.revoked() {
        return;
    }
    let line = match classify(bytes) {
        Ok(line) => line,
        Err(id) => {
            reject(
                writer,
                hub,
                id.as_ref(),
                &ErrorCode::Malformed,
                "A command is one JSON object per line, with a string `id` and `command`.",
            );
            return;
        }
    };
    if let Some(session) = line.session_id {
        crate::relay::relay_command(&line.id, &session, &line.value, hub, writer, relays);
        return;
    }
    match line.command.as_str() {
        "device_login" if hub.machine.is_some() => {
            remote.login.start(&line.id, &line.args, hub, writer)
        }
        "device_login_result" if hub.machine.is_some() => {
            remote.login.result(&line.id, &line.args, hub, writer)
        }
        "device_login_cancel" if hub.machine.is_some() => {
            remote.login.cancel(&line.id, &line.args, hub, writer)
        }
        "search_files" if hub.machine.is_some() => {
            remote.search.submit(&line.id, &line.args, hub, writer)
        }
        "configure" if hub.machine.is_some() => answer(
            writer,
            hub,
            &line.id,
            crate::machine::on_configure(hub, &line.args),
        ),
        "read_config" if hub.machine.is_some() => answer(
            writer,
            hub,
            &line.id,
            crate::machine::on_read_config(hub, &line.args),
        ),
        "write_config" if hub.machine.is_some() => answer(
            writer,
            hub,
            &line.id,
            crate::machine::on_write_config(hub, &line.args),
        ),
        "pairing_code" => match hub.pairing.mint(hub, who, &line.args) {
            Ok(crate::pairing::Outcome::Done(result)) => {
                accept_result(writer, hub, &line.id, result)
            }
            Ok(crate::pairing::Outcome::Dropped) => {}
            Err(refused) => reject(writer, hub, Some(&line.id), &refused.code, &refused.message),
        },
        "devices" => answer(writer, hub, &line.id, hub.pairing.devices(hub, &line.args)),
        "revoke" => match hub.pairing.revoke(hub, who, &line.args) {
            Ok(crate::pairing::Outcome::Done(revoked)) => {
                answer(writer, hub, &line.id, Ok(None));
                hub.pairing.finish(hub, revoked);
            }
            Ok(crate::pairing::Outcome::Dropped) => {}
            Err(refused) => reject(writer, hub, Some(&line.id), &refused.code, &refused.message),
        },
        "start" => on_start(&line.id, &line.args, hub, writer, relays),
        "status" => on_status(&line.id, &line.args, hub, writer),
        "placeholder_env" => on_placeholder_env(&line.id, &line.args, hub, writer),
        "refresh" => answer(
            writer,
            hub,
            &line.id,
            crate::environment::refresh(hub, &line.args),
        ),
        "stop" => on_stop(&line.id, &line.args, hub, writer, web),
        "prompt_history" => answer(
            writer,
            hub,
            &line.id,
            crate::prompt_history::answer(&hub.home, &line.args).map(Some),
        ),
        "feed" => on_feed(&line.id, &line.args, hub, writer, closer, &mut remote.fed),
        "dismiss" => answer(writer, hub, &line.id, hub.feed.dismiss(&line.args)),
        "recent" => answer(writer, hub, &line.id, hub.feed.recent(&line.args)),
        "sessions" => {
            // The hub's own writer and clock travel as one pair, and only
            // at debug: at info no callable is built and no stamp is taken.
            let timing = hub.diag.debugging().then(|| hub.diag.timings());
            answer(
                writer,
                hub,
                &line.id,
                crate::sessions::answer(
                    &hub.feed,
                    &hub.home,
                    &line.args,
                    hub.clock.as_ref(),
                    timing.as_ref(),
                ),
            )
        }
        "delete" => answer(
            writer,
            hub,
            &line.id,
            crate::delete::delete(hub, &line.args),
        ),
        "import" if hub.machine.is_some() => answer(
            writer,
            hub,
            &line.id,
            crate::machine::on_import(hub, &line.args),
        ),
        "directories" => answer(
            writer,
            hub,
            &line.id,
            crate::directories::directories(hub.environments.user_home().as_deref(), &line.args),
        ),
        "read_file" => answer(
            writer,
            hub,
            &line.id,
            crate::read_file::read_file(&hub.home, &line.args),
        ),
        command => {
            let message = format!("`{command}` is not a hub command.");
            reject(
                writer,
                hub,
                Some(&line.id),
                &ErrorCode::UnknownCommand,
                &message,
            );
        }
    }
}

pub(crate) struct Classified {
    pub(crate) id: CommandId,
    pub(crate) command: String,
    pub(crate) session_id: Option<String>,
    pub(crate) value: Value,
    pub(crate) args: Map<String, Value>,
}

/// `Err` carries `command_id` when the line had a string `id`.
pub(crate) fn classify(bytes: &[u8]) -> Result<Classified, Option<CommandId>> {
    let text = std::str::from_utf8(bytes).map_err(|_| None)?;
    let value: Value = serde_json::from_str(text).map_err(|_| None)?;
    let Some(map) = value.as_object() else {
        return Err(None);
    };
    let id = match map.get("id") {
        Some(Value::String(id)) => Some(CommandId(id.clone())),
        _ => None,
    };
    if map
        .keys()
        .any(|key| key != "id" && key != "command" && key != "session_id" && key != "args")
    {
        return Err(id);
    }
    let Some(id) = id else {
        return Err(None);
    };
    let Some(Value::String(command)) = map.get("command") else {
        return Err(Some(id));
    };
    let args = match map.get("args") {
        None => Map::new(),
        Some(Value::Object(args)) => args.clone(),
        Some(_) => return Err(Some(id)),
    };
    let session_id = match map.get("session_id") {
        None => None,
        Some(Value::String(session)) => Some(session.clone()),
        Some(_) => return Err(Some(id)),
    };
    Ok(Classified {
        id,
        command: command.clone(),
        session_id,
        value,
        args,
    })
}

/// Answers `start` (`docs/invocation.md`, "What the hub speaks"). With
/// `content`, the answer comes first: the release entry is registered and
/// the `hub-first-prompt` thread started, unarmed, before the answer is
/// written, and its bound is armed only once the write returns, whether it
/// succeeded or not. So the prompt never precedes the answer, and a
/// release can only follow it.
fn on_start(
    id: &CommandId,
    args: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    relays: &Arc<Mutex<Relays>>,
) {
    let Some(args) = start_args(args) else {
        reject(
            writer,
            hub,
            Some(id),
            &ErrorCode::InvalidArguments,
            "The arguments do not fit this command.",
        );
        return;
    };
    let continues = args.continues.map(|id| SessionId(id.to_owned()));
    let (session_id, held) = match start::run(
        hub,
        id,
        args.workspace,
        args.model,
        &args.overrides,
        args.worktree,
        args.content,
        continues.as_ref(),
    ) {
        Outcome::Accepted { session_id, first } => (session_id, first),
        Outcome::Rejected(refused) => {
            reject(writer, hub, Some(id), &refused.code, &refused.message);
            return;
        }
    };
    let first = match held {
        Some(held) => {
            let first = First::new(Arc::clone(&hub.tick));
            lock(relays)
                .awaiting
                .push((session_id.0.clone(), Arc::clone(&first)));
            if crate::first::later(hub, relays, *held, Arc::clone(&first)).is_err() {
                crate::first::forget(relays, &first);
                let failed =
                    start::io_failed(hub, &session_id, "its first prompt could not be sent.");
                if let Outcome::Rejected(refused) = failed {
                    reject(writer, hub, Some(id), &refused.code, &refused.message);
                }
                return;
            }
            Some(first)
        }
        None => None,
    };
    let result = CommandResult::Start { session_id };
    accept_result(
        writer,
        hub,
        id,
        serde_json::to_value(&result).unwrap_or(Value::Null),
    );
    if let Some(first) = first {
        first.arm(hub.clock.now() + FIRST_PROMPT_WAIT);
    }
}

fn on_status(
    id: &CommandId,
    args: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
) {
    if !args.is_empty() {
        reject(
            writer,
            hub,
            Some(id),
            &ErrorCode::InvalidArguments,
            "The arguments do not fit this command.",
        );
        return;
    }
    let result = CommandResult::Status {
        running: true,
        fiber_version: hub.fiber_version.clone(),
        clients: hub.clients(),
        variables: hub.environments.get().variables(),
    };
    accept_result(
        writer,
        hub,
        id,
        serde_json::to_value(&result).unwrap_or(Value::Null),
    );
}

/// Ends the hub as SIGTERM does; with `sessions` true, first shuts down
/// every running session the hub knows of, each through its own
/// shutdown, then exits (`docs/invocation.md`, "What the hub speaks").
/// Any authenticated connection may send it. Without `sessions` the
/// answer carries the `command_id` only, on local and websocket paths,
/// and the hub ends at once. With `sessions` the answer carries
/// `{"sessions_stopped": N}`, or `command_rejected` `io_failed` naming
/// each session unconfirmed at the bound or the abort, and the hub exits
/// after answering. On a websocket the answer goes out behind the drain
/// barrier: a `hub-stop-out` thread writes under the connection writer
/// lock and shuts the bridge write side; the serving thread parks on the
/// tick until written and `web.drained()`, or `STOP_CLOSE_WAIT`. A
/// `sessions` that is not a boolean, an explicit null, or any unknown
/// key is `invalid_arguments` and the hub keeps running. The store runs
/// under the tick hold before the acknowledgement is written, and the
/// wake after it, so the client always reads the acceptance before the
/// exit closes its socket, and a waiter that has checked and not yet
/// parked cannot miss the store.
fn on_stop(
    id: &CommandId,
    args: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
    web: Option<&Arc<crate::port::Web>>,
) {
    let sessions = match stop_sessions(args) {
        Ok(sessions) => sessions,
        Err(()) => {
            reject(
                writer,
                hub,
                Some(id),
                &ErrorCode::InvalidArguments,
                "The arguments do not fit this command.",
            );
            return;
        }
    };
    if !sessions.unwrap_or(false) {
        let payload = Map::from_iter([("command_id".to_owned(), Value::String(id.0.clone()))]);
        match web {
            Some(web) => drain_web(hub, writer, web, "command_accepted", payload),
            None => send(writer, hub, "command_accepted", payload),
        }
        hub.request_stop();
        hub.tick.wake();
        return;
    }
    let _answering = Answering::take(hub);
    hub.sessions_stopping.store(true, Ordering::SeqCst);
    hub.tick.wake();
    let outcome = crate::sessions_stop::run(hub);
    if hub.stop_requested() {
        #[cfg(test)]
        if let Some(hook) = lock(&hub.before_abort_answer).take() {
            hook();
        }
    }
    if outcome.unconfirmed.is_empty() {
        let result = CommandResult::Stop {
            sessions_stopped: outcome.confirmed.len(),
        };
        let result = serde_json::to_value(&result).unwrap_or(Value::Null);
        match web {
            Some(web) => {
                let mut payload =
                    Map::from_iter([("command_id".to_owned(), Value::String(id.0.clone()))]);
                payload.insert("result".to_owned(), result);
                drain_web(hub, writer, web, "command_accepted", payload);
            }
            None => accept_result(writer, hub, id, result),
        }
    } else {
        let message = unconfirmed_message(&outcome.unconfirmed);
        match web {
            Some(web) => drain_web(
                hub,
                writer,
                web,
                "command_rejected",
                rejection(Some(id), &ErrorCode::IoFailed, &message),
            ),
            None => reject(writer, hub, Some(id), &ErrorCode::IoFailed, &message),
        }
    }
    hub.request_stop();
    drop(_answering);
    hub.tick.wake();
}

/// The `sessions` request: `None` when absent (a bare stop), `Some`
/// when exactly one `sessions` boolean key. A `sessions` that is not a
/// boolean, an explicit null, or any unknown key is `Err`: the caller
/// refuses it `invalid_arguments` and the hub keeps running.
fn stop_sessions(args: &Map<String, Value>) -> Result<Option<bool>, ()> {
    if args.is_empty() {
        return Ok(None);
    }
    if contains_null(&Value::Object(args.clone())) {
        return Err(());
    }
    if args.len() != 1 {
        return Err(());
    }
    match args.get("sessions") {
        Some(Value::Bool(sessions)) => Ok(Some(*sessions)),
        _ => Err(()),
    }
}

/// What a partial sessions stop tells the asker: `io_failed` naming each
/// session unconfirmed at the bound or the abort, in id order.
fn unconfirmed_message(unconfirmed: &[SessionId]) -> String {
    let ids: Vec<&str> = unconfirmed.iter().map(|id| id.0.as_str()).collect();
    if ids.len() == 1 {
        format!("Session {} is still running.", ids[0])
    } else {
        format!("Sessions {} are still running.", ids.join(", "))
    }
}

/// A `stop` with `sessions` now answering: the Signal arm aborts the
/// loop only while one is held, and waits only for its flush. Held from
/// before the `sessions_stopping` store until after the answer and, on
/// a websocket, its drain barrier: the drop's wake ends the arm's flush
/// wait.
struct Answering<'a> {
    hub: &'a Hub,
}

impl<'a> Answering<'a> {
    fn take(hub: &'a Hub) -> Self {
        hub.sessions_answering.fetch_add(1, Ordering::SeqCst);
        Self { hub }
    }
}

impl Drop for Answering<'_> {
    fn drop(&mut self) {
        self.hub.sessions_answering.fetch_sub(1, Ordering::SeqCst);
        self.hub.tick.wake();
    }
}

/// Writes one stop answer on a websocket behind the drain barrier: a
/// `hub-stop-out` thread writes under the connection writer lock and
/// shuts the bridge write side, so the pump forwards the answer, then
/// its close, then drains; the serving thread parks on the tick until
/// written and `web.drained()`, or `STOP_CLOSE_WAIT`.
fn drain_web(
    hub: &Hub,
    writer: &Arc<Mutex<UnixStream>>,
    web: &Arc<crate::port::Web>,
    kind: &str,
    payload: Map<String, Value>,
) {
    let written = Arc::new(AtomicBool::new(false));
    let line = Arc::new(line(hub, kind, payload));
    let wrote = spawn(hub, "hub-stop-out", {
        let writer = Arc::clone(writer);
        let written = Arc::clone(&written);
        let tick = Arc::clone(&hub.tick);
        let line = Arc::clone(&line);
        move || {
            let mut writer = lock(&writer);
            support::framing::write_line(&mut *writer, &line).unwrap_or(());
            writer.shutdown(Shutdown::Write).unwrap_or(());
            written.store(true, Ordering::SeqCst);
            tick.wake();
        }
    });
    if wrote.is_err() {
        // A thread that cannot be spawned writes on the serving thread:
        // the answer still goes out before the exit.
        let mut writer = lock(writer);
        support::framing::write_line(&mut *writer, &line).unwrap_or(());
        writer.shutdown(Shutdown::Write).unwrap_or(());
        written.store(true, Ordering::SeqCst);
    }
    let end = hub.clock.now() + crate::port::STOP_CLOSE_WAIT;
    hub.tick.wait_for(&*hub.clock, &mut |now| {
        if written.load(Ordering::SeqCst) && web.drained() {
            return crate::tick::Wait::Done;
        }
        if now >= end {
            return crate::tick::Wait::Done;
        }
        crate::tick::Wait::Until(Some(end))
    });
}

/// Answers `placeholder_env` with the hub's captured values for the
/// requested placeholder variable names, absent names omitted: the names
/// cross, never the whole environment, and the values are never logged.
fn on_placeholder_env(
    id: &CommandId,
    args: &Map<String, Value>,
    hub: &Arc<Hub>,
    writer: &Arc<Mutex<UnixStream>>,
) {
    let Some(names) = placeholder_names(args) else {
        reject(
            writer,
            hub,
            Some(id),
            &ErrorCode::InvalidArguments,
            "The arguments do not fit this command.",
        );
        return;
    };
    let environment = hub.environments.get();
    let mut values = std::collections::BTreeMap::new();
    for name in &names {
        if let Some(value) = environment.value(name) {
            values.insert(name.clone(), value);
        }
    }
    let result = CommandResult::PlaceholderEnv { values };
    accept_result(
        writer,
        hub,
        id,
        serde_json::to_value(&result).unwrap_or(Value::Null),
    );
}

/// The requested placeholder variable names: `names`, an array of
/// strings, and no other key. A missing or misshaped `names`, an extra
/// key or an explicit `null` is `None`.
fn placeholder_names(args: &Map<String, Value>) -> Option<Vec<String>> {
    if contains_null(&Value::Object(args.clone())) {
        return None;
    }
    if args.len() != 1 {
        return None;
    }
    let names = args.get("names")?;
    let names = names.as_array()?;
    names
        .iter()
        .map(|name| name.as_str().map(str::to_owned))
        .collect()
}

/// `start`'s `args`: `workspace` (required string), `model` (optional
/// string), `overrides` (optional array of strings, each one a
/// `key=value` the session takes as `-c`), `worktree` (optional boolean,
/// absent is false), `continues` (optional string, the id of the session
/// whose worktree the new session runs in), `content` (optional, passed
/// through as JSON once it fits `prompt`'s `content`). A wrong, missing or extra key, or an
/// explicit `null`, is `None`.
struct StartArgs<'a> {
    workspace: &'a str,
    model: Option<&'a str>,
    overrides: Vec<&'a str>,
    worktree: bool,
    continues: Option<&'a str>,
    content: Option<&'a Value>,
}

fn start_args(args: &Map<String, Value>) -> Option<StartArgs<'_>> {
    if contains_null(&Value::Object(args.clone())) {
        return None;
    }
    if args.keys().any(|key| {
        key != "workspace"
            && key != "model"
            && key != "overrides"
            && key != "worktree"
            && key != "continues"
            && key != "content"
    }) {
        return None;
    }
    let workspace = args.get("workspace")?.as_str()?;
    let model = match args.get("model") {
        None => None,
        Some(Value::String(model)) => Some(model.as_str()),
        Some(_) => return None,
    };
    let overrides = match args.get("overrides") {
        None => Vec::new(),
        Some(Value::Array(items)) => {
            let mut overrides = Vec::with_capacity(items.len());
            for item in items {
                overrides.push(item.as_str()?);
            }
            overrides
        }
        Some(_) => return None,
    };
    let worktree = match args.get("worktree") {
        None => false,
        Some(Value::Bool(worktree)) => *worktree,
        Some(_) => return None,
    };
    let continues = match args.get("continues") {
        None => None,
        Some(Value::String(continues)) => Some(continues.as_str()),
        Some(_) => return None,
    };
    let content = args.get("content");
    if let Some(content) = content
        && serde_json::from_value::<Vec<SentPart>>(content.clone()).is_err()
    {
        return None;
    }
    Some(StartArgs {
        workspace,
        model,
        overrides,
        worktree,
        continues,
        content,
    })
}

pub(crate) fn accept_result(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Hub,
    id: &CommandId,
    result: Value,
) {
    let mut payload = Map::new();
    payload.insert("command_id".to_owned(), Value::String(id.0.clone()));
    payload.insert("result".to_owned(), result);
    send(writer, hub, "command_accepted", payload);
}

/// Why a hub command is rejected: its code and Fiber's own sentence.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Refused {
    /// The rejection code.
    pub(crate) code: ErrorCode,
    /// What the client is told.
    pub(crate) message: String,
}

/// One admitted bind: decrements `inflight_binds` on drop, then wakes
/// the tick, so a sessions stop parked in its zero-wait sees the count
/// reach zero. Dropping follows the bind on every path: on any arm but
/// `Connected`, after `started.kill()`.
pub(crate) struct Admitted<'a> {
    hub: &'a Hub,
}

impl Drop for Admitted<'_> {
    fn drop(&mut self) {
        self.hub.inflight_binds.fetch_sub(1, Ordering::SeqCst);
        self.hub.tick.wake();
    }
}

/// Why a start, resume or rewind is refused while a sessions stop runs:
/// `io_failed`, with the sentence every stopping refusal carries.
pub(crate) fn stopping() -> Refused {
    Refused {
        code: ErrorCode::IoFailed,
        message: "the hub is stopping.".to_owned(),
    }
}

pub(crate) fn reject(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Hub,
    id: Option<&CommandId>,
    code: &ErrorCode,
    message: &str,
) {
    send(
        writer,
        hub,
        "command_rejected",
        rejection(id, code, message),
    );
}

/// A `command_rejected` payload for `code` and `message`, with `id` when
/// one was readable: the port answers refusals the same way.
pub(crate) fn rejection(
    id: Option<&CommandId>,
    code: &ErrorCode,
    message: &str,
) -> Map<String, Value> {
    let mut payload = Map::new();
    payload.insert(
        "code".to_owned(),
        serde_json::to_value(code).unwrap_or(Value::String("io_failed".to_owned())),
    );
    if let Some(id) = id {
        payload.insert("command_id".to_owned(), Value::String(id.0.clone()));
    }
    payload.insert("message".to_owned(), Value::String(message.to_owned()));
    payload
}

pub(crate) fn send(
    writer: &Arc<Mutex<UnixStream>>,
    hub: &Hub,
    kind: &str,
    payload: Map<String, Value>,
) {
    let bytes = line(hub, kind, payload);
    let mut writer = lock(writer);
    support::framing::write_line(&mut *writer, &bytes).unwrap_or(());
}

/// Serialises one hub line, without its trailing newline: the port sends
/// it as one websocket text message.
pub(crate) fn line(hub: &Hub, kind: &str, payload: Map<String, Value>) -> Vec<u8> {
    let line = HubLine {
        kind: kind.to_owned(),
        ts: wall_ms(hub.clock.wall()),
        schema_version: SCHEMA_VERSION,
        payload,
    };
    serde_json::to_vec(&line).unwrap_or_default()
}

/// Whether `value` holds an explicit `null`: an optional key is absent,
/// never `null`.
// exempted: every explicit null this scan rejects is rejected again in
// `start_args` below: `workspace`, `model`, `overrides`, `worktree` and
// `continues` take no null through their typed checks, and `content` must parse as
// `Vec<SentPart>`, whose fields are all required strings. Forcing this
// scan to false changes no answer `start` gives.
#[cfg_attr(false, mutants::skip)]
fn contains_null(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(items) => items.iter().any(contains_null),
        Value::Object(map) => map.values().any(contains_null),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

pub(crate) fn spawn(
    hub: &Hub,
    name: &str,
    run: impl FnOnce() + Send + 'static,
) -> std::io::Result<()> {
    #[cfg(not(test))]
    let _ = hub;
    #[cfg(test)]
    if lock(&hub.fail_spawn).as_deref() == Some(name) {
        return Err(std::io::Error::other(format!(
            "the test refused to spawn {name}"
        )));
    }
    std::thread::Builder::new()
        .name(name.into())
        .spawn(run)
        .map(|_| ())
}

pub(crate) fn lock<'a, T>(mutex: &'a Mutex<T>) -> MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
#[path = "connection_tests.rs"]
mod tests;
