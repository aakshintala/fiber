//! One session process's door side (`docs/invocation.md`, "Lifecycle" and
//! "Processes"): its socket at `run/<session_id>` in Fiber home, its event
//! stream copied to stdout, the clients on that socket, and what is left
//! when it exits.

use std::fs;
use std::io::Write;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};

use crate::client;
use crate::socket::{bind, remove_socket};
use crate::{failure, mint};
use contract::clock::{Clock, Wake};
use contract::diag::Timing;
use contract::emit::Emit;
use contract::events::{
    Clients, CommandInfo, CommandResult, CredentialLabels, Event, McpServerRow, QuotaInfo,
    SkillInfo, ToolInfo, ToolState,
};
use contract::inbox::{Ack, Delivery, Message};
use contract::shapes::{ContentPart, Failure, Origin, Sender as CommandSender};
use contract::steering::Queue;
use contract::tool::Tool;
use contract::{CommandId, ErrorCode, SessionId};
use log::{Injector, Log, Watcher};
use support::lock;

mod accept;
mod accepted;
pub(crate) mod admit;
mod clients;
mod conns;
mod event;
mod shells;
use accept::accept_loop;
use accepted::Accepted;
use clients::Clients as ClientCounts;
use conns::Conns;
#[cfg(test)]
use conns::GRACE;
pub(crate) use event::envelope;
use shells::Shells;
pub(crate) use shells::Stopped;

/// A running session process's door side: its socket, the clients on it, and
/// the thread copying its events to stdout.
pub struct Session {
    dir: PathBuf,
    socket: PathBuf,
    printer: JoinHandle<()>,
    printer_stop: Injector,
    gate: Arc<Gate>,
    listener: Mutex<Option<UnixListener>>,
    accept: Mutex<Option<JoinHandle<()>>>,
}

/// The answers and extension handles installed together by a reload.
pub struct Replacement {
    /// The complete command listing.
    pub commands: Vec<CommandInfo>,
    /// The complete skill discovery listing.
    pub skills: Vec<SkillInfo>,
    /// The extension command and interaction endpoint.
    pub door: Option<Arc<dyn contract::extension::ExtensionDoor>>,
    /// The session's replacement hooks.
    pub hooks: Option<Arc<dyn contract::hook::Hooks>>,
}

type CredentialsRead = Arc<dyn Fn() -> Option<CredentialLabels> + Send + Sync>;
type AdmittedRead = Arc<dyn Fn() -> String + Send + Sync>;
type QuotaRead = Arc<dyn Fn(bool) -> (Vec<QuotaInfo>, f64) + Send + Sync>;
/// Reads the current MCP server rows for the `mcp_servers` answer from
/// the tools answer's rows: installed by the session process, which owns
/// the configuration, the servers and the log the rows fold.
pub type ServersRefresh =
    Arc<dyn Fn(Vec<ToolInfo>) -> Result<Vec<McpServerRow>, Failure> + Send + Sync>;
/// Runs one server's login to completion: installed by the session
/// process alongside the in-flight map the entry guards on. The door
/// runs it on its own thread, so a waiting login never holds a
/// connection reader.
pub type McpLogin =
    Arc<dyn Fn(&str, Arc<dyn Fn(Delivery) + Send + Sync>) -> Result<(), Failure> + Send + Sync>;
const DEFAULT_QUOTA_NOTICE_AT: f64 = 80.0;

/// What every connection thread shares. The session holds it strongly; the
/// log is weak, so [`Session::close`] is what releases the lock.
pub(crate) struct Gate {
    pub(crate) log: Weak<Log>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) session_id: SessionId,
    /// What the `tools` command answers with; a `model` switch changes it
    /// through [`Session::declarer`].
    pub(crate) tools: Mutex<Vec<ToolInfo>>,
    /// The bytes-to-tokens rate the `tools` answer converts sizes with:
    /// seeded from the events the log replayed, then folded from each event
    /// the log offers before its `append` returns.
    pub(crate) rate: Arc<Mutex<log::RateFold>>,
    /// Keeps the fold's subscription on the log registered.
    _feed: log::Subscription,
    /// What the `commands` command answers with, set by
    /// [`Session::commands`]; empty until then.
    commands: Mutex<Vec<CommandInfo>>,
    /// What the `skills` command answers with, set by [`Session::skills`];
    /// empty until then.
    skills: Mutex<Vec<SkillInfo>>,
    quota: OnceLock<QuotaRead>,
    credentials: OnceLock<(CredentialsRead, AdmittedRead)>,
    /// The installed server-rows reader and login runner for the MCP
    /// driver commands (`docs/mcp.md`, "Servers, their state and the
    /// per-server commands"). With none installed, both commands are
    /// unknown, as `commands` is empty until set.
    servers_refresh: OnceLock<ServersRefresh>,
    mcp_login: OnceLock<McpLogin>,
    pub(crate) accepted: Accepted,
    /// The loop's inbox (`docs/architecture.md`, "One inbox"): unbounded;
    /// each entry is one admitted command, a job's end or a hook's delivery,
    /// and the loop drains it at step boundaries.
    inbox: Mutex<Option<Sender<Delivery>>>,
    /// The steering queue every connection thread shares
    /// (`docs/architecture.md`, "One inbox"): a reader admits a `steer`
    /// into it at once and answers `command_accepted`, and the loop
    /// reconciles each delivery against it at its next drain.
    pub(crate) steering: Arc<Queue>,
    /// What the `cancel` command asks: whether a turn is running. Stored
    /// by [`Session::run`], so a missing closure is no turn.
    cancel: OnceLock<Arc<dyn Fn() -> bool + Send + Sync>>,
    /// What a `close` with `now` starts (`docs/invocation.md`, "Shutdown"),
    /// wired by the session process. Unset, `now` is an ordinary close.
    close_now: OnceLock<Arc<dyn Fn() + Send + Sync>>,
    /// The tool a driver `shell` runs. None leaves `shell` unknown.
    driver_shell: OnceLock<Arc<dyn Tool>>,
    /// The workspace a driver `propose_notes` gathers evidence under.
    /// None leaves `propose_notes` unknown.
    notes_workspace: OnceLock<PathBuf>,
    /// The session's jobs, which `job_stop` and `background` reach. None
    /// leaves both with nothing to act on.
    jobs: OnceLock<Arc<dyn contract::jobs::Jobs>>,
    repository: OnceLock<Arc<dyn contract::repository::RepositoryCode>>,
    /// The session's hooks, which receive the loop's inbox so an extension's
    /// program run can be logged as `extension_exec`.
    hooks: Mutex<Option<Arc<dyn contract::hook::Hooks>>>,
    /// The session's extensions as the door reaches them, for the `command`
    /// driver command.
    door: Mutex<Option<Arc<dyn contract::extension::ExtensionDoor>>>,
    /// The image child's driver, which pasted images are processed through.
    /// None leaves image parts rejected: this Fiber processes no images yet.
    images: OnceLock<Arc<dyn contract::images::Images>>,
    /// The cancel a pasted image in flight sees: one per session, cancelled
    /// by shutdown alongside the driver shells, and never by `cancel`.
    pub(crate) pasting: Arc<support::cancel::Token>,
    /// The project's `history.jsonl`, set by [`Session::serve`] alone, so
    /// `fiber ask` and a bare [`Session::run`] append no prompt.
    pub(crate) history: OnceLock<Option<PathBuf>>,
    /// The prompts waiting for their acknowledgement, by command id: the
    /// loop's `on_rewrite` replaces a replaced prompt's content before it
    /// is accepted (`docs/state.md`, "Prompt history").
    pub(crate) prompts: crate::prompt_history::Pending,
    /// Fiber home: what the sender-name lookup reads (`docs/tools.md`,
    /// "Messaging other sessions").
    pub(crate) home: PathBuf,
    /// The session's directory: what `propose_notes` reads its project
    /// key from, copied before its thread starts.
    pub(crate) dir: PathBuf,
    /// The admission the door shares with the loop: once it closes, no
    /// session message enters the inbox (`docs/tools.md`, "Messaging
    /// other sessions").
    pub(crate) admission: Arc<support::admission::Admission>,
    /// Driver shells running now, and whether shutdown has begun, which
    /// cancels a new one as it registers.
    pub(crate) shells: Shells,
    /// Whether shutdown has begun: set when [`Gate::cancel_shells`] seals
    /// the shells, which every `close` path does before waiting for them.
    /// A setter that must stop at `close`'s start reads this, not the
    /// connections' stopped state, which is marked after the wait.
    pub(crate) sealed: AtomicBool,
    /// What a test observes, or holds, at a [`tests::Probe`] point.
    #[cfg(test)]
    pub(crate) probe: Mutex<Option<tests::Prober>>,
    clients: ClientCounts,
    one_turn: AtomicBool,
    /// The live connections, their writers, and whether the session stopped.
    pub(crate) conns: Conns,
}

/// Replaces (`Some`) or removes (`None`) the `tools` answer's entry for a
/// name: [`Session::declarer`].
pub type Declare = Arc<dyn Fn(&str, Option<ToolInfo>) + Send + Sync>;

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
        Self::open_with_timing(home, dir, log, clock, tools, out, None)
    }

    /// Opens the door side of a new session with the session's timer:
    /// as [`Session::open`], timing socket preparation as
    /// `startup.socket` until the bound listener is ready. The only
    /// production caller is the session command; helpers keep the
    /// untimed [`Session::open`].
    pub fn open_with_timing(
        home: &Path,
        dir: &Path,
        log: &Arc<Log>,
        clock: Arc<dyn Clock>,
        tools: Vec<ToolInfo>,
        out: Box<dyn Write + Send>,
        timing: Option<Timing>,
    ) -> Result<Self, Failure> {
        let opened = open_in(home, dir, log, clock, tools, out, timing);
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
        open_in(home, dir, log, clock, tools, out, None)
    }

    /// Opens the door side of a resumed session with the session's
    /// timer: as [`Session::resume`], timing socket preparation as
    /// `startup.socket` until the bound listener is ready. The only
    /// production caller is the resume command; helpers keep the untimed
    /// [`Session::resume`].
    pub fn resume_with_timing(
        home: &Path,
        dir: &Path,
        log: &Arc<Log>,
        clock: Arc<dyn Clock>,
        tools: Vec<ToolInfo>,
        out: Box<dyn Write + Send>,
        timing: Option<Timing>,
    ) -> Result<Self, Failure> {
        open_in(home, dir, log, clock, tools, out, timing)
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
        // A `Close` in `first` closes admission before anything is sent, so
        // every later `message` is refused `closing`: `fiber ask` queues
        // `Prompt` then `Close` (`docs/tools.md`, "Messaging other sessions").
        if first
            .iter()
            .any(|delivery| matches!(delivery, Delivery::Close(_)))
        {
            self.gate.mark_closed();
        }
        for delivery in first {
            // The receiver is still here, so the send cannot fail.
            match inbox.send(delivery) {
                Ok(()) => {}
                Err(mpsc::SendError(delivery)) => drop(delivery),
            }
        }
        // A job's end wakes the loop through the same inbox
        // (`docs/tools.md`, "Background jobs").
        if let Some(jobs) = self.gate.jobs.get() {
            jobs.deliver_to(inbox.clone());
        }
        let hooks = lock(&self.gate.hooks).clone();
        if let Some(hooks) = hooks {
            hooks.deliver_to(inbox.clone());
        }
        *lock(&self.gate.inbox) = Some(inbox);
        let stored = self.gate.cancel.set(cancel);
        debug_assert!(stored.is_ok(), "cancel is wired once");
        self.start_accept()?;
        run(waiting)
    }

    /// Whether a person can answer now: a full connection is attached, except
    /// in `fiber ask`. Holds the gate weakly; a gate that is gone reads false.
    pub fn attended(&self) -> Arc<dyn Fn() -> bool + Send + Sync> {
        let gate = Arc::downgrade(&self.gate);
        Arc::new(move || {
            gate.upgrade().is_some_and(|gate| {
                !gate.one_turn.load(Ordering::SeqCst) && gate.clients.count() > 0
            })
        })
    }

    /// Runs `fiber ask`'s one turn: sends `prompt` then `close`, so the
    /// loop finishes that turn and exits. A client attached to the socket
    /// neither keeps the session alive nor starts a second turn. When the
    /// loop rejects the first prompt, that rejection is the run's failure
    /// (`docs/invocation.md`, "What each command does"); `run`'s own
    /// failure wins.
    pub fn ask(
        &self,
        prompt: String,
        cancel: Arc<dyn Fn() -> bool + Send + Sync>,
        run: impl FnOnce(Receiver<Delivery>) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        self.gate.one_turn.store(true, Ordering::SeqCst);
        let (done, rejection) = mpsc::channel();
        let ack = Ack(Box::new(move |answer| {
            if let Err(rejection) = answer {
                done.send(rejection).unwrap_or(());
            }
        }));
        let outcome = self.run(
            vec![
                Delivery::Prompt(prompt_message(prompt), ack),
                Delivery::Close(ignore()),
            ],
            cancel,
            run,
        );
        match outcome {
            Err(failure) => Err(failure),
            Ok(()) => match rejection.try_recv() {
                Ok(rejection) => Err(Failure {
                    code: rejection.code,
                    message: rejection.message,
                    retry_after_ms: None,
                    provider: None,
                }),
                Err(_) => Ok(()),
            },
        }
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
        let stored = self.gate.driver_shell.set(tool);
        debug_assert!(stored.is_ok(), "driver_shell is wired once");
    }

    /// The workspace a driver `propose_notes` gathers evidence under
    /// (`docs/invocation.md`, "What each command does"). With none set,
    /// `propose_notes` stays an unknown command.
    pub fn notes_workspace(&self, workspace: PathBuf) {
        let stored = self.gate.notes_workspace.set(workspace);
        debug_assert!(stored.is_ok(), "notes_workspace is wired once");
    }

    /// Replaces the session's listings and extension handles on reload.
    pub fn reloader(&self) -> Arc<dyn Fn(Replacement) + Send + Sync> {
        let gate = Arc::downgrade(&self.gate);
        Arc::new(move |replacement| {
            let Some(gate) = gate.upgrade() else {
                return;
            };
            *lock(&gate.commands) = replacement.commands;
            *lock(&gate.skills) = replacement.skills;
            *lock(&gate.door) = replacement.door;
            *lock(&gate.hooks) = replacement.hooks.clone();
            let inbox = lock(&gate.inbox).clone();
            if let (Some(hooks), Some(inbox)) = (replacement.hooks, inbox) {
                hooks.deliver_to(inbox);
            }
        })
    }

    /// Every `/name` the session runs, which the `commands` driver command
    /// answers with verbatim (`docs/invocation.md`, "What each command
    /// does"). Set before [`Session::run`]; with none set, the answer is an
    /// empty list.
    pub fn commands(&self, commands: Vec<CommandInfo>) {
        *lock(&self.gate.commands) = commands;
    }

    /// Every skill discovery read, switched-off and shadowed ones included
    /// and marked, which the `skills` driver command answers with
    /// verbatim (`docs/invocation.md`, "What each command does"). Set
    /// before [`Session::run`]; with none set, the answer is an empty
    /// list.
    pub fn skills(&self, skills: Vec<SkillInfo>) {
        *lock(&self.gate.skills) = skills;
    }

    /// Wires the non-waiting cached quota reader before the session runs.
    pub fn quota(&self, read: QuotaRead) {
        let stored = self.gate.quota.set(read);
        debug_assert!(stored.is_ok(), "quota is wired once");
    }

    /// Wires the server-rows reader for the `mcp_servers` answer before
    /// the session runs.
    pub fn mcp_servers(&self, refresh: ServersRefresh) {
        let stored = self.gate.servers_refresh.set(refresh);
        debug_assert!(stored.is_ok(), "mcp_servers is wired once");
    }

    /// Wires one server's login runner for the `mcp_login` command before
    /// the session runs.
    pub fn mcp_login(&self, login: McpLogin) {
        let stored = self.gate.mcp_login.set(login);
        debug_assert!(stored.is_ok(), "mcp_login is wired once");
    }

    /// Wires the credential labels reader and the admitted-provider reader
    /// before the session runs. The latter is called when the loop answers.
    pub fn credentials(&self, labels: CredentialsRead, admitted: AdmittedRead) {
        let stored = self.gate.credentials.set((labels, admitted));
        debug_assert!(stored.is_ok(), "credentials are wired once");
    }

    /// The session's jobs, which the `job_stop` and `background` driver
    /// commands act on (`docs/invocation.md`, "Driver commands"). With none
    /// set, both are rejected `stale_request`: no job or call is running.
    pub fn jobs(&self, jobs: Arc<dyn contract::jobs::Jobs>) {
        let stored = self.gate.jobs.set(jobs);
        debug_assert!(stored.is_ok(), "jobs is wired once");
    }

    /// The repository's code, which `offered_file` reads a listed file
    /// through. With none set, it is rejected `stale_request`.
    pub fn repository_code(&self, code: Arc<dyn contract::repository::RepositoryCode>) {
        let stored = self.gate.repository.set(code);
        debug_assert!(stored.is_ok(), "repository code is wired once");
    }

    /// The session's hooks, which receive the loop's inbox beside the jobs.
    pub fn hooks(&self, hooks: Arc<dyn contract::hook::Hooks>) {
        *lock(&self.gate.hooks) = Some(hooks);
    }

    /// The session's extensions as the `command` driver command reaches them.
    pub fn extensions(&self, door: Arc<dyn contract::extension::ExtensionDoor>) {
        *lock(&self.gate.door) = Some(door);
    }

    /// The in-process driver `host.drive` sends through (`docs/extensions.md`,
    /// "Host calls"). It holds the gate weakly, so the extensions never keep
    /// the door alive after [`Session::close`].
    pub fn driver(&self) -> Arc<dyn contract::extension::Drive> {
        Arc::new(crate::drive::Driver::new(Arc::downgrade(&self.gate)))
    }

    /// The image child's driver, which a pasted image is processed through
    /// (`docs/architecture.md`, "The call rules"). With none set, an image
    /// part is rejected: this Fiber processes no images yet.
    pub fn images(&self, images: Arc<dyn contract::images::Images>) {
        let stored = self.gate.images.set(images);
        debug_assert!(stored.is_ok(), "images is wired once");
    }

    /// What changes one entry of the `tools` answer when a `model` switch
    /// applies (`docs/tools.md`, "Seeing the tools"): `Some` replaces the
    /// entry named `name`, or adds it before the first entry whose name sorts
    /// after it; `None` removes it. It holds the gate weakly and does nothing
    /// once [`Session::close`] has begun.
    pub fn declarer(&self) -> Declare {
        let gate = Arc::downgrade(&self.gate);
        Arc::new(move |name: &str, info: Option<ToolInfo>| {
            let Some(gate) = gate.upgrade() else {
                return;
            };
            if gate.conns.stopped() {
                return;
            }
            let mut tools = lock(&gate.tools);
            match (
                tools.binary_search_by(|tool| tool.name.as_str().cmp(name)),
                info,
            ) {
                (Ok(at), Some(info)) => {
                    if let Some(entry) = tools.get_mut(at) {
                        *entry = info;
                    }
                }
                (Ok(at), None) => {
                    tools.remove(at);
                }
                (Err(at), Some(info)) => {
                    tools.insert(at, info);
                }
                (Err(_), None) => {}
            }
        })
    }

    /// What sets one entry of the `tools` answer's state when a preamble
    /// build defers or loads a tool (`docs/tools.md`, "Seeing the tools"):
    /// only the named row's `state` changes; `bytes`, `tokens`, `source`
    /// and order are unchanged. An unknown name changes nothing and adds
    /// no row. It holds the gate weakly and does nothing once
    /// [`Session::close`] has begun.
    #[allow(
        clippy::type_complexity,
        reason = "no new export: the setter returns the closure type itself"
    )]
    pub fn state_setter(&self) -> Arc<dyn Fn(&str, ToolState) + Send + Sync> {
        let gate = Arc::downgrade(&self.gate);
        Arc::new(move |name: &str, state: ToolState| {
            let Some(gate) = gate.upgrade() else {
                return;
            };
            if gate.sealed.load(Ordering::Relaxed) || gate.conns.stopped() {
                return;
            }
            let mut tools = lock(&gate.tools);
            if let Ok(at) = tools.binary_search_by(|tool| tool.name.as_str().cmp(name))
                && let Some(entry) = tools.get_mut(at)
            {
                entry.state = state;
            }
        })
    }

    /// What `close` with `now` starts; unset, `now` is an ordinary close.
    pub fn close_now(&self, start: Arc<dyn Fn() + Send + Sync>) {
        let stored = self.gate.close_now.set(start);
        debug_assert!(stored.is_ok(), "close_now is wired once");
    }

    /// What a shutdown calls to stop the door side's work
    /// (`docs/invocation.md`, "Shutdown"): every driver shell is cancelled,
    /// a later one is cancelled as it starts, and the loop is woken. Once
    /// the inbox is gone it wakes nothing.
    pub fn stopper(&self) -> Arc<dyn Fn() + Send + Sync> {
        let gate = Arc::clone(&self.gate);
        Arc::new(move || {
            // No session message enters once a shutdown starts.
            gate.mark_closed();
            gate.cancel_shells();
            gate.deliver(Delivery::Cancelled);
        })
    }

    /// Reports a replaced prompt's replacement, to record in the prompt
    /// history before the prompt's acceptance (`docs/state.md`, "Prompt
    /// history"): what the loop's `on_rewrite` calls with the command id
    /// and the replacement as the hook returned it. It holds the gate
    /// weakly and does nothing once [`Session::close`] has begun.
    #[allow(
        clippy::type_complexity,
        reason = "the loop's `on_rewrite` takes the same shape; naming it twice keeps both readable"
    )]
    pub fn rewrites(&self) -> Arc<dyn Fn(&CommandId, &[ContentPart]) + Send + Sync> {
        let gate = Arc::downgrade(&self.gate);
        Arc::new(move |id, content| {
            let Some(gate) = gate.upgrade() else {
                return;
            };
            if gate.conns.stopped() {
                return;
            }
            gate.prompts.replace(id, content.to_vec());
        })
    }

    /// The wake a running tool call's interaction reaches the loop through
    /// (`docs/architecture.md`, "One inbox"): each wake puts
    /// `Delivery::Cancelled` in the inbox. It holds the gate weakly, so the
    /// loop never keeps its own inbox open, and after [`Session::close`] it
    /// wakes nothing.
    pub fn inbox_wake(&self) -> Arc<dyn Wake> {
        Arc::new(InboxWake(Arc::downgrade(&self.gate)))
    }

    /// The admission the door shares with the loop: the loop's idle exit
    /// closes it, and the door refuses `message` once it is closed
    /// (`docs/tools.md`, "Messaging other sessions").
    pub fn admission(&self) -> Arc<support::admission::Admission> {
        Arc::clone(&self.gate.admission)
    }

    /// The steering queue the door shares with the loop: a reader admits
    /// into it at once, and the loop reconciles each delivery against it
    /// at the drain (`docs/architecture.md`, "One inbox").
    pub fn steering(&self) -> Arc<Queue> {
        Arc::clone(&self.gate.steering)
    }

    /// The session's own id: its directory's name, which also names its
    /// socket in `run/`.
    pub fn id(&self) -> &SessionId {
        &self.gate.session_id
    }

    /// Makes `fiber_exited` the last line this process writes: no later
    /// `clients` line is emitted, no new driver shell runs, and every
    /// running one is cancelled and waited for. Called just before
    /// `fiber_exited`. Idempotent; [`Session::close`] behaves as before.
    pub fn quiesce(&self) {
        // No session message enters once the session quiets.
        self.gate.mark_closed();
        // An emission holds this lock, so one in flight finishes first.
        self.gate.clients.seal();
        if let Some(door) = self.gate.door() {
            door.seal();
        }
        self.gate.cancel_shells();
        self.gate.wait_shells();
    }

    /// Ends the door side: marks the gate stopped, unlinks the socket,
    /// drops `log`
    /// (the last handle, which releases the lock), waits up to [`conns::GRACE`] for
    /// each writer, then shuts down whatever is still open. Every driver
    /// shell is cancelled first, since shutting its socket does not stop the
    /// tool, and waited for: its thread is not joined, and its answer is
    /// queued before it ends. The accept thread is detached, never joined
    /// and never woken: a wake would take a blocking `UnixStream::connect`,
    /// which on Linux blocks forever when a replacement listener's backlog
    /// is full, and when the path is gone, or was rebound by another
    /// listener, nothing can wake this session's `accept`, so a join would
    /// block forever; process exit ends the thread. A connection the loop
    /// admits after the stop is rejected, not served: the stopped check and
    /// the reader's publication share the connection lock, so a late
    /// reader's stream is shut down and its thread ends instead of leaking.
    /// A reader can still admit one until it is
    /// joined; that shell starts cancelled and is waited for once no reader
    /// is left, though its answer may reach no writer.
    pub fn close(self, log: Arc<Log>) {
        self.gate.cancel_shells();
        self.gate.wait_shells();
        #[cfg(test)]
        self.gate.note(tests::Probe::FirstShellWaitDone);
        self.gate.conns.mark_stopped();
        // The inbox sender is dropped here, so a kept receiver sees
        // `Disconnected`: the detached accept thread keeps its own `Arc`
        // until process exit and can no longer be relied on to release it.
        drop(lock(&self.gate.inbox).take());
        // Detached, never joined and never woken (see above): process exit
        // ends the thread.
        let _accept = lock(&self.accept).take();
        drop(lock(&self.listener).take());
        remove_socket(&self.socket);
        if !prompted(&self.dir) {
            fs::remove_dir_all(&self.dir).unwrap_or(());
        }
        drop(log);
        self.gate.conns.wait_writers(self.gate.clock.as_ref());
        self.gate.conns.join_clients();
        self.gate.wait_shells();
        // The printer's watcher returns only on a `client::STOP` line or
        // the log's end: with nothing written and another `Arc<Log>` still
        // held, neither comes, so stop it directly. Lines already queued
        // still print first, so a normal close still prints `fiber_exited`.
        self.printer_stop
            .push_kept(client::control_line(&self.gate.session_id, client::STOP, 0));
        #[cfg(test)]
        self.gate.note(tests::Probe::PrinterStopQueued);
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
    /// Stops a running turn and every driver shell.
    pub(crate) fn stop_running(&self) -> Stopped {
        let turn = self.cancel.get().is_some_and(|cancel| cancel());
        let shell = self.shells.stop_all();
        Stopped { turn, shell }
    }

    /// Marks the gate so a shell that registers later is cancelled at once,
    /// and cancels the shells already running. A pasted image in flight is
    /// cancelled too: the session started its child, so shutdown stops it
    /// inside the bound. Closing the socket does not stop a tool blocked
    /// in `run`.
    pub(super) fn cancel_shells(&self) {
        self.sealed.store(true, Ordering::Relaxed);
        self.shells.seal_and_cancel();
        self.pasting.cancel();
    }

    /// Waits until no driver shell is registered: each has answered.
    pub(super) fn wait_shells(&self) {
        #[cfg(test)]
        self.shells.wait(&|| self.note(tests::Probe::ShellsWaiting));
        #[cfg(not(test))]
        self.shells.wait(&|| {});
    }

    #[cfg(test)]
    pub(crate) fn note(&self, point: tests::Probe) {
        let probe = lock(&self.probe).clone();
        if let Some(probe) = probe {
            probe(point);
        }
    }

    /// One more `full` connection, and the `clients` line for it.
    pub(crate) fn attach(&self) {
        self.clients.attach(|count| self.emit_clients(count));
    }

    /// One fewer `full` connection, and the `clients` line for it.
    pub(crate) fn detach(&self) {
        self.clients.detach(|count| self.emit_clients(count));
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

    pub(crate) fn quota(&self, fetch: bool) -> (Vec<QuotaInfo>, f64) {
        self.quota
            .get()
            .map_or_else(|| (Vec::new(), DEFAULT_QUOTA_NOTICE_AT), |read| read(fetch))
    }

    /// The installed server-rows reader, if one is wired.
    pub(crate) fn servers_refresh(&self) -> Option<ServersRefresh> {
        self.servers_refresh.get().cloned()
    }

    /// The installed login runner, if one is wired.
    pub(crate) fn mcp_login(&self) -> Option<McpLogin> {
        self.mcp_login.get().cloned()
    }

    pub(crate) fn credentials_reader(&self) -> Option<CredentialsRead> {
        self.credentials.get().map(|(labels, _)| Arc::clone(labels))
    }

    pub(crate) fn with_provider(&self, ack: Ack) -> Ack {
        let Some((_, admitted)) = self.credentials.get() else {
            return ack;
        };
        let admitted = Arc::clone(admitted);
        Ack(Box::new(move |answer| {
            ack.0(match answer {
                Ok(None) => Ok(Some(CommandResult::CredentialSwitch {
                    provider: admitted(),
                })),
                other => other,
            });
        }))
    }

    pub(crate) fn skills(&self) -> Vec<SkillInfo> {
        lock(&self.skills).clone()
    }

    pub(crate) fn door(&self) -> Option<Arc<dyn contract::extension::ExtensionDoor>> {
        lock(&self.door).clone()
    }

    pub(crate) fn repository(&self) -> Option<Arc<dyn contract::repository::RepositoryCode>> {
        self.repository.get().cloned()
    }

    pub(crate) fn jobs(&self) -> Option<Arc<dyn contract::jobs::Jobs>> {
        self.jobs.get().cloned()
    }

    pub(crate) fn driver_shell(&self) -> Option<Arc<dyn Tool>> {
        self.driver_shell.get().cloned()
    }

    pub(crate) fn notes_workspace(&self) -> Option<PathBuf> {
        self.notes_workspace.get().cloned()
    }

    /// What a `close` with `now` starts, cloned out of the slot so the
    /// shutdown runs with no gate lock held.
    pub(crate) fn close_now(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        self.close_now.get().cloned()
    }

    /// The image child's driver, cloned out of the slot so no child run
    /// holds it.
    pub(crate) fn images(&self) -> Option<Arc<dyn contract::images::Images>> {
        self.images.get().cloned()
    }

    pub(crate) fn deliver(&self, delivery: Delivery) {
        // The ack's answer runs host code `doors` does not own, so no
        // `doors` lock is held across it: under the guard only the slot
        // read and the one send run, and the undelivered delivery drops
        // after the guard releases.
        let undelivered = {
            let inbox = lock(&self.inbox);
            if let Some(sender) = inbox.as_ref() {
                match sender.send(delivery) {
                    Ok(()) => None,
                    Err(mpsc::SendError(delivery)) => Some(delivery),
                }
            } else {
                Some(delivery)
            }
        };
        drop(undelivered);
    }

    /// Hands `delivery` to the loop when it is listening: false when the
    /// inbox is gone or closed, so a steer's admission answers `closing`
    /// instead of queueing what no drain will ever reconcile.
    pub(crate) fn try_deliver(&self, delivery: Delivery) -> bool {
        let inbox = lock(&self.inbox);
        let Some(inbox) = inbox.as_ref() else {
            return false;
        };
        inbox.send(delivery).is_ok()
    }
}

/// [`Session::inbox_wake`]'s wake.
struct InboxWake(Weak<Gate>);

impl Wake for InboxWake {
    fn wake(&self) {
        if let Some(gate) = self.0.upgrade() {
            gate.deliver(Delivery::Cancelled);
        }
    }
}

impl Wake for Gate {
    fn wake(&self) {
        self.conns.notify();
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
    timing: Option<Timing>,
) -> Result<Session, Failure> {
    // The stamp comes from the session clock before binding, so the
    // helper takes no start read of its own; without a callable the
    // bind costs no clock read. The guard drops at binding readiness,
    // before the printer starts: a failure measures through the
    // failure instead.
    let started = timing.as_ref().map(|_| clock.now());
    let timer = timing
        .as_ref()
        .and_then(|timing| timing(contract::diag::Op::StartupSocket, None, started));
    let (socket, listener) = bind(home, dir)?;
    drop(timer);
    let session_id = SessionId(
        dir.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
    );
    let printer_watcher = log.watch();
    let rate = Arc::new(Mutex::new(log.replayed_rate()));
    let feed = {
        let rate = Arc::clone(&rate);
        log.subscribe(Arc::new(move |_, event| lock(&rate).fold(event)))
    };
    let gate = Arc::new(Gate {
        log: Arc::downgrade(log),
        clock: Arc::clone(&clock),
        session_id,
        tools: Mutex::new(tools),
        rate,
        _feed: feed,
        commands: Mutex::new(Vec::new()),
        skills: Mutex::new(Vec::new()),
        quota: OnceLock::new(),
        credentials: OnceLock::new(),
        servers_refresh: OnceLock::new(),
        mcp_login: OnceLock::new(),
        accepted: Accepted::new(),
        inbox: Mutex::new(None),
        steering: Arc::new(Queue::new()),
        cancel: OnceLock::new(),
        close_now: OnceLock::new(),
        driver_shell: OnceLock::new(),
        notes_workspace: OnceLock::new(),
        jobs: OnceLock::new(),
        repository: OnceLock::new(),
        hooks: Mutex::new(None),
        door: Mutex::new(None),
        images: OnceLock::new(),
        pasting: Arc::new(support::cancel::Token::new()),
        history: OnceLock::new(),
        prompts: crate::prompt_history::Pending::new(),
        home: home.to_owned(),
        dir: dir.to_owned(),
        admission: Arc::new(support::admission::Admission::default()),
        shells: Shells::new(),
        sealed: AtomicBool::new(false),
        #[cfg(test)]
        probe: Mutex::new(None),
        clients: ClientCounts::new(),
        one_turn: AtomicBool::new(false),
        conns: Conns::new(),
    });
    // The session's `Arc<Gate>` keeps this subscription alive: the weak
    // handle upgrades for as long as that allocation lives.
    let wake = Arc::clone(&gate);
    let wake: Arc<dyn Wake> = wake;
    clock.subscribe(Arc::downgrade(&wake));
    let printer_stop = printer_watcher.injector();
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
        printer_stop,
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

/// Whether the stored shutdown closures skip their `shutdown`: with the
/// stoppable read wired, the stop's pipe ends a silent reader on its own.
#[cfg(test)]
pub(crate) fn shutdown_skipped_for_test() -> bool {
    tests::shutdown_skipped()
}

/// Whether the session's log shows it was ever prompted: a `turn_started`
/// or a `repository_code_offered`, read one line at a time up to the first:
/// a session that exits on its first offer is kept, so the offer is raised
/// again on resume. A session that continues another is kept once its
/// process started, even with no prompt yet: the old log's `rewound`
/// points at it (`docs/invocation.md`, "Lifecycle"). A log that cannot be
/// read, or a line that does not parse before the first, keeps the session,
/// so nothing is deleted on a guess.
fn prompted(dir: &Path) -> bool {
    log::lines(dir).map_or(true, |mut lines| {
        let mut forked = false;
        lines.any(|line| {
            line.map_or(true, |line| match Event::from_envelope(&line) {
                Ok(Some(Event::TurnStarted(_) | Event::RepositoryCodeOffered(_))) => true,
                Ok(Some(Event::SessionStarted(started))) => {
                    forked = started.forked_from.is_some();
                    false
                }
                Ok(Some(Event::FiberStarted(_))) => forked,
                Ok(_) | Err(_) => false,
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

/// Copies every event to `out`, one JSON line each, until `fiber_exited`
/// or `rewound`, or the end of the log. Durable lines serialize as the log
/// wrote them, so stdout filtered to them is `events.jsonl` byte for byte
/// (`docs/invocation.md`, "What a caller gets back"). `rewound` closes
/// the process boundary as `fiber_exited` does: nothing follows it, so the
/// copy ends there even while the log itself is still held
/// (`docs/events.md`, "Rewind").
fn print(mut watcher: Watcher, mut out: Box<dyn Write + Send>) {
    while let Ok(Some(line)) = watcher.recv() {
        if line.kind == client::STOP {
            drain(&mut watcher, out.as_mut());
            return;
        }
        if write_until_stop(out.as_mut(), &line) {
            return;
        }
    }
}

/// Writes `line`, reporting whether printing stops after it: a failed
/// write, `fiber_exited` or `rewound` (`docs/invocation.md`, "What a
/// caller gets back"). A reader that went away, or a line that cannot be
/// written, stops the copy, never the session.
fn write_until_stop(out: &mut dyn Write, line: &contract::Envelope) -> bool {
    if client::write_line(out, line).is_err() {
        return true;
    }
    line.kind == "fiber_exited" || line.kind == "rewound"
}

/// Prints what `print` has not yet reached once its `STOP` arrived: the
/// durable lines the queue dropped, re-read from the log without waiting,
/// until none is available or the copy stops as `print` stops it. The
/// `STOP` is kept ahead of the catch-up, so returning on it would end
/// printing before those lines are recovered, including `fiber_exited`.
fn drain(watcher: &mut Watcher, out: &mut dyn Write) {
    while let Ok(Some(line)) = watcher.try_recv() {
        if line.kind == client::STOP {
            continue;
        }
        if write_until_stop(out, &line) {
            return;
        }
    }
}

#[cfg(test)]
#[path = "session_tests.rs"]
pub(crate) mod tests;
